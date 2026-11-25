//! The single write path: every operation is a tool, called through `call`.
//! Adding a tool = one `args` struct, one `spec::<Args>(..)` entry in `specs`, one arm in `dispatch`.

use crate::auth::{Actor, Scope};
use crate::changes::{self, Target};
use crate::query::{self, Query};
use crate::schema::{check_def, check_view, normalize, project, Action, CollectionDef, FieldDef, FieldType, ViewDef, MAX_NOTES};
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
            spec::<UpdateCollection>("update_collection", Scope::Structure, "修改集合草稿：标题、谁能看（read：public 公开 / members 登录会员 / owner 每个会员只看自己的 / private 仅后台）、谁能从网站提交（create：none / members / public，提交即上线并记下提交人）、增加/修改/删除字段。会员相关的规则要先用 set_member_login 打开会员登录。"),
            spec::<DeleteCollection>("delete_collection", Scope::Structure, "删除集合草稿；从未发布的集合直接消失，已发布的待发布时才真正删除。"),
            spec::<CreateRecord>("create_record", Scope::Write, "新建记录草稿：集合名和字段值（data）。发布后才对外生效。"),
            spec::<UpdateRecord>("update_record", Scope::Write, "修改记录草稿：只需传要改的字段，null 表示清空该字段。"),
            spec::<DeleteRecord>("delete_record", Scope::Write, "删除记录；从未发布的记录直接消失，已发布的待发布时才真正删除。"),
            spec::<GetRecord>("get_record", Scope::Read, "按编号读取一条记录（有草稿时返回草稿）和状态。"),
            spec::<QueryRecords>("query_records", Scope::Read, "查询集合的记录（草稿视图）。filter 与 sort 只能用 id 或设置了 index/unique 的字段；status 可只看某几种状态的记录。"),
            spec::<Search>("search", Scope::Read, "全文搜索已发布的记录（中文、英文都行），按相关度排序，返回集合、编号和命中的一小段文字。网站页面里用模板函数 search(q, collection=..., limit=...) 做站内搜索，访客只会搜到自己能看的内容。"),
            spec::<SchedulePublish>("schedule_publish", Scope::Publish, "定时发布：到了时间按普通发布的流程和检查上线，记在发布记录里，发布人是你。at 是时间，tz_offset_minutes 是这个时间所在时区比 UTC 快多少分钟（北京时间 480，默认 0 即 UTC）。不传 targets 就发布到时所有待发布的改动。"),
            spec::<NoArgs>("list_schedules", Scope::Read, "列出等待中的定时发布和最近执行过的定时发布（时间是 UTC）。"),
            spec::<IdArgs>("cancel_schedule", Scope::Publish, "取消一个还没执行的定时发布。"),
            spec::<NoArgs>("list_pending", Scope::Read, "列出所有待发布的改动：改了什么、谁改的、波及多少记录。"),
            spec::<Publish>("publish", Scope::Publish, "把待发布的改动发布上线：全部，或只发 targets 里列出的几项。发布前整体检查，不通过就什么都不上线。"),
            spec::<TargetArgs>("list_versions", Scope::Read, "列出一个对象的发布历史（最近 30 个版本）。target 形如 record:12、collection:news。"),
            spec::<RestoreVersion>("restore_version", Scope::Write, "把一个历史版本放回草稿，不会直接上线；发布后才生效。已删除的对象也能这样找回。"),
            spec::<TargetArgs>("discard_draft", Scope::Write, "丢弃一个对象的草稿或待删除标记，回到线上版本；从未发布过的对象会直接消失。"),
            spec::<NoArgs>("list_views", Scope::Read, "列出所有界面声明（不含已删除的），带状态；有草稿时返回草稿定义。"),
            spec::<SetView>("set_view", Scope::Structure, "新建或修改界面草稿：它基于哪个集合、显示哪些列、可过滤和排序的字段、允许的操作。发布后才生效。"),
            spec::<DeleteView>("delete_view", Scope::Structure, "删除界面草稿；从未发布的界面直接消失，已发布的待发布时才真正删除。"),
            spec::<NoArgs>("get_site_notes", Scope::Read, "读取站点说明（有草稿时返回草稿）和状态。"),
            spec::<SetSiteNotes>("set_site_notes", Scope::Structure, "修改站点说明草稿：一段给对话模型看的文字，最多 20000 个字符。发布后才生效。"),
            spec::<NoArgs>("get_site_settings", Scope::Read, "读取网站的前端设置：import_map（浏览器里 import 的包从哪个网址加载）和 script_origins（网站允许加载脚本的其他来源，默认 https://esm.sh）。"),
            spec::<SetSiteSettings>("set_site_settings", Scope::Structure, "修改网站的设置（草稿，发布后生效）：name 是站点名称（网页标题、内置页面和后台都用它），import_map、script_origins 管前端脚本。只传要改的项，传 null 恢复默认。例：用 React 时 import_map 设为 {\"imports\": {\"react\": \"https://esm.sh/react@19\", \"react/jsx-runtime\": \"https://esm.sh/react@19/jsx-runtime\", \"react-dom/client\": \"https://esm.sh/react-dom@19/client\"}}。"),
            spec::<SetMemberLogin>("set_member_login", Scope::Structure, "打开或关闭网站的会员注册和登录（草稿，发布后生效）。打开后网站有 /account/signup、/account/login、/account/logout 页面和 /api/v1/members/* 接口，模板里 member 是当前登录的会员；后台左边会自动出现「用户管理」，关闭后它自动消失；要改它的样子，用 set_view 并写 collection: \"_members\"（字段 email、name、status、created_at、last_login）。网站要做登录功能时用它，不要自己建会员集合。"),
            spec::<ListMembers>("list_members", Scope::Read, "列出网站的注册会员（不含密码）：可按状态 active/disabled 和关键词筛选。"),
            spec::<SetMemberStatus>("set_member_status", Scope::Write, "停用或恢复一个会员（立即生效）：停用后他不能登录，已登录的会话也会失效。"),
            spec::<UploadMedia>("upload_media", Scope::Write, "上传图片或 PDF（base64，最大 5 MB）：返回 /uploads/ 路径，可填进 media 字段。"),
            spec::<NoArgs>("list_site_files", Scope::Read, "列出网站的源文件：状态、字节数和行数（有草稿时按草稿）。不含内容，读内容用 get_site_file。"),
            spec::<PathArgs>("get_site_file", Scope::Read, "按路径读一个网站文件：内容（有草稿时返回草稿）和状态。"),
            spec::<WriteSiteFile>("write_site_file", Scope::Structure, "新建或替换一个网站文件的草稿，发布后上线。最多 2000 个文件。路径：相对路径，字母（区分大小写）、数字和 . _ / -，不能有 ..，最长 200 个字符，不能在 admin/ api/ uploads/ _hc/ 下，也不能叫 mcp。文本文件（html css js mjs jsx ts tsx json svg txt md xml webmanifest map）：content 是文字，最大 1 MB。二进制文件（png jpg jpeg gif webp avif ico woff woff2 ttf otf mp4 webm）：content 是标准 base64，解码后最大 5 MB。`.html` 是 minijinja 模板，按路径访问（/about 对应 about.html 或 about/index.html），可以 extends 或 include 其他文件，用 collection(名字, filter?, sort?, limit?, offset?) 和 record(集合, 编号) 读线上内容，还有 site、request、member。`.jsx/.ts/.tsx` 会转换后提供（去掉 TypeScript，JSX 走 react/jsx-runtime）。写入时检查语法。用户在画板上手动调整过的元素（get_site_file 返回的 hand_edits）要保留：改写会让它们对不上时，工具不写入，先问用户。"),
            spec::<DeleteSiteFile>("delete_site_file", Scope::Structure, "删除一个网站文件：从没发布过的立刻消失，已上线的在发布删除后才消失。"),
            spec::<UploadSiteBundle>("upload_site_bundle", Scope::Structure, "一次上传一个网站文件的 zip（比如 Vite、Next 导出的静态网站，图片和字体一起）作为草稿：每个文件都按 write_site_file 的规则检查，解压后最大 50 MB。有一个文件不合格，整个包都不收，并列出所有问题。replace: true 时，线上有而包里没有的文件会同时标记为待删除；否则保持不变。只有一层顶层文件夹且里面有 index.html 时会去掉这层；目录和 macOS 元数据会跳过。大包也可以把 zip 原样 POST 到 /api/v1/site/bundle?note=...&replace=1。"),
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
    dispatch(app, &conn, actor, name, args)
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
        let top_level = specs()
            .iter()
            .find(|s| s.name == tool)
            .is_some_and(|s| s.schema["properties"].get(k).is_some());
        if first.starts_with("missing field") && !map.contains_key(k) && top_level {
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

fn dispatch(app: &App, conn: &Connection, actor: &Actor, name: &str, args: Value) -> Result<Value, String> {
    match name {
        "list_collections" => parse::<NoArgs>(name, args).and_then(|_| list_collections(conn)),
        "get_collection" => get_collection(conn, parse(name, args)?),
        "create_collection" => create_collection(conn, actor, parse(name, args)?),
        "update_collection" => update_collection(conn, actor, parse(name, args)?),
        "delete_collection" => delete_collection(conn, actor, parse(name, args)?),
        "create_record" => create_record(conn, actor, &app.origin, parse(name, args)?),
        "update_record" => update_record(conn, actor, &app.origin, parse(name, args)?),
        "delete_record" => delete_record(conn, actor, parse(name, args)?),
        "get_record" => get_record(conn, parse(name, args)?),
        "query_records" => query_records(conn, parse(name, args)?),
        "search" => {
            let a: Search = parse(name, args)?;
            let hits = crate::search::search(conn, &a.q, a.collection.as_deref(), crate::access::Viewer::Key, a.limit.unwrap_or(20).clamp(1, 50))?;
            Ok(json!(hits.into_iter().map(|h| json!({ "collection": h.collection, "id": h.id, "snippet": h.snippet })).collect::<Vec<_>>()))
        }
        "schedule_publish" => {
            let a: SchedulePublish = parse(name, args)?;
            let note = check_note(&a.note)?;
            let at = crate::schedule::normalize(conn, &a.at, a.tz_offset_minutes.unwrap_or(0))?;
            let id = crate::schedule::create(conn, actor, &at, a.targets, note)?;
            Ok(json!({ "id": id, "at_utc": at }))
        }
        "list_schedules" => parse::<NoArgs>(name, args).and_then(|_| Ok(json!(crate::schedule::list(conn)?))),
        "cancel_schedule" => {
            let a: IdArgs = parse(name, args)?;
            crate::schedule::cancel(conn, a.id).map(|_| json!({ "id": a.id, "status": "cancelled" }))
        }
        "list_pending" => parse::<NoArgs>(name, args).and_then(|_| Ok(json!(changes::pending(conn)?))),
        "publish" => publish(conn, actor, parse(name, args)?),
        "list_versions" => list_versions(conn, parse(name, args)?),
        "restore_version" => restore_version(conn, actor, parse(name, args)?),
        "discard_draft" => discard_draft(conn, actor, parse(name, args)?),
        "list_views" => parse::<NoArgs>(name, args).and_then(|_| list_views(conn)),
        "set_view" => set_view(conn, actor, parse(name, args)?),
        "delete_view" => delete_view(conn, actor, parse(name, args)?),
        "get_site_notes" => parse::<NoArgs>(name, args).and_then(|_| get_site_notes(conn)),
        "set_site_notes" => set_site_notes(conn, actor, parse(name, args)?),
        "set_member_login" => {
            let a: SetMemberLogin = parse(name, args)?;
            crate::members::set_login(conn, actor, a.enabled, check_note(&a.note)?)
        }
        "list_members" => {
            let a: ListMembers = parse(name, args)?;
            crate::members::list(conn, a.status.as_deref(), a.query.as_deref(), a.limit.unwrap_or(50), a.offset.unwrap_or(0))
        }
        "set_member_status" => {
            let a: SetMemberStatus = parse(name, args)?;
            check_note(&a.note)?;
            crate::members::set_status(conn, a.id, &a.status)
        }
        "get_site_settings" => parse::<NoArgs>(name, args).and_then(|_| get_site_settings(conn)),
        "set_site_settings" => set_site_settings(conn, actor, parse(name, args)?),
        "upload_media" => upload_media(app, parse(name, args)?),
        "list_site_files" => parse::<NoArgs>(name, args).and_then(|_| list_site_files(conn)),
        "get_site_file" => get_site_file(conn, parse(name, args)?),
        "write_site_file" => write_site_file(conn, actor, parse(name, args)?),
        "delete_site_file" => delete_site_file(conn, actor, parse(name, args)?),
        "upload_site_bundle" => upload_site_bundle(conn, actor, parse(name, args)?),
        _ => Err(format!("工具“{name}”{NOT_IMPLEMENTED}：它出现在工具列表里却没有处理逻辑，请联系开发者。")),
    }
}

/// Distinguishes a key that is present with `null` (`Some(None)`) from one that is absent (`None`).
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<Value>>, D::Error> {
    Ok(Some(Option::deserialize(d)?))
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
    /// Old form of `read: public`.
    #[serde(default)]
    public_read: bool,
    /// Who may read live records: public, members (logged-in members), owner (each member their own), private (default).
    read: Option<crate::schema::Read>,
    /// Who may add records from the site, live at once: none (default), members, public.
    create: Option<crate::schema::Create>,
    /// Where each record's page lives, like "/news/{slug}"; "/<name>/{slug}" when left out. Pages appear once the
    /// site has a detail template: "<name>/_item.html" or the site-wide "_item.html".
    url: Option<String>,
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
    /// A value sets the default; `null` removes it.
    #[serde(default, deserialize_with = "present")]
    #[schemars(with = "Option<Value>")]
    default: Option<Option<Value>>,
    options: Option<Vec<String>>,
    target: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateCollection {
    name: String,
    title: Option<String>,
    /// Old form of `read`: true means public, false private.
    public_read: Option<bool>,
    /// public, members, owner or private.
    read: Option<crate::schema::Read>,
    /// none, members or public.
    create: Option<crate::schema::Create>,
    /// Where each record's page lives, like "/news/{slug}" or "/products/{category.slug}/{slug}"; "" for the default.
    url: Option<String>,
    add_fields: Option<Vec<FieldDef>>,
    change_fields: Option<Vec<FieldChange>>,
    remove_fields: Option<Vec<String>>,
    /// Required when `remove_fields` would drop values: the number of records that hold them.
    confirm: Option<i64>,
    note: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeleteCollection {
    name: String,
    /// Required when the collection holds records: how many.
    confirm: Option<i64>,
    note: String,
}

/// (name, definition, status) of every collection that is not deleted.
fn collections(conn: &Connection, only: Option<&str>) -> Result<Vec<(String, CollectionDef, &'static str)>, String> {
    let names: Vec<String> = match only {
        Some(n) => vec![n.to_string()],
        None => {
            let mut stmt = conn
                .prepare("SELECT name FROM collections WHERE deleted = 0 OR draft IS NOT NULL ORDER BY name")
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
            "record_count": changes::record_count(conn, &name)? }));
    }
    Ok(Value::Array(out))
}

fn get_collection(conn: &Connection, a: NameArgs) -> Result<Value, String> {
    let (name, def, status) = collections(conn, Some(&a.name))?
        .pop()
        .ok_or_else(|| format!("集合“{}”不存在或已删除，请先用 list_collections 查看现有集合。", a.name))?;
    Ok(json!({ "definition": def, "status": status, "record_count": changes::record_count(conn, &name)? }))
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
    changes::save_definition(conn, actor, def, note)?;
    Ok(json!({ "name": def.name, "status": changes::collection_status(conn, &def.name)? }))
}

fn create_collection(conn: &Connection, actor: &Actor, a: CreateCollection) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    let def = CollectionDef {
        name: a.name,
        title: a.title,
        single: a.single,
        access: crate::schema::Access {
            read: a.read.unwrap_or(if a.public_read { crate::schema::Read::Public } else { crate::schema::Read::Private }),
            create: a.create.unwrap_or_default(),
        },
        fields: a.fields,
        url: a.url.filter(|u| !u.trim().is_empty()),
    };
    let taken = conn
        .query_row("SELECT 1 FROM collections WHERE name = ?1", [&def.name], |_| Ok(()))
        .optional()
        .map_err(|e| format!("检查集合名“{}”失败：{e}", def.name))?;
    if taken.is_some() {
        if changes::collection_status(conn, &def.name)? == "deleted" {
            return Err(format!(
                "名字“{0}”属于一个已删除的集合，它的记录还保留着：要用回它，请对 collection:{0} 调用 list_versions 和 restore_version 把它恢复；否则换一个名字。",
                def.name
            ));
        }
        let pending = if changes::collection_status(conn, &def.name)? == "deleting" {
            format!("它正等待删除；对 collection:{} 调用 discard_draft 可以取消删除。", def.name)
        } else {
            "要修改它请用 update_collection。".to_string()
        };
        return Err(format!("集合“{}”已存在，请换一个名字。{pending}", def.name));
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
        def.access.read = if p { crate::schema::Read::Public } else { crate::schema::Read::Private };
    }
    if let Some(r) = a.read {
        def.access.read = r;
    }
    if let Some(c) = a.create {
        def.access.create = c;
    }
    if let Some(u) = a.url {
        def.url = Some(u.trim().to_string()).filter(|u| !u.is_empty());
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
        if let Some(v) = c.default { f.default = v }
        if let Some(v) = c.options { f.options = v }
        if let Some(v) = c.target { f.target = Some(v) }
    }
    let removed = a.remove_fields.unwrap_or_default();
    for n in &removed {
        let i = def
            .fields
            .iter()
            .position(|f| &f.name == n)
            .ok_or_else(|| format!("字段“{n}”不存在：无法删除，请检查字段名。"))?;
        def.fields.remove(i);
    }
    let affected = changes::affected_by_removal(conn, &a.name, &removed)?;
    if affected > 0 && a.confirm != Some(affected) {
        return Err(format!(
            "删除字段 {} 会让 {affected} 条记录里已有的内容不再显示：确认要删，请加上参数 confirm: {affected}。内容不会被清除，把字段加回来就能恢复。",
            removed.join("、")
        ));
    }
    save_collection(conn, actor, &def, note)
}

