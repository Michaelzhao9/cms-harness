//! Server functions: the back end the harness writes. A site file under a `_fn/` folder (`_fn/add.js`,
//! `teacher/_fn/grade.js`) is a script the server runs when its address (`/_fn/add`, `/teacher/_fn/grade`) is
//! asked for. Being a site file it is a draft until published, shows up in the publish review and can be restored
//! like any other.
//!
//! ```js
//! export default function (req) {          // req: method, path, query, body, member
//!   const id = db.create("orders", { item: req.body.item });
//!   return { id };                          // or reply(403, { error: "…" })
//! }
//! ```
//!
//! What keeps it in its box:
//! - The script runs in QuickJS on a thread of its own and has no file, network, process or timer. The only thing
//!   it can do to the outside is ask this thread, by message, to read or write records.
//! - Time, memory and stack are capped, and so is the number of data calls.
//! - One run is one transaction: an exception, a cut-off, or an answer of 400 and up, and nothing it wrote is kept.
//! - Every record it writes passes the same checks as any other write and goes into history under `fn:<file>`.
//!
//! A function is trusted code: a builder (or the model) wrote it and a person published it. So it reads and writes
//! any collection, whatever the collection's own access rules say: it is where rules too involved for an
//! expression are written. An 端 only members may enter covers its functions too.

use crate::schema::{fill_defaults, normalize, project, CollectionDef};
use crate::site::Mode;
use crate::{access, changes, query, App};
use rquickjs::{Context, Ctx, Function, Module, Promise, Runtime};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS fn_log(id INTEGER PRIMARY KEY, at TEXT NOT NULL DEFAULT (datetime('now')), path TEXT NOT NULL,
  method TEXT NOT NULL, status INTEGER NOT NULL, ms INTEGER NOT NULL, error TEXT, logs TEXT NOT NULL);
";

/// The folder functions live in, at the site's root or inside any folder.
pub const DIR: &str = "_fn";
/// One run may take this long, the data calls it makes included.
pub const TIME: Duration = Duration::from_millis(500);
const MEMORY: usize = 32 * 1024 * 1024;
const STACK: usize = 512 * 1024;
/// Data calls one run may make.
const MAX_CALLS: u32 = 200;
/// How many messages one run may send to the builder's 通知 destinations.
const MAX_NOTIFY: u32 = 5;
/// The request body a function takes, and the answer it may give.
pub const MAX_BODY: usize = 64 * 1024;
const MAX_REPLY: usize = 1024 * 1024;
const MAX_LOG_LINES: usize = 50;
/// Runs kept in the log.
const LOG_ROWS: i64 = 500;

/// What the visitor sees when a function fails: the script's own words are for the log.
pub const FAILED: &str = "这个功能出错了，请稍后再试。";

/// Whether a site path (no leading slash) is inside a functions folder.
pub fn is_fn_path(rel: &str) -> bool {
    rel.split('/').any(|seg| seg == DIR)
}

/// The script behind an address: `/teacher/_fn/grade` is `teacher/_fn/grade.js` (or `.ts`). `None` when the
/// address names no function, or names a file (`…/grade.js`): the source is never an address.
pub fn file_of(conn: &Connection, mode: Mode, path: &str) -> Option<(String, String)> {
    let rel = path.trim_matches('/');
    if !is_fn_path(rel) || rel.rsplit('/').next().is_none_or(|last| last.contains('.') || last == DIR) {
        return None;
    }
    ["js", "mjs", "ts"].iter().find_map(|ext| {
        let name = format!("{rel}.{ext}");
        crate::site::check_path(&name).ok()?;
        crate::site::file(conn, mode, &name).ok().flatten().map(|src| (name, src))
    })
}

/// One request to a function.
pub struct Call {
    pub method: String,
    /// The address as asked for: `/teacher/_fn/grade`.
    pub path: String,
    pub query: Vec<(String, String)>,
    /// The parsed body: JSON as it is, a form as an object of text, nothing as null.
    pub body: Value,
    /// The logged-in member (`id`, `email`, `name`), if any.
    pub member: Option<Value>,
}

/// How one run ended.
pub struct Outcome {
    pub status: u16,
    pub body: Value,
    /// What the script wrote with `console.log`.
    pub logs: Vec<String>,
    /// Why it failed, in the script's own words: for the builder, never for the visitor.
    pub error: Option<String>,
    pub ms: u64,
}

/// What the script's thread sends to this one.
enum ToHost {
    /// A data call: its name and its arguments as JSON. Answered with `{"ok": …}` or `{"error": "…"}`.
    Data(String, String),
    Log(String),
    /// The run is over: the answer as JSON, or what went wrong.
    Done(Result<String, String>),
}

