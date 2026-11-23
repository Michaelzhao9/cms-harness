//! The single write path: every operation is a tool, called through `call`.
//! Adding a tool = one `args` struct, one `spec::<Args>(..)` entry in `specs`, one arm in `dispatch`.

use crate::auth::{Actor, Scope};
use crate::changes::{self, Target};
use crate::schema::{check_def, CollectionDef, FieldDef, FieldType};
use crate::App;
use rusqlite::{Connection, OptionalExtension};
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use std::sync::OnceLock;

pub struct ToolSpec {
    pub name: &'static str,
    pub scope: Scope,
    pub description: &'static str,
    pub schema: Value,
}

fn spec<T: JsonSchema>(name: &'static str, scope: Scope, description: &'static str) -> ToolSpec {
    let mut settings = schemars::generate::SchemaSettings::draft2020_12();
    settings.inline_subschemas = true;
    let schema = settings.into_generator().into_root_schema_for::<T>();
    ToolSpec { name, scope, description, schema: serde_json::to_value(schema).expect("schema is valid JSON") }
}

pub fn specs() -> &'static [ToolSpec] {
    static SPECS: OnceLock<Vec<ToolSpec>> = OnceLock::new();
    SPECS.get_or_init(|| {
        vec![
            spec::<NoArgs>("list_collections", Scope::Read, "列出所有集合（不含已删除的），带状态和记录数。"),
            spec::<NameArgs>("get_collection", Scope::Read, "查看一个集合的定义（有草稿时返回草稿）、状态和记录数。"),
            spec::<CreateCollection>("create_collection", Scope::Structure, "新建集合草稿：名字、标题和字段列表。发布后才对外生效。"),
            spec::<UpdateCollection>("update_collection", Scope::Structure, "修改集合草稿：标题、公开读取、增加/修改/删除字段。"),
            spec::<DeleteCollection>("delete_collection", Scope::Structure, "删除集合草稿；从未发布的集合直接消失，已发布的待发布时才真正删除。"),
        ]
    })
}

pub fn call(app: &App, actor: &Actor, name: &str, args: Value) -> Result<Value, String> {
    let spec = specs().iter().find(|s| s.name == name).ok_or_else(|| {
        let all: Vec<_> = specs().iter().map(|s| s.name).collect();
        format!("没有名为“{name}”的工具，可用工具：{}。", all.join("、"))
    })?;
    if !actor.can(spec.scope) {
        let need = serde_json::to_value(spec.scope).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
        return Err(format!("“{}”没有权限调用“{name}”：需要 {need} 权限。", actor.name));
    }
    let args = if args.is_null() { json!({}) } else { args };
    let conn = app.conn();
    dispatch(&conn, actor, name, args)
}

/// Marker of the fall-through message for a listed tool that has no dispatch arm (checked by a test).
const NOT_IMPLEMENTED: &str = "尚未实现";

fn first_quoted(msg: &str) -> Option<&str> {
    let rest = &msg[msg.find('`')? + 1..];
    Some(&rest[..rest.find('`')?])
}

/// Deserialise a tool's arguments; every failure is a Chinese sentence that names the offending parameter.
fn parse<T: DeserializeOwned>(tool: &str, args: Value) -> Result<T, String> {
    let Value::Object(map) = &args else {
        return Err(format!("“{tool}”的参数必须是一个 JSON 对象，请用 {{\"参数名\": 值}} 的形式传参。"));
    };
    let first = match serde_json::from_value::<T>(args.clone()) {
        Ok(v) => return Ok(v),
        Err(e) => e.to_string(),
    };
    let accepted = || -> String {
        specs()
            .iter()
            .find(|s| s.name == tool)
            .and_then(|s| s.schema["properties"].as_object())
            .map(|p| p.keys().cloned().collect::<Vec<_>>().join("、"))
            .unwrap_or_default()
    };
    if let Some(k) = first_quoted(&first) {
        if first.starts_with("unknown field") && map.contains_key(k) {
            return Err(format!("“{tool}”没有参数“{k}”：可用的参数是 {}。", accepted()));
        }
        if first.starts_with("missing field") && !map.contains_key(k) {
            return Err(format!("“{tool}”缺少必填参数“{k}”：请补上它。"));
        }
    }
    // Find the top-level parameter whose removal changes the outcome.
    for key in map.keys() {
        let mut copy = map.clone();
        copy.remove(key);
        let same = matches!(serde_json::from_value::<T>(Value::Object(copy)), Err(e) if e.to_string() == first);
        if !same {
            return Err(format!("参数“{key}”不正确，请按工具的参数说明修改（{first}）。"));
        }
    }
    Err(format!("“{tool}”的参数不正确，请按工具的参数说明修改（{first}）。"))
}