fn delete_collection(conn: &Connection, actor: &Actor, a: DeleteCollection) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    changes::effective_def(conn, &a.name)?;
    let count = changes::record_count(conn, &a.name)?;
    if count > 0 && a.confirm != Some(count) {
        let fate = if changes::collection_status(conn, &a.name)? == "draft" {
            "这个集合从未发布，它和这些记录会被直接清除、无法恢复"
        } else {
            "发布删除后记录会保留，恢复集合就能找回"
        };
        return Err(format!("集合“{}”里有 {count} 条记录（{fate}）：确认要删除，请加上参数 confirm: {count}。", a.name));
    }
    changes::mark_deleted(conn, actor, &Target::Collection(a.name.clone()), note)?;
    // A never-published collection's row is gone, so its status is "deleted".
    let status = changes::collection_status(conn, &a.name)?;
    Ok(json!({ "name": a.name, "status": status }))
}

// ---- record tools ----

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateRecord {
    collection: String,
    data: Value,
    /// The record's address segment; made from its title when left out.
    slug: Option<String>,
    note: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateRecord {
    id: i64,
    data: Value,
    /// A new address segment (a draft until published; the old address keeps redirecting).
    slug: Option<String>,
    note: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeleteRecord {
    id: i64,
    note: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetRecord {
    id: i64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryRecords {
    collection: String,
    /// `{"field": value}` or `{"field": {"gte": value}}`; operators: eq ne gt gte lt lte contains.
    filter: Option<Value>,
    /// Keep only records in these states (draft, changed, deleting, published); leave out for all.
    status: Option<Vec<query::Status>>,
    /// Field name, or `-field` for descending. Default: newest first.
    sort: Option<String>,
    /// 1 to 100, default 20.
    limit: Option<i64>,
    offset: Option<i64>,
}

struct RecRow {
    collection: String,
    published: Option<String>,
    draft: Option<String>,
    draft_deleted: bool,
}

impl RecRow {
    fn status(&self) -> &'static str {
        changes::status(self.published.is_some(), self.draft.is_some(), self.draft_deleted)
    }
}

fn find_record(conn: &Connection, id: i64) -> Result<Option<RecRow>, String> {
    conn.query_row("SELECT collection, published, draft, draft_deleted FROM records WHERE id = ?1", [id], |r| {
        Ok(RecRow { collection: r.get(0)?, published: r.get(1)?, draft: r.get(2)?, draft_deleted: r.get(3)? })
    })
    .optional()
    .map(|r| r.filter(|r| r.published.is_some() || r.draft.is_some()))
    .map_err(|e| format!("读取记录 {id} 失败：{e}"))
}

fn load_record(conn: &Connection, id: i64) -> Result<RecRow, String> {
    find_record(conn, id)?.ok_or_else(|| format!("记录 {id} 不存在或已被删除：请检查编号。"))
}

/// Every relation value in `changed` must be a live record of the field's target collection.
fn check_relations(conn: &Connection, def: &CollectionDef, changed: &Value) -> Result<(), String> {
    for f in def.fields.iter().filter(|f| f.kind == FieldType::Relation) {
        let ids = match changed.get(&f.name) {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Null) | None => continue,
            Some(v) => vec![v.clone()],
        };
        let target = f.target.as_deref().unwrap_or_default();
        for id in ids.iter().filter_map(Value::as_i64) {
            let found = find_record(conn, id)?.filter(|r| r.collection == target);
            if found.is_none() {
                return Err(format!("关联字段“{}”指向的记录 {id} 不存在：它必须是集合“{target}”里现有的记录，请检查编号。", f.name));
            }
        }
    }
    Ok(())
}

/// Links in rich text that point at a record's page on this site are kept as `/_r/<id>`, so they follow the record.
fn own_links(conn: &Connection, origin: &str, def: &CollectionDef, mut doc: Value) -> Value {
    for f in def.fields.iter().filter(|f| f.kind == crate::schema::FieldType::Richtext) {
        if let Some(Value::String(html)) = doc.get_mut(&f.name) {
            *html = crate::routes::link_refs(conn, origin, html);
        }
    }
    doc
}

fn create_record(conn: &Connection, actor: &Actor, origin: &str, a: CreateRecord) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    let def = changes::effective_def(conn, &a.collection)?;
    if def.single && changes::record_count(conn, &a.collection)? > 0 {
        return Err(format!("集合“{}”是单例集合，已经有一条记录：请用 update_record 修改它。", a.collection));
    }
    let doc = crate::schema::fill_defaults(&def, normalize(&def, &json!({}), &a.data)?)?;
    changes::check_unique(conn, &def, None, &doc, false)?;
    check_relations(conn, &def, &doc)?;
    let doc = own_links(conn, origin, &def, doc);
    let id = changes::insert_record(conn, actor, &a.collection, &doc, note)?;
    let slug = match a.slug.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => crate::routes::rename(conn, &a.collection, id, s).map(|_| s.to_string())?,
        None => crate::routes::assign(conn, &def, id, &doc)?,
    };
    Ok(json!({ "id": id, "slug": slug, "status": load_record(conn, id)?.status() }))
}