/// What a script sees besides the language itself.
const PRELUDE: &str = r#"
globalThis.__call = (op, args) => { const r = JSON.parse(__host(op, JSON.stringify(args))); if (r.error) throw new Error(r.error); return r.ok; };
globalThis.db = Object.freeze({
  query: (collection, options) => __call('query', [collection, options || {}]),
  aggregate: (collection, options) => __call('aggregate', [collection, options || {}]),
  get: (collection, id) => __call('get', [collection, id]),
  create: (collection, data) => __call('create', [collection, data]),
  update: (collection, id, data) => __call('update', [collection, id, data]),
  delete: (collection, id) => __call('delete', [collection, id]),
});
globalThis.reply = (status, body) => ({ __reply: true, status, body });
globalThis.notify = (text) => __call('notify', [String(text)]);
{
  const line = (...parts) => __log(parts.map(p => typeof p === 'string' ? p : JSON.stringify(p)).join(' '));
  globalThis.console = Object.freeze({ log: line, info: line, warn: line, error: line });
}
globalThis.__run = async (handler, req) => JSON.stringify((await handler(req)) ?? null);
"#;

/// An engine error as words: the exception the script threw, or what the engine says.
fn explain(ctx: &Ctx<'_>, e: rquickjs::Error) -> String {
    let said = if matches!(e, rquickjs::Error::Exception) {
        let thrown = ctx.catch();
        match thrown.get::<rquickjs::Exception>() {
            Ok(ex) => format!("{}\n{}", ex.message().unwrap_or_default(), ex.stack().unwrap_or_default()).trim().to_string(),
            Err(_) => ctx.json_stringify(thrown).ok().flatten().and_then(|s| s.to_string().ok()).unwrap_or_default(),
        }
    } else {
        e.to_string()
    };
    if said.contains("interrupted") {
        format!("运行超过了时间上限（{} 毫秒），已经中止。", TIME.as_millis())
    } else if said.contains("out of memory") || said.is_empty() || said == "null" {
        format!("用掉的内存超过了上限（{} MB），已经中止。", MEMORY / 1024 / 1024)
    } else if matches!(e, rquickjs::Error::WouldBlock) {
        "函数返回了一个不会完成的 Promise。".into()
    } else {
        said.chars().take(2000).collect()
    }
}

/// The script's thread: build the box, run the function, report.
fn engine(name: String, source: String, req: String, deadline: Instant, to_host: Sender<ToHost>, answers: Receiver<String>) {
    let run = || -> Result<String, String> {
        let rt = Runtime::new().map_err(|e| e.to_string())?;
        rt.set_memory_limit(MEMORY);
        rt.set_max_stack_size(STACK);
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
        let ctx = Context::full(&rt).map_err(|e| e.to_string())?;
        ctx.with(|ctx| {
            let (data, log) = (to_host.clone(), to_host.clone());
            let inner = || -> rquickjs::Result<String> {
                let host = Function::new(ctx.clone(), move |op: String, args: String| -> String {
                    let _ = data.send(ToHost::Data(op, args));
                    answers.recv().unwrap_or_else(|_| json!({ "error": "这次运行已经结束。" }).to_string())
                })?;
                ctx.globals().set("__host", host)?;
                ctx.globals().set("__log", Function::new(ctx.clone(), move |line: String| drop(log.send(ToHost::Log(line))))?)?;
                ctx.eval::<(), _>(PRELUDE)?;
                let (module, loaded) = Module::declare(ctx.clone(), name, source)?.eval()?;
                loaded.finish::<rquickjs::Value>()?;
                let handler: rquickjs::Value = module.get("default")?;
                if !handler.is_function() {
                    return Err(rquickjs::Exception::throw_type(&ctx, "函数文件要有默认导出：export default function (req) { … }"));
                }
                let start: Function = ctx.globals().get("__run")?;
                let promise: Promise = start.call((handler, ctx.json_parse(req)?))?;
                promise.finish::<String>()
            };
            inner().map_err(|e| explain(&ctx, e))
        })
    };
    let _ = to_host.send(ToHost::Done(run()));
}

/// The definition a function works against: what is live, or in a trial run what the drafts say.
fn def_of(conn: &Connection, name: &str, dry: bool) -> Result<CollectionDef, String> {
    if dry { changes::effective_def(conn, name) } else { changes::live_def(conn, name) }
}