fn dispatch(conn: &Connection, actor: &Actor, name: &str, args: Value) -> Result<Value, String> {
    match name {
        "list_collections" => parse::<NoArgs>(name, args).and_then(|_| list_collections(conn)),
        "get_collection" => get_collection(conn, parse(name, args)?),
        "create_collection" => create_collection(conn, actor, parse(name, args)?),
        "update_collection" => update_collection(conn, actor, parse(name, args)?),
        "delete_collection" => delete_collection(conn, actor, parse(name, args)?),
        _ => Err(format!("工具“{name}”{NOT_IMPLEMENTED}：它出现在工具列表里却没有处理逻辑，请联系开发者。")),
    }
}

fn check_note(note: &str) -> Result<&str, String> {
    let note = note.trim();
    if note.is_empty() {
        return Err("note 不能为空：请用一句话写明这次修改的原因。".into());
    }
    if note.chars().count() > 200 {
        return Err("note 太长：最多 200 个字符，请缩短。".into());
    }
    Ok(note)
}

// ---- collection tools ----

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NameArgs {
    name: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateCollection {
    name: String,
    title: String,
    #[serde(default)]
    single: bool,
    #[serde(default)]
    public_read: bool,
    fields: Vec<FieldDef>,
    note: String,
}

/// Only the properties that appear are changed; `name` cannot be changed.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FieldChange {
    name: String,
    title: Option<String>,
    #[serde(rename = "type")]
    kind: Option<FieldType>,
    required: Option<bool>,
    unique: Option<bool>,
    index: Option<bool>,
    multiple: Option<bool>,
    default: Option<Value>,
    options: Option<Vec<String>>,
    target: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateCollection {
    name: String,
    title: Option<String>,
    public_read: Option<bool>,
    add_fields: Option<Vec<FieldDef>>,
    change_fields: Option<Vec<FieldChange>>,
    remove_fields: Option<Vec<String>>,
    /// Accepted and ignored until the destructive-change check exists.
    #[allow(dead_code)]
    confirm: Option<bool>,
    note: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeleteCollection {
    name: String,
    #[allow(dead_code)]
    confirm: Option<bool>,
    note: String,
}

fn record_count(conn: &Connection, name: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT count(*) FROM records WHERE collection = ?1 AND (published IS NOT NULL OR draft IS NOT NULL)",
        [name],
        |r| r.get(0),
    )
    .map_err(|e| format!("统计集合“{name}”的记录数失败：{e}"))
}

/// (name, definition, status) of every collection that is not deleted.
fn collections(conn: &Connection, only: Option<&str>) -> Result<Vec<(String, CollectionDef, &'static str)>, String> {
    let names: Vec<String> = match only {
        Some(n) => vec![n.to_string()],
        None => {
            let mut stmt = conn
                .prepare("SELECT name FROM collections WHERE deleted = 0 ORDER BY name")
                .map_err(|e| format!("读取集合失败：{e}"))?;
            let rows = stmt.query_map([], |r| r.get(0)).map_err(|e| format!("读取集合失败：{e}"))?;
            rows.collect::<Result<_, _>>().map_err(|e| format!("读取集合失败：{e}"))?
        }
    };
    let mut out = vec![];
    for name in names {
        if let Some((def, st)) = changes::collection_view(conn, &name)? {
            out.push((name, def, st));
        }
    }
    Ok(out)
}

fn list_collections(conn: &Connection) -> Result<Value, String> {
    let mut out = vec![];
    for (name, def, status) in collections(conn, None)? {
        out.push(json!({ "name": def.name, "title": def.title, "single": def.single, "status": status,
            "record_count": record_count(conn, &name)? }));
    }
    Ok(Value::Array(out))
}

fn get_collection(conn: &Connection, a: NameArgs) -> Result<Value, String> {
    let (name, def, status) = collections(conn, Some(&a.name))?
        .pop()
        .ok_or_else(|| format!("集合“{}”不存在或已删除，请先用 list_collections 查看现有集合。", a.name))?;
    Ok(json!({ "definition": def, "status": status, "record_count": record_count(conn, &name)? }))
}

/// Every relation target must be an existing, non-deleted collection, or the collection itself.
fn check_targets(conn: &Connection, def: &CollectionDef) -> Result<(), String> {
    for f in def.fields.iter().filter(|f| f.kind == FieldType::Relation) {
        let t = f.target.as_deref().unwrap_or_default();
        let exists = t == def.name
            || conn
                .query_row("SELECT 1 FROM collections WHERE name = ?1 AND deleted = 0", [t], |_| Ok(()))
                .optional()
                .map_err(|e| format!("检查目标集合“{t}”失败：{e}"))?
                .is_some();
        if !exists {
            return Err(format!("关联字段“{}”的目标集合“{t}”不存在：请先创建它，或改成已有集合的名字。", f.name));
        }
    }
    Ok(())
}

fn save_collection(conn: &Connection, actor: &Actor, def: &CollectionDef, note: &str) -> Result<Value, String> {
    check_def(def)?;
    check_targets(conn, def)?;
    changes::save_draft(conn, actor, &Target::Collection(def.name.clone()), &json!(def), note)?;
    Ok(json!({ "name": def.name, "status": changes::collection_status(conn, &def.name)? }))
}

fn create_collection(conn: &Connection, actor: &Actor, a: CreateCollection) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    let def = CollectionDef {
        name: a.name,
        title: a.title,
        single: a.single,
        access: crate::schema::Access { public_read: a.public_read },
        fields: a.fields,
    };
    let taken = conn
        .query_row("SELECT 1 FROM collections WHERE name = ?1", [&def.name], |_| Ok(()))
        .optional()
        .map_err(|e| format!("检查集合名“{}”失败：{e}", def.name))?;
    if taken.is_some() {
        return Err(format!("集合“{}”已存在（可能是已删除或待删除的）：请换一个名字。", def.name));
    }
    save_collection(conn, actor, &def, note)
}