fn update_record(conn: &Connection, actor: &Actor, origin: &str, a: UpdateRecord) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    let row = load_record(conn, a.id)?;
    if row.status() == "deleting" {
        return Err(format!("记录 {} 正等待删除，不能修改：如要保留它，请先用 discard_draft 放弃删除。", a.id));
    }
    let def = changes::effective_def(conn, &row.collection)?;
    let base: Value = match (&row.draft, &row.published) {
        (Some(d), _) => serde_json::from_str(d).map_err(|e| format!("记录 {} 的内容已损坏：{e}", a.id))?,
        (None, Some(p)) => {
            // Only a live copy: it follows the live definition, drafts follow the one in force for drafts.
            let live: Value = serde_json::from_str(p).map_err(|e| format!("记录 {} 的内容已损坏：{e}", a.id))?;
            changes::to_current(conn, &row.collection, &live)?
        }
        (None, None) => json!({}),
    };
    let doc = own_links(conn, origin, &def, normalize(&def, &base, &a.data)?);
    changes::check_unique(conn, &def, Some(a.id), &doc, false)?;
    check_relations(conn, &def, &a.data)?;
    changes::save_draft(conn, actor, &Target::Record(a.id), &doc, note)?;
    if let Some(s) = a.slug.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        crate::routes::rename(conn, &row.collection, a.id, s)?;
    }
    Ok(json!({ "id": a.id, "status": load_record(conn, a.id)?.status() }))
}

