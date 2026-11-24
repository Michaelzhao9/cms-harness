//! Record queries: one SQL builder serves both the live (published) and the draft view.
//! Field expressions are exactly `json_extract(<column>,'$.<field>')` so that expression indexes can match them.

use crate::changes;
use crate::schema::{normalize, project, valid_name, CollectionDef, FieldDef, FieldType};
use rusqlite::{params_from_iter, types::Value as Sql, Connection};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op { Eq, Ne, Gt, Gte, Lt, Lte, Contains }

/// A record's state in the draft view, as `changes::status` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Status { Draft, Changed, Deleting, Published }

impl Status {
    /// The same rule as `changes::status`, as SQL over the row's own columns.
    fn sql(self) -> &'static str {
        match self {
            Status::Draft => "(published IS NULL AND draft IS NOT NULL)",
            Status::Changed => "(published IS NOT NULL AND draft IS NOT NULL AND draft_deleted = 0)",
            Status::Deleting => "(published IS NOT NULL AND draft_deleted = 1)",
            Status::Published => "(published IS NOT NULL AND draft IS NULL AND draft_deleted = 0)",
        }
    }
}

pub struct Query {
    pub filters: Vec<(String, Op, Value)>,
    /// Keep only records in one of these states; empty keeps all.
    pub status: Vec<Status>,
    /// (field, descending)
    pub sort: Option<(String, bool)>,
    pub limit: i64,
    pub offset: i64,
    /// Only records this member owns (access rule `owner`).
    pub owner: Option<i64>,
}

pub struct Row {
    pub id: i64,
    pub status: &'static str,
    pub doc: Value,
}

/// A JSON scalar as an SQL parameter; `None` for null, arrays and objects.
pub fn to_sql(v: &Value) -> Option<Sql> {
    match v {
        Value::String(s) => Some(Sql::Text(s.clone())),
        Value::Bool(b) => Some(Sql::Integer(*b as i64)),
        Value::Number(n) => Some(n.as_i64().map_or_else(|| Sql::Real(n.as_f64().unwrap_or(0.0)), Sql::Integer)),
        _ => None,
    }
}

/// `id`, or a declared field that has `index` or `unique`.
fn check_field(def: &CollectionDef, name: &str) -> Result<(), String> {
    if name == "id" {
        return Ok(());
    }
    let f = def.field(name).ok_or_else(|| format!("集合“{}”没有字段“{name}”：请检查字段名。", def.name))?;
    if f.index || f.unique {
        Ok(())
    } else {
        Err(format!("字段“{name}”没有设置 index 或 unique，不能用于筛选或排序：请先在集合定义里给它加上 index。"))
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null", Value::Bool(_) => "布尔值", Value::Number(_) => "数字",
        Value::String(_) => "字符串", Value::Array(_) => "数组", Value::Object(_) => "对象",
    }
}

fn check_filter(def: &CollectionDef, field: &str, op: Op, v: &Value) -> Result<(), String> {
    check_field(def, field)?;
    let f = def.field(field);
    let (want, ok) = match f.map(|f| f.kind) {
        None | Some(FieldType::Relation) => ("整数", v.as_i64().is_some()),
        Some(FieldType::Number) => ("数字", v.is_number()),
        Some(FieldType::Boolean) => ("布尔值", v.is_boolean()),
        Some(_) => ("字符串", v.is_string()),
    };
    if !ok {
        return Err(format!(
            "筛选字段“{field}”的值 {v} 类型不对：它需要{want}，收到的是{}。",
            kind_of(v)
        ));
    }
    if let Some(f) = f.filter(|f| matches!(f.kind, FieldType::Date | FieldType::Select)) {
        // reuse the record validator on a one-field definition so dates and options are judged the same way
        let one = CollectionDef { name: def.name.clone(), title: String::new(), single: false, access: Default::default(),
            fields: vec![FieldDef { required: false, ..f.clone() }], url: None };
        normalize(&one, &Value::Null, &serde_json::json!({ field: v }))
            .map_err(|e| format!("筛选字段“{field}”的值不合法：{e}"))?;
    }
    if op == Op::Contains && f.map(|f| f.kind) != Some(FieldType::Text) {
        return Err(format!("字段“{field}”不能用 contains：它只能用于 text 字段，且值必须是字符串。"));
    }
    Ok(())
}