fn update_collection(conn: &Connection, actor: &Actor, a: UpdateCollection) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    let mut def = changes::effective_def(conn, &a.name)?;
    if let Some(t) = a.title {
        def.title = t;
    }
    if let Some(p) = a.public_read {
        def.access.public_read = p;
    }
    for f in a.add_fields.unwrap_or_default() {
        if def.field(&f.name).is_some() {
            return Err(format!("字段“{}”已存在：不能重复添加，要修改请用 change_fields。", f.name));
        }
        def.fields.push(f);
    }
    for c in a.change_fields.unwrap_or_default() {
        let f = def
            .fields
            .iter_mut()
            .find(|f| f.name == c.name)
            .ok_or_else(|| format!("字段“{}”不存在：无法修改，请检查字段名。", c.name))?;
        if let Some(v) = c.title { f.title = v }
        if let Some(v) = c.kind { f.kind = v }
        if let Some(v) = c.required { f.required = v }
        if let Some(v) = c.unique { f.unique = v }
        if let Some(v) = c.index { f.index = v }
        if let Some(v) = c.multiple { f.multiple = v }
        if let Some(v) = c.default { f.default = Some(v) }
        if let Some(v) = c.options { f.options = v }
        if let Some(v) = c.target { f.target = Some(v) }
    }
    for n in a.remove_fields.unwrap_or_default() {
        let i = def
            .fields
            .iter()
            .position(|f| f.name == n)
            .ok_or_else(|| format!("字段“{n}”不存在：无法删除，请检查字段名。"))?;
        def.fields.remove(i);
    }
    save_collection(conn, actor, &def, note)
}

fn delete_collection(conn: &Connection, actor: &Actor, a: DeleteCollection) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    changes::effective_def(conn, &a.name)?;
    changes::mark_deleted(conn, actor, &Target::Collection(a.name.clone()), note)?;
    // A never-published collection's row is gone, so its status is "deleted".
    let status = changes::collection_status(conn, &a.name)?;
    Ok(json!({ "name": a.name, "status": status }))
}