fn delete_record(conn: &Connection, actor: &Actor, a: DeleteRecord) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    load_record(conn, a.id)?;
    changes::mark_deleted(conn, actor, &Target::Record(a.id), note)?;
    let status = find_record(conn, a.id)?.map_or("deleted", |r| r.status());
    Ok(json!({ "id": a.id, "status": status }))
}

fn collection_def_for_read(conn: &Connection, name: &str) -> Result<CollectionDef, String> {
    changes::collection_view(conn, name)?
        .map(|(d, _)| d)
        .ok_or_else(|| format!("集合“{name}”不存在或已删除，请先用 list_collections 查看现有集合。"))
}

fn get_record(conn: &Connection, a: GetRecord) -> Result<Value, String> {
    let row = load_record(conn, a.id)?;
    let def = collection_def_for_read(conn, &row.collection)?;
    let doc: Value = serde_json::from_str(row.draft.as_deref().or(row.published.as_deref()).unwrap_or("{}"))
        .map_err(|e| format!("记录 {} 的内容已损坏：{e}", a.id))?;
    let slug: Option<String> = conn.query_row("SELECT coalesce(draft_slug, slug) FROM records WHERE id = ?1", [a.id], |r| r.get(0)).ok().flatten();
    let url = crate::routes::url_for(conn, crate::site::Mode::Preview, &def, a.id, &doc);
    Ok(json!({ "id": a.id, "collection": row.collection, "status": row.status(), "slug": slug, "url": url, "data": project(&def, &doc) }))
}