fn db(e: rusqlite::Error) -> String {
    format!("读取数据库失败：{e}")
}

fn id_of(v: &Value) -> Result<i64, String> {
    v.as_i64().ok_or_else(|| format!("记录的编号要是整数，收到的是 {v}。"))
}

/// A live record of `def` as stored (the whole document, translations included).
fn stored(conn: &Connection, def: &CollectionDef, id: i64) -> Result<Option<(String, Option<String>)>, String> {
    conn.query_row("SELECT published, draft FROM records WHERE id = ?1 AND collection = ?2 AND published IS NOT NULL", params![id, def.name], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(db)
}

/// One data call from the script. Writes are live at once, checked like any write, and recorded under `actor`.
fn data_call(conn: &Connection, op: &str, args: &Value, dry: bool, actor: &str, owner: Option<i64>) -> Result<Value, String> {
    let collection = args[0].as_str().ok_or("第一个参数是集合的名字。")?;
    let def = def_of(conn, collection, dry)?;
    match op {
        "query" => {
            let o = &args[1];
            let q = query::Query {
                filters: o.get("filter").filter(|f| !f.is_null()).map(|f| query::parse_filter(&def, f)).transpose()?.unwrap_or_default(),
                status: vec![],
                sort: o["sort"].as_str().map(|s| match s.strip_prefix('-') {
                    Some(f) => (f.to_string(), true),
                    None => (s.to_string(), false),
                }),
                limit: o["limit"].as_i64().unwrap_or(20),
                offset: o["offset"].as_i64().unwrap_or(0),
                owner: o["owner"].as_i64(),
                search: o["search"].as_str().map(String::from),
                rule: None,
            };
            let (rows, total) = query::run(conn, &def, &q, true)?;
            let items: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    let mut item = r.doc;
                    item["id"] = json!(r.id);
                    item
                })
                .collect();
            Ok(json!({ "items": items, "total": total }))
        }
        "aggregate" => {
            let o = &args[1];
            let names = |key: &str| o[key].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>();
            let q = query::Query {
                filters: o.get("filter").filter(|f| !f.is_null()).map(|f| query::parse_filter(&def, f)).transpose()?.unwrap_or_default(),
                status: vec![],
                sort: None,
                limit: 1,
                offset: 0,
                owner: o["owner"].as_i64(),
                search: o["search"].as_str().map(String::from),
                rule: None,
            };
            let t = query::Totals { group_by: o["group_by"].as_str().map(String::from), sum: names("sum"), avg: names("avg"), min: names("min"), max: names("max") };
            query::totals(conn, &def, &q, true, &t)
        }
        "get" => Ok(match stored(conn, &def, id_of(&args[1])?)? {
            Some((text, _)) => {
                let mut item = project(&def, &serde_json::from_str(&text).unwrap_or(Value::Null));
                item["id"] = args[1].clone();
                item
            }
            None => Value::Null,
        }),
        "create" => {
            let doc = access::clean(&def, fill_defaults(&def, normalize(&def, &json!({}), &args[1])?)?);
            access::unique(conn, &def, None, &doc)?;
            access::check_live_relations(conn, &def, &doc)?;
            conn.execute("INSERT INTO records(collection, published, owner, submitted_at) VALUES (?1, ?2, ?3, datetime('now'))", params![def.name, doc.to_string(), owner]).map_err(db)?;
            let id = conn.last_insert_rowid();
            crate::routes::assign(conn, &def, id, &doc)?;
            access::history(conn, actor, id, Some(&doc.to_string()), None, "函数写入")?;
            Ok(json!(id))
        }
        "update" => {
            let id = id_of(&args[1])?;
            let (text, draft) = stored(conn, &def, id)?.ok_or_else(|| format!("集合“{collection}”里没有线上的记录 {id}。"))?;
            let old: Value = serde_json::from_str(&text).map_err(|e| format!("记录 {id} 的内容已损坏：{e}"))?;
            let doc = access::clean(&def, normalize(&def, &old, &args[2])?);
            access::unique(conn, &def, Some(id), &doc)?;
            access::check_live_relations(conn, &def, &doc)?;
            // A draft the admin has pending gets the same fields, so publishing it does not undo this.
            let pending = draft.and_then(|d| serde_json::from_str::<Value>(&d).ok()).map(|mut d| {
                if let (Some(d), Some(sent)) = (d.as_object_mut(), args[2].as_object()) {
                    d.extend(sent.keys().map(|k| (k.clone(), doc[k].clone())));
                }
                d.to_string()
            });
            conn.execute("UPDATE records SET published = ?2, draft = coalesce(?3, draft), updated_at = datetime('now') WHERE id = ?1", params![id, doc.to_string(), pending]).map_err(db)?;
            access::history(conn, actor, id, Some(&doc.to_string()), Some(&text), "函数修改")?;
            Ok(json!(id))
        }
        "delete" => {
            let id = id_of(&args[1])?;
            let (text, _) = stored(conn, &def, id)?.ok_or_else(|| format!("集合“{collection}”里没有线上的记录 {id}。"))?;
            if access::pointed_at(conn, collection, id)? {
                return Err(format!("记录 {id} 还被别的内容用着，不能删除。"));
            }
            conn.execute("UPDATE records SET published = NULL, updated_at = datetime('now') WHERE id = ?1", [id]).map_err(db)?;
            access::history(conn, actor, id, None, Some(&text), "函数删除")?;
            Ok(json!(id))
        }
        other => Err(format!("没有叫 db.{other} 的操作。")),
    }
}