pub fn parse_filter(def: &CollectionDef, filter: &Value) -> Result<Vec<(String, Op, Value)>, String> {
    let obj = filter.as_object().ok_or("filter 必须是 JSON 对象：形如 {\"字段名\": 值} 或 {\"字段名\": {\"gte\": 值}}。")?;
    let mut out = vec![];
    for (field, v) in obj {
        let ops: Vec<(&str, &Value)> = match v {
            Value::Object(m) => m.iter().map(|(k, v)| (k.as_str(), v)).collect(),
            _ => vec![("eq", v)],
        };
        if ops.is_empty() {
            return Err(format!("筛选字段“{field}”没有给出任何运算符：请写成 {{\"{field}\": {{\"gte\": 值}}}} 或直接写值。"));
        }
        for (name, val) in ops {
            let op = match name {
                "eq" => Op::Eq, "ne" => Op::Ne, "gt" => Op::Gt, "gte" => Op::Gte,
                "lt" => Op::Lt, "lte" => Op::Lte, "contains" => Op::Contains,
                _ => return Err(format!("筛选字段“{field}”用了未知的运算符“{name}”：可用的有 eq、ne、gt、gte、lt、lte、contains。")),
            };
            check_filter(def, field, op, val)?;
            out.push((field.clone(), op, val.clone()));
        }
    }
    Ok(out)
}

fn expr(field: &str, col: &str) -> Result<String, String> {
    if field == "id" {
        return Ok("id".into());
    }
    if !valid_name(field) {
        return Err(format!("字段名“{field}”不合法，不能用于查询。"));
    }
    Ok(format!("json_extract({col},'$.{field}')"))
}

/// (column, WHERE clause, parameters) shared by the page query and the count query.
fn build(def: &CollectionDef, q: &Query, live: bool) -> Result<(&'static str, String, Vec<Sql>), String> {
    if !valid_name(&def.name) {
        return Err(format!("集合名“{}”不合法，不能用于查询。", def.name));
    }
    let col = if live { "published" } else { "coalesce(draft, published)" };
    let mut wh = format!("collection = '{}' AND {col} IS NOT NULL", def.name);
    let mut params = vec![];
    for (field, op, v) in &q.filters {
        check_filter(def, field, *op, v)?;
        let e = expr(field, col)?;
        let mut p = to_sql(v).expect("checked by check_filter");
        let sql = match op {
            Op::Eq => "=", Op::Ne => "!=", Op::Gt => ">", Op::Gte => ">=", Op::Lt => "<", Op::Lte => "<=",
            Op::Contains => {
                if let Sql::Text(s) = &p {
                    p = Sql::Text(format!("%{}%", s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
                }
                "LIKE"
            }
        };
        wh.push_str(&format!(" AND {e} {sql} ?{}", if *op == Op::Contains { " ESCAPE '\\'" } else { "" }));
        params.push(p);
    }
    if let Some(owner) = q.owner {
        wh.push_str(" AND owner = ?");
        params.push(Sql::Integer(owner));
    }
    if !q.status.is_empty() {
        wh.push_str(&format!(" AND ({})", q.status.iter().map(|s| s.sql()).collect::<Vec<_>>().join(" OR ")));
    }
    Ok((col, wh, params))
}

/// `records`, pinned to the first filtered (else sorted) field's index in the live view: SQLite otherwise prefers `records_collection`.
fn from(def: &CollectionDef, q: &Query, live: bool) -> String {
    let field = q.filters.iter().map(|f| &f.0).chain(q.sort.as_ref().map(|s| &s.0)).find(|f| *f != "id");
    match field {
        Some(f) if live && def.field(f).is_some() => format!("records INDEXED BY \"hc:{}:{f}\"", def.name),
        _ => "records".into(),
    }
}

/// The page query and its parameters (limit and offset last). Fails only on filters or sorts the definition does not allow.
fn page_sql(def: &CollectionDef, q: &Query, live: bool) -> Result<(String, Vec<Sql>), String> {
    let (col, wh, mut params) = build(def, q, live)?;
    let order = match &q.sort {
        None => "id DESC".to_string(),
        Some((f, desc)) => {
            check_field(def, f)?;
            let d = if *desc { "DESC" } else { "ASC" };
            format!("{} {d}, id {d}", expr(f, col)?)
        }
    };
    params.push(Sql::Integer(q.limit));
    params.push(Sql::Integer(q.offset));
    Ok((
        format!("SELECT id, published IS NOT NULL, draft IS NOT NULL, draft_deleted, {col} FROM {} WHERE {wh} ORDER BY {order} LIMIT ? OFFSET ?", from(def, q, live)),
        params,
    ))
}

/// The SQL `run` uses for the live view. Panics if the query was not checked (`parse_params` / `parse_filter` do that).
pub fn live_sql(def: &CollectionDef, q: &Query) -> (String, Vec<Sql>) {
    page_sql(def, q, true).expect("query was checked against the definition")
}

pub fn run(conn: &Connection, def: &CollectionDef, q: &Query, live: bool) -> Result<(Vec<Row>, i64), String> {
    check_page(q)?;
    let (_, wh, params) = build(def, q, live)?;
    let (sql, page_params) = page_sql(def, q, live)?;
    let err = |e: rusqlite::Error| format!("查询集合“{}”失败：{e}", def.name);
    let total: i64 = conn
        .query_row(&format!("SELECT count(*) FROM {} WHERE {wh}", from(def, q, live)), params_from_iter(&params), |r| r.get(0))
        .map_err(err)?;
    let mut stmt = conn.prepare(&sql).map_err(err)?;
    let rows = stmt
        .query_map(params_from_iter(&page_params), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?, r.get::<_, bool>(2)?, r.get::<_, bool>(3)?, r.get::<_, String>(4)?))
        })
        .map_err(err)?;
    let mut out = vec![];
    for r in rows {
        let (id, p, d, dd, doc) = r.map_err(err)?;
        let doc: Value = serde_json::from_str(&doc).map_err(|e| format!("记录 {id} 的内容已损坏：{e}"))?;
        out.push(Row { id, status: changes::status(p, d, dd), doc: project(def, &doc) });
    }
    Ok((out, total))
}