fn query_records(conn: &Connection, a: QueryRecords) -> Result<Value, String> {
    let def = collection_def_for_read(conn, &a.collection)?;
    let q = Query {
        filters: a.filter.as_ref().map(|f| query::parse_filter(&def, f)).transpose()?.unwrap_or_default(),
        status: a.status.unwrap_or_default(),
        sort: a.sort.map(|s| match s.strip_prefix('-') {
            Some(f) => (f.to_string(), true),
            None => (s, false),
        }),
        limit: a.limit.unwrap_or(20),
        offset: a.offset.unwrap_or(0),
        owner: None,
    };
    let (rows, total) = query::run(conn, &def, &q, false)?;
    let items: Vec<Value> = rows.into_iter().map(|r| json!({ "id": r.id, "status": r.status, "data": r.doc })).collect();
    Ok(json!({ "items": items, "total": total }))
}

// ---- publish and history tools ----

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TargetArgs {
    /// `collection:<name>`, `record:<id>`, `view:<name>`, `file:<path>` or `notes`.
    target: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Publish {
    /// Targets to publish; leave out to publish everything you are allowed to.
    targets: Option<Vec<String>>,
    note: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RestoreVersion {
    target: String,
    version_id: i64,
    note: String,
}

/// Structure objects (collections, views, the site notes) need the structure scope on top of the tool's own.
fn check_structure_scope(actor: &Actor, target: &Target) -> Result<(), String> {
    if actor.can(changes::own_scope(target)) {
        return Ok(());
    }
    Err(format!("“{}”没有权限改动“{target}”：它属于结构，需要 structure 权限。", actor.name))
}

fn publish(conn: &Connection, actor: &Actor, a: Publish) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    let targets = a.targets.map(|list| list.iter().map(|t| Target::parse(t)).collect::<Result<Vec<_>, _>>()).transpose()?;
    let release = changes::publish(conn, actor, targets.as_deref(), note)?;
    Ok(json!({ "release_id": release.id, "published": release.published, "skipped": release.skipped }))
}

fn list_versions(conn: &Connection, a: TargetArgs) -> Result<Value, String> {
    Ok(json!(changes::versions(conn, &Target::parse(&a.target)?)?))
}

fn restore_version(conn: &Connection, actor: &Actor, a: RestoreVersion) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    let target = Target::parse(&a.target)?;
    check_structure_scope(actor, &target)?;
    changes::restore(conn, actor, &target, a.version_id, note)?;
    Ok(json!({ "target": a.target, "status": changes::target_status(conn, &target)? }))
}

fn discard_draft(conn: &Connection, actor: &Actor, a: TargetArgs) -> Result<Value, String> {
    let target = Target::parse(&a.target)?;
    check_structure_scope(actor, &target)?;
    changes::discard(conn, &target)?;
    Ok(json!({ "target": a.target, "status": changes::target_status(conn, &target)? }))
}