/// Runs the function in `source` (the site file `name`) for one request. `dry`: a trial run against the drafts'
/// definitions that keeps nothing it writes.
pub fn run(conn: &Connection, name: &str, source: &str, call: &Call, dry: bool) -> Outcome {
    let started = Instant::now();
    let failed = |error: String, logs: Vec<String>| Outcome { status: 500, body: Value::Null, logs, error: Some(error), ms: started.elapsed().as_millis() as u64 };
    // TypeScript is turned into JavaScript the way the site's own scripts are.
    let source = if name.ends_with(".ts") {
        match crate::site::transpile(name, source) {
            Ok(js) => js,
            Err(e) => return failed(e, vec![]),
        }
    } else {
        source.to_string()
    };
    let query: serde_json::Map<String, Value> = call.query.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let req = json!({ "method": call.method, "path": call.path, "query": query, "body": call.body, "member": call.member }).to_string();
    let owner = call.member.as_ref().and_then(|m| m["id"].as_i64());
    let actor = format!("fn:{}", name.rsplit_once('.').map_or(name, |(stem, _)| stem));

    if let Err(e) = conn.execute_batch("SAVEPOINT hc_fn") {
        return failed(db(e), vec![]);
    }
    let (to_host, from_script) = channel();
    let (answer, answers) = channel();
    let deadline = started + TIME;
    let script = name.to_string();
    let spawned = std::thread::Builder::new().name("hc-fn".into()).stack_size(4 * STACK).spawn(move || engine(script, source, req, deadline, to_host, answers));
    let (mut logs, mut calls, mut told) = (vec![], 0, 0);
    let result = match spawned {
        Err(e) => Err(format!("启动运行环境失败：{e}")),
        Ok(_) => loop {
            // The script's own clock stops it; this is for a thread that died or will not come back.
            match from_script.recv_timeout(TIME + Duration::from_secs(2)) {
                Ok(ToHost::Log(line)) => {
                    if logs.len() < MAX_LOG_LINES {
                        logs.push(line.chars().take(500).collect());
                    }
                }
                Ok(ToHost::Data(op, args)) => {
                    calls += 1;
                    let out = if calls > MAX_CALLS {
                        Err(format!("这次运行读写数据超过了 {MAX_CALLS} 次：把查询合并，或者分几次请求来做。"))
                    } else if Instant::now() > deadline {
                        Err(format!("运行超过了时间上限（{} 毫秒），已经中止。", TIME.as_millis()))
                    } else if op == "notify" {
                        // Written down inside the savepoint: a run that does not count tells nobody.
                        told += 1;
                        let text = serde_json::from_str::<Value>(&args).ok().and_then(|a| a[0].as_str().map(String::from)).unwrap_or_default();
                        if told > MAX_NOTIFY {
                            Err(format!("一次运行最多发 {MAX_NOTIFY} 条通知。"))
                        } else {
                            crate::hooks::fire(conn, "notify", &text, json!({ "function": call.path }));
                            if dry && logs.len() < MAX_LOG_LINES {
                                logs.push(format!("notify（试运行，不会真的发）：{}", text.chars().take(400).collect::<String>()));
                            }
                            Ok(Value::Null)
                        }
                    } else {
                        serde_json::from_str::<Value>(&args).map_err(|e| e.to_string()).and_then(|a| data_call(conn, &op, &a, dry, &actor, owner))
                    };
                    let reply = match out {
                        Ok(v) => json!({ "ok": v }),
                        Err(e) => json!({ "error": e }),
                    };
                    if answer.send(reply.to_string()).is_err() {
                        break Err("运行环境意外结束了。".to_string());
                    }
                }
                Ok(ToHost::Done(r)) => break r,
                Err(RecvTimeoutError::Timeout) => break Err(format!("运行超过了时间上限（{} 毫秒），已经中止。", TIME.as_millis())),
                Err(RecvTimeoutError::Disconnected) => break Err("运行环境意外结束了。".to_string()),
            }
        },
    };
    let mut out = match result {
        Err(e) => failed(e, logs),
        Ok(text) if text.len() > MAX_REPLY => failed(format!("返回的内容太大：最多 {} KB。", MAX_REPLY / 1024), logs),
        Ok(text) => {
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let (status, body) = match v.get("__reply").and_then(Value::as_bool) {
                Some(true) => (v["status"].as_u64().filter(|s| (200..600).contains(s)).unwrap_or(200) as u16, v["body"].clone()),
                _ => (200, v),
            };
            Outcome { status, body, logs, error: None, ms: 0 }
        }
    };
    // Kept only when the run ended well and is not a trial.
    let keep = !dry && out.error.is_none() && out.status < 400;
    let end = if keep { "RELEASE hc_fn" } else { "ROLLBACK TO hc_fn; RELEASE hc_fn" };
    if let Err(e) = conn.execute_batch(end) {
        out = failed(db(e), std::mem::take(&mut out.logs));
    }
    out.ms = started.elapsed().as_millis() as u64;
    out
}

