//! Record queries: one SQL builder serves both the live (published) and the draft view.
//! Field expressions are exactly `json_extract(<column>,'$.<field>')` so that expression indexes can match them.

use crate::changes;
use crate::schema::{project, valid_name, CollectionDef, FieldType};
use rusqlite::{params_from_iter, types::Value as Sql, Connection};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op { Eq, Ne, Gt, Gte, Lt, Lte, Contains }

pub struct Query {
    pub filters: Vec<(String, Op, Value)>,
    /// (field, descending)
    pub sort: Option<(String, bool)>,
    pub limit: i64,
    pub offset: i64,
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

fn check_filter(def: &CollectionDef, field: &str, op: Op, v: &Value) -> Result<(), String> {
    check_field(def, field)?;
    if to_sql(v).is_none() {
        return Err(format!("筛选字段“{field}”的值 {v} 不合法：只能是字符串、数字或布尔值。"));
    }
    if op == Op::Contains && (field == "id" || def.field(field).map(|f| f.kind) != Some(FieldType::Text) || !v.is_string()) {
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

fn expr(field: &str) -> String {
    if field == "id" {
        return "id".into();
    }
    debug_assert!(valid_name(field));
    format!("json_extract({{col}},'$.{field}')")
}

/// (column, WHERE clause, parameters) shared by the page query and the count query.
fn build(def: &CollectionDef, q: &Query, live: bool) -> Result<(&'static str, String, Vec<Sql>), String> {
    debug_assert!(valid_name(&def.name));
    let col = if live { "published" } else { "coalesce(draft, published)" };
    let mut wh = format!("collection = '{}' AND {col} IS NOT NULL", def.name);
    let mut params = vec![];
    for (field, op, v) in &q.filters {
        check_filter(def, field, *op, v)?;
        let e = expr(field).replace("{col}", col);
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
    Ok((col, wh, params))
}

pub fn run(conn: &Connection, def: &CollectionDef, q: &Query, live: bool) -> Result<(Vec<Row>, i64), String> {
    if !(1..=100).contains(&q.limit) {
        return Err(format!("limit 必须在 1 到 100 之间，收到 {}：请调整后重试。", q.limit));
    }
    if q.offset < 0 {
        return Err(format!("offset 不能小于 0，收到 {}：请传 0 或更大的数。", q.offset));
    }
    let (col, wh, mut params) = build(def, q, live)?;
    let order = match &q.sort {
        None => "id DESC".to_string(),
        Some((f, desc)) => {
            check_field(def, f)?;
            let d = if *desc { "DESC" } else { "ASC" };
            format!("{} {d}, id {d}", expr(f).replace("{col}", col))
        }
    };
    let err = |e: rusqlite::Error| format!("查询集合“{}”失败：{e}", def.name);
    let total: i64 = conn
        .query_row(&format!("SELECT count(*) FROM records WHERE {wh}"), params_from_iter(&params), |r| r.get(0))
        .map_err(err)?;
    params.push(Sql::Integer(q.limit));
    params.push(Sql::Integer(q.offset));
    let mut stmt = conn
        .prepare(&format!(
            "SELECT id, published IS NOT NULL, draft IS NOT NULL, draft_deleted, {col} FROM records WHERE {wh} ORDER BY {order} LIMIT ? OFFSET ?"
        ))
        .map_err(err)?;
    let rows = stmt
        .query_map(params_from_iter(&params), |r| {
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