// ---- views and site notes ----

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetView {
    name: String,
    title: String,
    collection: String,
    columns: Vec<String>,
    #[serde(default)]
    filters: Vec<String>,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    actions: Vec<Action>,
    /// table (default), cards (a grid with picture, title, subtitle) or list.
    #[serde(default)]
    layout: crate::schema::Layout,
    /// Cards and lists: the field used as each item's title (default: the first column).
    title_field: Option<String>,
    subtitle_field: Option<String>,
    /// Cards: a media field shown as the picture.
    image_field: Option<String>,
    /// comfortable (default) or compact.
    #[serde(default)]
    density: crate::schema::Density,
    /// An indexed select field: one section per option.
    group_by: Option<String>,
    note: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeleteView {
    name: String,
    note: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetMemberLogin {
    enabled: bool,
    note: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListMembers {
    /// active or disabled
    status: Option<String>,
    /// Matches email or name.
    query: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetMemberStatus {
    id: i64,
    /// active or disabled
    status: String,
    note: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetSiteSettings {
    /// An import map object ({"imports": {...}}), or null for none. Leave out to keep it.
    #[serde(default, deserialize_with = "present")]
    import_map: Option<Option<Value>>,
    /// Origins scripts may load from, like ["https://esm.sh"]; null for the default. Leave out to keep it.
    #[serde(default, deserialize_with = "present")]
    script_origins: Option<Option<Value>>,
    /// The site's name, shown in page titles, the built-in pages and the admin; null for the default. Leave out to keep it.
    #[serde(default, deserialize_with = "present")]
    name: Option<Option<Value>>,
    note: String,
}

/// The site's front-end options as drafts see them (the draft if there is one), with each one's status.
fn get_site_settings(conn: &Connection) -> Result<Value, String> {
    let get = |k: &str| -> Result<Value, String> {
        let target = Target::Setting(k.into());
        Ok(json!({ "value": changes::setting(conn, k, true), "status": changes::target_status(conn, &target)? }))
    };
    Ok(json!({ "name": get("site_name")?, "import_map": get("import_map")?, "script_origins": get("script_origins")? }))
}

fn set_site_settings(conn: &Connection, actor: &Actor, a: SetSiteSettings) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    let origin_ok = |o: &str| {
        let rest = o.strip_prefix("https://").or_else(|| o.strip_prefix("http://"));
        rest.is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_alphanumeric() || b".-:".contains(&b)))
    };
    let mut writes: Vec<(&str, Option<String>)> = vec![];
    if let Some(n) = a.name {
        match n.as_ref().map(|v| v.as_str().map(str::trim)) {
            Some(None) => return Err("name 要是一段文字，或者 null 恢复默认。".into()),
            None | Some(Some("")) => writes.push(("site_name", None)),
            Some(Some(n)) if n.chars().count() <= 60 => writes.push(("site_name", Some(json!(n).to_string()))),
            Some(Some(_)) => return Err("站点名称太长：最多 60 个字符。".into()),
        }
    }
    if let Some(m) = a.import_map {
        let m = m.unwrap_or(Value::Null);
        match m {
            Value::Null => writes.push(("import_map", None)),
            Value::Object(ref o) if o.get("imports").is_none_or(Value::is_object) && o.get("scopes").is_none_or(Value::is_object) => {
                writes.push(("import_map", Some(m.to_string())))
            }
            _ => return Err("import_map 要形如 {\"imports\": {\"react\": \"https://esm.sh/react@19\"}}。".into()),
        }
    }
    if let Some(l) = a.script_origins {
        match l.unwrap_or(Value::Null) {
            Value::Null => writes.push(("script_origins", None)),
            Value::Array(items) => {
                let mut list = vec![];
                for o in items {
                    let s = o.as_str().unwrap_or("").trim().trim_end_matches('/').to_string();
                    if !origin_ok(&s) {
                        return Err(format!("script_origins 里的 {o} 不是来源网址：要形如 https://esm.sh，不带路径。"));
                    }
                    list.push(s);
                }
                writes.push(("script_origins", Some(json!(list).to_string())));
            }
            _ => return Err("script_origins 要是一组网址，比如 [\"https://esm.sh\"]。".into()),
        }
    }
    // Drafts like everything else: the live site changes only when a person publishes.
    for (k, v) in writes {
        let target = Target::Setting(k.into());
        match v {
            Some(v) => changes::save_draft(conn, actor, &target, &serde_json::from_str::<Value>(&v).map_err(|e| e.to_string())?, note)?,
            None if changes::target_status(conn, &target)? != "deleted" => changes::mark_deleted(conn, actor, &target, note)?,
            None => {}
        }
    }
    get_site_settings(conn)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetSiteNotes {
    text: String,
    note: String,
}

fn list_views(conn: &Connection) -> Result<Value, String> {
    let fail = |e: rusqlite::Error| format!("读取界面失败：{e}");
    let mut stmt = conn
        .prepare("SELECT name, published, draft, draft_deleted FROM views WHERE published IS NOT NULL OR draft IS NOT NULL ORDER BY name")
        .map_err(fail)?;
    let rows: Vec<(String, Option<String>, Option<String>, bool)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
    let mut out = vec![];
    for (name, published, draft, deleting) in rows {
        let status = changes::status(published.is_some(), draft.is_some(), deleting);
        let json = draft.or(published).unwrap_or_default();
        let def: Value = serde_json::from_str(&json).map_err(|e| format!("界面“{name}”的定义已损坏：{e}"))?;
        out.push(json!({ "definition": def, "status": status }));
    }
    Ok(Value::Array(out))
}

fn set_view(conn: &Connection, actor: &Actor, a: SetView) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    let view = ViewDef {
        name: a.name,
        title: a.title,
        collection: a.collection,
        columns: a.columns,
        filters: a.filters,
        sort: a.sort,
        actions: a.actions,
        layout: a.layout,
        title_field: a.title_field,
        subtitle_field: a.subtitle_field,
        image_field: a.image_field,
        density: a.density,
        group_by: a.group_by,
    };
    let def = changes::effective_def(conn, &view.collection)?;
    check_view(&view, &def)?;
    let target = Target::View(view.name.clone());
    if changes::target_status(conn, &target)? == "deleting" {
        return Err(format!("界面“{}”正等待删除，不能修改：如要保留它，请先对 view:{} 调用 discard_draft 放弃删除。", view.name, view.name));
    }
    changes::save_draft(conn, actor, &target, &json!(view), note)?;
    Ok(json!({ "name": view.name, "status": changes::target_status(conn, &target)? }))
}

fn delete_view(conn: &Connection, actor: &Actor, a: DeleteView) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    let target = Target::parse(&format!("view:{}", a.name))?;
    if changes::target_status(conn, &target)? == "deleted" {
        return Err(format!("界面“{}”不存在或已删除，请先用 list_views 查看现有界面。", a.name));
    }
    changes::mark_deleted(conn, actor, &target, note)?;
    Ok(json!({ "name": a.name, "status": changes::target_status(conn, &target)? }))
}

fn get_site_notes(conn: &Connection) -> Result<Value, String> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row("SELECT published, draft FROM notes WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(|e| format!("读取站点说明失败：{e}"))?;
    let (published, draft) = row.unwrap_or_default();
    let status = if published.is_none() && draft.is_none() { "published" } else { changes::status(published.is_some(), draft.is_some(), false) };
    let text = match draft.or(published) {
        Some(json) => serde_json::from_str::<String>(&json).map_err(|e| format!("站点说明已损坏：{e}"))?,
        None => String::new(),
    };
    Ok(json!({ "text": text, "status": status }))
}

fn set_site_notes(conn: &Connection, actor: &Actor, a: SetSiteNotes) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    let n = a.text.chars().count();
    if n > MAX_NOTES {
        return Err(format!("站点说明太长：最多 {MAX_NOTES} 个字符，现在有 {n} 个，请缩短。"));
    }
    changes::save_draft(conn, actor, &Target::Notes, &json!(a.text), note)?;
    Ok(json!({ "status": changes::target_status(conn, &Target::Notes)? }))
}