/// Writes a run down when there is something to see: it failed, or the script logged.
pub fn record(conn: &Connection, call: &Call, out: &Outcome) {
    if out.error.is_none() && out.logs.is_empty() {
        return;
    }
    let _ = conn.execute(
        "INSERT INTO fn_log(path, method, status, ms, error, logs) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![call.path, call.method, out.status, out.ms as i64, out.error, json!(out.logs).to_string()],
    );
    let _ = conn.execute("DELETE FROM fn_log WHERE id <= (SELECT max(id) FROM fn_log) - ?1", [LOG_ROWS]);
}

/// `function_log`: the newest runs that failed or logged, of one function or of all.
pub fn log(conn: &Connection, path: Option<&str>, limit: i64) -> Result<Value, String> {
    let mut st = conn.prepare("SELECT at, path, method, status, ms, error, logs FROM fn_log WHERE (?1 IS NULL OR path = ?1) ORDER BY id DESC LIMIT ?2").map_err(db)?;
    let rows = st
        .query_map(params![path, limit.clamp(1, 100)], |r| {
            Ok(json!({ "at": r.get::<_, String>(0)?, "path": r.get::<_, String>(1)?, "method": r.get::<_, String>(2)?, "status": r.get::<_, i64>(3)?, "ms": r.get::<_, i64>(4)?,
                "error": r.get::<_, Option<String>>(5)?, "logs": serde_json::from_str::<Value>(&r.get::<_, String>(6)?).unwrap_or(Value::Null) }))
        })
        .map_err(db)?;
    Ok(Value::Array(rows.collect::<Result<_, _>>().map_err(db)?))
}

/// `run_function`: a trial run of the draft at `path`. Nothing it writes is kept.
pub fn trial(_app: &App, conn: &Connection, path: &str, method: Option<&str>, query: Option<&Value>, body: Option<Value>, member_id: Option<i64>) -> Result<Value, String> {
    if !path.starts_with('/') {
        return Err("path 是函数的网址，以 / 开头：_fn/add.js 的网址是 /_fn/add。".into());
    }
    let Some((name, source)) = file_of(conn, Mode::Preview, path) else {
        return Ok(json!({ "path": path, "status": 404, "dry_run": true, "error": "草稿里没有这个函数：网址 /_fn/add 对应文件 _fn/add.js，先用 write_site_file 写出它。" }));
    };
    let member = match member_id {
        Some(id) => Some(crate::members::by_id(conn, id).map(|m| crate::members::member_json(&m)).ok_or_else(|| format!("没有编号为 {id} 的会员（或者已停用）：用 list_members 看有哪些。"))?),
        None => None,
    };
    let query = query.and_then(Value::as_object).map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), String::from))).collect()).unwrap_or_default();
    let call = Call { method: method.unwrap_or("GET").to_ascii_uppercase(), path: path.to_string(), query, body: body.unwrap_or(Value::Null), member };
    let out = run(conn, &name, &source, &call, true);
    Ok(json!({ "path": path, "file": name, "status": out.status, "body": out.body, "logs": out.logs, "error": out.error, "ms": out.ms, "dry_run": true }))
}