fn check_page(q: &Query) -> Result<(), String> {
    if !(1..=100).contains(&q.limit) {
        return Err(format!("limit 必须在 1 到 100 之间，收到 {}：请调整后重试。", q.limit));
    }
    if q.offset < 0 {
        return Err(format!("offset 不能小于 0，收到 {}：请传 0 或更大的数。", q.offset));
    }
    Ok(())
}

/// Query-string parameters to a checked `Query` plus the `expand` field names.
/// `field=v` is equality, `field__ne|gt|gte|lt|lte|contains=v` the other operators; values are read by field type.
pub fn parse_params(def: &CollectionDef, params: &[(String, String)]) -> Result<(Query, Vec<String>), String> {
    let mut q = Query { filters: vec![], status: vec![], sort: None, limit: 20, offset: 0, owner: None };
    let mut expand = vec![];
    let int = |name: &str, v: &str| v.parse::<i64>().map_err(|_| format!("参数“{name}”必须是整数，收到“{v}”：请改成数字。"));
    for (name, v) in params {
        match name.as_str() {
            "limit" => q.limit = int(name, v)?,
            "offset" => q.offset = int(name, v)?,
            "sort" => q.sort = Some(match v.strip_prefix('-') { Some(f) => (f.to_string(), true), None => (v.clone(), false) }),
            "expand" => {
                for f in v.split(',').filter(|f| !f.is_empty()) {
                    if def.field(f).map(|d| d.kind) != Some(FieldType::Relation) {
                        return Err(format!("expand 的字段“{f}”不是集合“{}”的 relation 字段：只能展开关系字段。", def.name));
                    }
                    expand.push(f.to_string());
                }
            }
            _ => {
                let (field, op) = match name.rsplit_once("__") {
                    Some((f, o)) if def.field(name).is_none() => {
                        let op = match o {
                            "ne" => Op::Ne, "gt" => Op::Gt, "gte" => Op::Gte, "lt" => Op::Lt, "lte" => Op::Lte, "contains" => Op::Contains,
                            _ => return Err(format!("参数“{name}”的运算符“{o}”未知：可用的有 ne、gt、gte、lt、lte、contains。")),
                        };
                        (f, op)
                    }
                    _ => (name.as_str(), Op::Eq),
                };
                let val = match def.field(field).map(|f| f.kind) {
                    None if field == "id" => Value::from(int(name, v)?),
                    Some(FieldType::Relation) => Value::from(int(name, v)?),
                    Some(FieldType::Number) => v.parse::<i64>().map(Value::from)
                        .or_else(|_| v.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number).ok_or(()))
                        .map_err(|_| format!("参数“{name}”必须是数字，收到“{v}”：请改成数字。"))?,
                    Some(FieldType::Boolean) => match v.as_str() {
                        "true" => Value::Bool(true),
                        "false" => Value::Bool(false),
                        _ => return Err(format!("参数“{name}”必须是 true 或 false，收到“{v}”。")),
                    },
                    _ => Value::String(v.clone()),
                };
                check_filter(def, field, op, &val)?;
                q.filters.push((field.to_string(), op, val));
            }
        }
    }
    check_page(&q)?;
    if let Some((f, _)) = &q.sort {
        check_field(def, f)?;
    }
    Ok((q, expand))
}