// ---- media ----

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UploadMedia {
    /// Only used in error messages; the stored name and type come from the content.
    filename: String,
    data_base64: String,
}

fn upload_media(app: &App, a: UploadMedia) -> Result<Value, String> {
    use base64::Engine;
    // Reject before decoding: base64 is 4/3 the size of the bytes.
    // Some clients wrap base64 at 76 characters; line breaks and spaces are not part of the data.
    let data: String = a.data_base64.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if data.len() > crate::media::MAX_BYTES_INLINE / 3 * 4 + 8 {
        return Err(format!("“{}”太大：通过工具上传最大 {} MB，更大的文件请用 POST /api/v1/media。", a.filename, crate::media::MAX_BYTES_INLINE >> 20));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data)
        .map_err(|e| format!("“{}”的 data_base64 不是有效的 base64：{e}。", a.filename))?;
    if bytes.len() > crate::media::MAX_BYTES_INLINE {
        return Err(format!("“{}”太大：通过工具上传最大 {} MB，更大的文件请用 POST /api/v1/media。", a.filename, crate::media::MAX_BYTES_INLINE >> 20));
    }
    Ok(json!({ "path": crate::media::store_upload(app, &a.filename, &bytes)? }))
}

// ---- site files ----

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SchedulePublish {
    /// When, like "2026-12-01 09:00".
    at: String,
    /// How many minutes the zone of `at` is ahead of UTC (480 for China); 0 when left out.
    tz_offset_minutes: Option<i64>,
    /// Which pending changes, like ["record:3", "file:index.html"]; leave out for everything pending at that time.
    targets: Option<Vec<String>>,
    /// Why: goes into the release history.
    note: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdArgs {
    id: i64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Search {
    /// What to look for: words or Chinese text.
    q: String,
    /// Only this collection.
    collection: Option<String>,
    /// At most this many hits, 1 to 50 (default 20).
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WriteSiteFile {
    path: String,
    content: String,
    note: String,
    /// true only after the user agreed that hand edits from the canvas on elements this write removes or
    /// changes may be dropped.
    #[serde(default)]
    replace_hand_edits: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeleteSiteFile {
    path: String,
    note: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UploadSiteBundle {
    zip_base64: String,
    note: String,
    /// Also delete live files that are not in the bundle.
    #[serde(default)]
    replace: bool,
    /// true only after the user agreed that hand edits from the canvas that no longer match may be dropped.
    #[serde(default)]
    replace_hand_edits: bool,
}

/// (status, current content) of a site file that exists; the draft copy when there is one.
fn site_file(conn: &Connection, path: &str) -> Result<Option<(&'static str, String)>, String> {
    let row: Option<(Option<String>, Option<String>, bool)> = conn
        .query_row("SELECT published, draft, draft_deleted FROM site_files WHERE path = ?1", [path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()
        .map_err(|e| format!("读取站点文件“{path}”失败：{e}"))?;
    let Some((published, draft, deleting)) = row.filter(|(p, d, _)| p.is_some() || d.is_some()) else { return Ok(None) };
    let status = changes::status(published.is_some(), draft.is_some(), deleting);
    let json = draft.or(published).unwrap_or_default();
    let content = serde_json::from_str(&json).map_err(|e| format!("站点文件“{path}”已损坏：{e}"))?;
    Ok(Some((status, content)))
}

fn no_file(path: &str) -> String {
    format!("站点文件“{path}”不存在或已删除，请先用 list_site_files 查看现有文件。")
}

fn list_site_files(conn: &Connection) -> Result<Value, String> {
    let fail = |e: rusqlite::Error| format!("读取站点文件失败：{e}");
    let mut stmt = conn.prepare("SELECT path FROM site_files WHERE published IS NOT NULL OR draft IS NOT NULL ORDER BY path").map_err(fail)?;
    let paths: Vec<String> = stmt.query_map([], |r| r.get(0)).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
    let mut out = vec![];
    for path in paths {
        let Some((status, content)) = site_file(conn, &path)? else { continue };
        if crate::site::is_binary(&path) {
            let bytes = crate::site::decode_binary(&content).map_or(0, |b| b.len());
            out.push(json!({ "path": path, "status": status, "bytes": bytes, "binary": true }));
        } else {
            out.push(json!({ "path": path, "status": status, "bytes": content.len(), "lines": content.lines().count() }));
        }
    }
    Ok(Value::Array(out))
}

fn get_site_file(conn: &Connection, a: PathArgs) -> Result<Value, String> {
    crate::site::check_path(&a.path)?;
    let (status, content) = site_file(conn, &a.path)?.ok_or_else(|| no_file(&a.path))?;
    let mut out = json!({ "path": a.path, "status": status, "content": content, "binary": crate::site::is_binary(&a.path) });
    let edits = crate::canvas::edits_of(&changes::setting(conn, crate::canvas::KEY, true), &a.path);
    if !edits.is_empty() {
        // What the user changed by hand on the canvas, element by element (`<path>#<n>`: the n-th start tag).
        out["hand_edits"] = json!(edits);
    }
    Ok(out)
}

fn file_status(conn: &Connection, path: &str) -> Result<&'static str, String> {
    changes::target_status(conn, &Target::File(path.to_string()))
}

fn write_site_file(conn: &Connection, actor: &Actor, a: WriteSiteFile) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    // Base64 may arrive wrapped in lines; it is stored as one.
    let content = if crate::site::is_binary(&a.path) { a.content.split_whitespace().collect() } else { a.content };
    crate::site::check_file(&a.path, &content)?;
    changes::check_file_count(conn, std::slice::from_ref(&a.path))?;
    let canvas = follow_hand_edits(conn, &[(a.path.clone(), content.clone())], a.replace_hand_edits)?;
    changes::stage_file(conn, actor, &a.path, &content, note)?;
    if let Some(doc) = canvas {
        changes::save_draft(conn, actor, &Target::Setting(crate::canvas::KEY.into()), &doc, note)?;
    }
    Ok(json!({ "path": a.path, "status": file_status(conn, &a.path)? }))
}

/// Hand edits from the canvas follow their elements into new file contents; the AI may not drop them without
/// the user's say (`replace`). Returns the canvas document to save when it changed.
fn follow_hand_edits(conn: &Connection, files: &[(String, String)], replace: bool) -> Result<Option<Value>, String> {
    let canvas = changes::setting(conn, crate::canvas::KEY, true);
    let mut doc = canvas.clone();
    let mut lost = vec![];
    for (path, content) in files.iter().filter(|(p, _)| p.ends_with(".html")) {
        let old = crate::site::file(conn, crate::site::Mode::Preview, path)?.unwrap_or_default();
        let (moved, gone) = crate::canvas::remap(&doc, path, &old, content);
        doc = moved;
        lost.extend(gone);
    }
    if !lost.is_empty() && !replace {
        let what: Vec<String> = lost.iter().map(|k| format!("{k}（{}）", canvas["els"][k]["fp"].as_str().unwrap_or_default())).collect();
        return Err(format!(
            "没有写入：用户在画板上手动调整过这些元素，这样改写后它们就对不上了：{}。先告诉用户这些手动调整会丢掉，用户同意后带上 replace_hand_edits: true 再写；或者保留这些元素（标签、id、class 和开头的文字不变）。",
            what.join("、")
        ));
    }
    Ok((doc != canvas).then_some(doc))
}

fn delete_site_file(conn: &Connection, actor: &Actor, a: DeleteSiteFile) -> Result<Value, String> {
    let note = check_note(&a.note)?;
    crate::site::check_path(&a.path)?;
    if site_file(conn, &a.path)?.is_none() || file_status(conn, &a.path)? == "deleting" {
        return Err(no_file(&a.path));
    }
    changes::mark_deleted(conn, actor, &Target::File(a.path.clone()), note)?;
    Ok(json!({ "path": a.path, "status": file_status(conn, &a.path)? }))
}

fn upload_site_bundle(conn: &Connection, actor: &Actor, a: UploadSiteBundle) -> Result<Value, String> {
    use base64::Engine;
    let note = check_note(&a.note)?;
    let data: String = a.zip_base64.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    // The archive itself can be no bigger than what it may unpack to.
    if data.len() > crate::site::MAX_BUNDLE_BYTES / 3 * 4 + 8 {
        return Err("压缩包太大：最多 50 MB。".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(&data).map_err(|e| format!("zip_base64 不是有效的 base64：{e}。"))?;
    let files = crate::site::read_bundle(&bytes)?;
    if files.is_empty() {
        return Err("压缩包里没有文件。".into());
    }
    let paths: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
    let canvas = follow_hand_edits(conn, &files, a.replace_hand_edits)?;
    let fail = |e: rusqlite::Error| format!("写入站点文件失败：{e}");
    let tx = conn.unchecked_transaction().map_err(fail)?;
    let mut deleted = 0;
    if a.replace {
        let live: Vec<String> = tx
            .prepare("SELECT path FROM site_files WHERE published IS NOT NULL AND draft_deleted = 0")
            .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
            .map_err(fail)?;
        for path in live.into_iter().filter(|p| !paths.contains(p)) {
            changes::mark_deleted_in(&tx, actor, &Target::File(path), note)?;
            deleted += 1;
        }
    }
    changes::check_file_count(&tx, &paths)?;
    let mut written = 0;
    for (path, content) in &files {
        written += changes::stage_file(&tx, actor, path, content, note)? as usize;
    }
    if let Some(doc) = canvas {
        changes::save_draft(&tx, actor, &Target::Setting(crate::canvas::KEY.into()), &doc, note)?;
    }
    tx.commit().map_err(fail)?;
    Ok(json!({ "files": files.len(), "written": written, "unchanged": files.len() - written, "deleted": deleted }))
}
