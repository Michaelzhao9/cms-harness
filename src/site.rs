//! Site files: the built-in website's sources. Path rules, the checks a file passes on write, the script
//! transpiler, and the page render that publish verification and the site server share.

use crate::schema::{CollectionDef, FieldType};
use base64::Engine;
use minijinja::value::{Enumerator, Kwargs, Object, ObjectRepr};
use minijinja::{Environment, ErrorKind, UndefinedBehavior};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::sync::{mpsc, Arc, Mutex};

/// Text extensions a site file may have.
pub const EXTENSIONS: [&str; 14] = ["html", "css", "js", "mjs", "jsx", "ts", "tsx", "json", "svg", "txt", "md", "xml", "webmanifest", "map"];
/// Binary extensions: stored as base64, served as bytes.
pub const BINARY_EXTENSIONS: [&str; 13] = ["png", "jpg", "jpeg", "gif", "webp", "avif", "ico", "woff", "woff2", "ttf", "otf", "mp4", "webm"];
/// Path prefixes the server's own routes use; `mcp` is reserved as a whole path.
const RESERVED: [&str; 4] = ["admin/", "api/", "uploads/", "_hc/"];
pub const MAX_PATH: usize = 200;
pub const MAX_FILE_BYTES: usize = 1 << 20;
/// A binary file's decoded size: enough for fonts, images and short clips.
pub const MAX_BINARY_BYTES: usize = 5 << 20;
pub const MAX_FILES: usize = 2_000;
/// Uncompressed total of one bundle.
pub const MAX_BUNDLE_BYTES: usize = 50 << 20;

fn ext(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, e)| e)
}

fn is_script(path: &str) -> bool {
    matches!(ext(path), "js" | "mjs" | "jsx" | "ts" | "tsx")
}

pub fn is_binary(path: &str) -> bool {
    BINARY_EXTENSIONS.contains(&ext(path))
}

/// The `Content-Type` a site file is served with.
pub fn content_type(path: &str) -> &'static str {
    match ext(path) {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" | "jsx" | "ts" | "tsx" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "webmanifest" => "application/manifest+json; charset=utf-8",
        "svg" => "image/svg+xml; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

/// The bytes of a binary file's stored base64.
pub fn decode_binary(content: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(content).ok()
}

/// Relative, `[A-Za-z0-9._/-]` (case-sensitive), no `..`, no empty or `.` segment, at most 200 characters, not a
/// reserved route, an allowed extension.
pub fn check_path(path: &str) -> Result<(), String> {
    let name = path.rsplit('/').next().unwrap_or_default();
    let why = if path.is_empty() {
        "不能为空".to_string()
    } else if path.len() > MAX_PATH {
        format!("最长 {MAX_PATH} 个字符，现在有 {} 个", path.len())
    } else if path.starts_with('/') {
        "必须是相对路径，不能以 / 开头".into()
    } else if !path.bytes().all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b)) {
        "只能用字母、数字和 . _ / - 这几个符号（区分大小写）".into()
    } else if path.contains("..") {
        "不能包含 ..".into()
    } else if path.split('/').any(|s| s.is_empty() || s == ".") {
        "不能有空的路径段或只有 . 的路径段，也不能以 / 结尾".into()
    } else if path == "mcp" || RESERVED.iter().any(|r| path.starts_with(r)) {
        "admin/、api/、uploads/、_hc/ 开头的路径和 mcp 留给系统自己的页面和接口".into()
    } else if !matches!(name.rsplit_once('.'), Some((stem, e)) if !stem.is_empty() && (EXTENSIONS.contains(&e) || BINARY_EXTENSIONS.contains(&e))) {
        format!("扩展名必须是 {} 之一（文本），或 {} 之一（二进制，内容用 base64）", EXTENSIONS.join("、"), BINARY_EXTENSIONS.join("、"))
    } else {
        return Ok(());
    };
    Err(format!("站点文件路径“{path}”不合法：{why}。"))
}

/// The checks every stored file passes: its path, its size, and that it parses as what its extension says.
/// A binary file's content is its bytes in standard base64.
pub fn check_file(path: &str, content: &str) -> Result<(), String> {
    check_path(path)?;
    if is_binary(path) {
        let bytes = decode_binary(content).ok_or_else(|| format!("站点文件“{path}”是二进制文件，内容必须是标准 base64 编码。"))?;
        if bytes.is_empty() {
            return Err(format!("站点文件“{path}”是空文件：请上传有内容的文件。"));
        }
        if bytes.len() > MAX_BINARY_BYTES {
            return Err(format!("站点文件“{path}”太大：二进制文件最多 5 MB（{MAX_BINARY_BYTES} 字节），现在有 {} 字节。", bytes.len()));
        }
        return Ok(());
    }
    if content.len() > MAX_FILE_BYTES {
        return Err(format!("站点文件“{path}”太大：每个文件最多 1 MB（{MAX_FILE_BYTES} 字节），现在有 {} 字节。", content.len()));
    }
    match ext(path) {
        "html" => syntax_env().template_from_named_str(path, content).map(|_| ()).map_err(|e| format!("站点文件“{path}”的模板写错了：{}", template_error(&e))),
        "json" | "webmanifest" | "map" => serde_json::from_str::<serde::de::IgnoredAny>(content)
            .map(|_| ())
            .map_err(|e| format!("站点文件“{path}”不是有效的 JSON：第 {} 行第 {} 列，{e}。", e.line(), e.column())),
        _ if is_script(path) => transpile(path, content).map(|_| ()),
        _ => Ok(()),
    }
}

/// 1-based line and column (in characters) of a byte offset.
fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let before = source.get(..offset.min(source.len())).unwrap_or(source);
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    (before.matches('\n').count() + 1, before[line_start..].chars().count() + 1)
}

fn script_error(path: &str, source: &str, errors: &[oxc::diagnostics::OxcDiagnostic]) -> String {
    let first = &errors[0];
    let at = first.labels.first().map(|l| line_col(source, l.offset() as usize));
    let at = at.map_or_else(String::new, |(l, c)| format!("第 {l} 行第 {c} 列，"));
    let more = if errors.len() > 1 { format!("（另有 {} 处错误）", errors.len() - 1) } else { String::new() };
    format!("站点文件“{path}”的脚本写错了：{at}{}{more}。", first.message)
}

/// A script as the browser runs it: TypeScript types stripped, JSX compiled for the automatic runtime
/// (imports from `react/jsx-runtime`). `.js` and `.mjs` come back unchanged once they parse.
pub fn transpile(path: &str, source: &str) -> Result<String, String> {
    use oxc::{allocator::Allocator, codegen::Codegen, diagnostics::Severity, parser::Parser, semantic::SemanticBuilder, span::SourceType};
    use oxc::transformer::{TransformOptions, Transformer};
    let source_type = SourceType::from_path(path).map_err(|_| format!("“{path}”不是脚本文件。"))?;
    // A `.js` file may be a classic script (a vendor library); everything else is served as a module.
    let source_type = if ext(path) == "js" { source_type.with_unambiguous(true) } else { source_type.with_module(true) };
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(script_error(path, source, &parsed.diagnostics));
    }
    if matches!(ext(path), "js" | "mjs") {
        return Ok(source.to_string());
    }
    let mut program = parsed.program;
    let scoping = SemanticBuilder::new().build(&program).semantic.into_scoping();
    let done = Transformer::new(&allocator, std::path::Path::new(path), &TransformOptions::default()).build_with_scoping(scoping, &mut program);
    let errors: Vec<_> = done.diagnostics.into_iter().filter(|d| d.severity == Severity::Error).collect();
    if !errors.is_empty() {
        return Err(script_error(path, source, &errors));
    }
    Ok(Codegen::new().build(&program).code)
}

// ---- pages ----

/// Which copy of the site a request sees: the live one, or every draft laid over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Live,
    Preview,
}

/// Instruction budget of one page render (minijinja fuel). A page that loops over 100 records with a dozen
/// fields spends a few tens of thousands; this stops a runaway loop after well under a second.
pub const FUEL: u64 = 2_000_000;
/// Content and file lookups one render may make.
const MAX_ASKS: usize = 1_000;

/// Built-in templates for routes no site file serves. `_hc/` paths are reserved, so a site file cannot shadow them.
const FALLBACKS: [(&str, &str); 6] = [
    ("_hc/account.html", include_str!("web/templates/site/account.html")),
    ("_hc/layout.html", include_str!("web/templates/site/layout.html")),
    ("_hc/home.html", include_str!("web/templates/site/home.html")),
    ("_hc/list.html", include_str!("web/templates/site/list.html")),
    ("_hc/item.html", include_str!("web/templates/site/item.html")),
    ("_hc/404.html", include_str!("web/templates/site/404.html")),
];

/// The environment used to check a template's syntax on write: nothing is loaded or run.
fn syntax_env() -> Environment<'static> {
    Environment::new()
}

fn template_error(e: &minijinja::Error) -> String {
    if e.kind() == ErrorKind::OutOfFuel {
        return format!("页面渲染超过了步骤上限（{FUEL} 步）：可能有死循环，或循环套得太深。");
    }
    let at = match (e.name(), e.line()) {
        (Some(n), Some(l)) => format!("{n} 第 {l} 行，"),
        (None, Some(l)) => format!("第 {l} 行，"),
        _ => String::new(),
    };
    let detail = e.detail().map_or_else(|| e.kind().to_string(), String::from);
    format!("{at}{detail}。")
}

/// A site file's content as `mode` sees it; `None` when it does not exist there.
pub fn file(conn: &Connection, mode: Mode, path: &str) -> Result<Option<String>, String> {
    let row: Option<(Option<String>, Option<String>, bool)> = conn
        .query_row("SELECT published, draft, draft_deleted FROM site_files WHERE path = ?1", [path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()
        .map_err(|e| format!("读取站点文件“{path}”失败：{e}"))?;
    let Some((published, draft, deleting)) = row else { return Ok(None) };
    let json = match mode {
        Mode::Live => published,
        Mode::Preview if deleting => None,
        Mode::Preview => draft.or(published),
    };
    json.map(|j| serde_json::from_str(&j).map_err(|e| format!("站点文件“{path}”已损坏：{e}"))).transpose()
}

/// A collection's definition as `mode` sees it, with what `viewer` may read of it (`None` = every record,
/// `Some(id)` = member `id`'s own), or `None` when the viewer may read nothing.
pub fn visible_def(conn: &Connection, mode: Mode, name: &str, viewer: crate::access::Viewer) -> Result<Option<(CollectionDef, Option<i64>)>, String> {
    Ok(any_def(conn, mode, name)?.and_then(|d| crate::access::read_filter(&d.access, viewer).map(|owner| (d, owner))))
}

/// A collection's definition as `mode` sees it, whoever may read it.
fn any_def(conn: &Connection, mode: Mode, name: &str) -> Result<Option<CollectionDef>, String> {
    if !crate::schema::valid_name(name) {
        return Ok(None);
    }
    let def = match mode {
        Mode::Live => crate::changes::live_def(conn, name).ok(),
        Mode::Preview => crate::changes::collection_view(conn, name)?.filter(|(_, st)| *st != "deleting").map(|(d, _)| d),
    };
    Ok(def)
}

/// What a render asks of the thread that holds the database connection.
enum Ask {
    File(String),
    Records { collection: String, filter: Value, sort: Option<String>, limit: i64, offset: i64, id: Option<i64> },
}

enum Answer {
    File(Option<String>),
    Records(Vec<Rec>),
}

/// One record for a template: (field, value, is rich text, change mark).
struct Rec(Vec<(String, Value, bool, Option<usize>)>);

type Line = (Ask, mpsc::Sender<Result<Answer, String>>);

/// The connection side of a render: answers file and content lookups in the render's mode, and in preview
/// knows which pending change each object belongs to.
struct Server<'c> {
    conn: &'c Connection,
    mode: Mode,
    page: String,
    /// Pending target → its 1-based position in `list_pending`; loaded on first need.
    order: Option<HashMap<String, usize>>,
    /// Marks of changed site files the render loaded.
    files_changed: Vec<usize>,
    asks: usize,
    /// Whose access rules apply: the logged-in member on the live site, else an anonymous visitor.
    viewer: crate::access::Viewer,
}

impl Server<'_> {
    fn mark(&mut self, target: &str) -> Result<Option<usize>, String> {
        if self.mode == Mode::Live {
            return Ok(None);
        }
        if self.order.is_none() {
            let order = crate::changes::pending_order(self.conn)?;
            self.order = Some(order.into_iter().enumerate().map(|(i, t)| (t, i + 1)).collect());
        }
        Ok(self.order.as_ref().and_then(|o| o.get(target).copied()))
    }

    fn answer(&mut self, ask: Ask) -> Result<Answer, String> {
        self.asks += 1;
        if self.asks > MAX_ASKS {
            return Err(format!("一个页面最多读取 {MAX_ASKS} 次内容或文件，请减少循环里的 collection()、record() 调用。"));
        }
        match ask {
            Ask::File(path) => {
                if check_path(&path).is_err() || is_binary(&path) {
                    return Ok(Answer::File(None));
                }
                let Some(source) = file(self.conn, self.mode, &path)? else { return Ok(Answer::File(None)) };
                let Some(n) = self.mark(&format!("file:{path}"))? else { return Ok(Answer::File(Some(source))) };
                self.files_changed.push(n);
                // A changed fragment is fenced so the finished page can show it as a region. The page itself,
                // and anything that extends another template, is a change of the whole page instead.
                if path == self.page || source.contains("extends") {
                    return Ok(Answer::File(Some(source)));
                }
                Ok(Answer::File(Some(format!("{REG_OPEN}{path}{REG_MID}{n}{REG_END_HEAD}{source}{REG_CLOSE}"))))
            }
            Ask::Records { collection, filter, sort, limit, offset, id } => self.records(&collection, &filter, sort, limit, offset, id).map(Answer::Records),
        }
    }

    fn records(&mut self, name: &str, filter: &Value, sort: Option<String>, limit: i64, offset: i64, id: Option<i64>) -> Result<Vec<Rec>, String> {
        use crate::query::{Op, Query, Status};
        let Some((def, owner)) = visible_def(self.conn, self.mode, name, self.viewer)? else {
            // Members-only content for a visitor who is not logged in: nothing, rather than a broken page.
            return match any_def(self.conn, self.mode, name)? {
                Some(d) if matches!(d.access.read, crate::schema::Read::Members | crate::schema::Read::Owner) => Ok(vec![]),
                _ => Err(format!("集合“{name}”不存在、没有上线或不公开，网站读不到它。")),
            };
        };
        let mut filters = if filter.is_null() { vec![] } else { crate::query::parse_filter(&def, filter)? };
        if let Some(id) = id {
            filters.push(("id".into(), Op::Eq, json!(id)));
        }
        let sort = sort.map(|s| match s.strip_prefix('-') {
            Some(f) => (f.to_string(), true),
            None => (s, false),
        });
        let live = self.mode == Mode::Live;
        // Preview: drafts over live copies, without records waiting to be deleted.
        let status = if live { vec![] } else { vec![Status::Draft, Status::Changed, Status::Published] };
        let q = Query { filters, status, sort, limit, offset, owner };
        let (rows, _) = crate::query::run(self.conn, &def, &q, live)?;
        let mut out = vec![];
        for row in rows {
            let mark = self.mark(&format!("record:{}", row.id))?;
            // A changed record: only the fields that differ from the live copy are marked.
            let live_copy = match (mark, row.status) {
                (Some(_), "changed") => {
                    let doc: Option<String> = self.conn
                        .query_row("SELECT published FROM records WHERE id = ?1", [row.id], |r| r.get(0))
                        .map_err(|e| format!("读取记录 {} 失败：{e}", row.id))?;
                    doc.and_then(|d| serde_json::from_str::<Value>(&d).ok()).map(|d| crate::schema::project(&def, &d))
                }
                _ => None,
            };
            let mut fields = vec![("id".to_string(), json!(row.id), false, None)];
            for f in &def.fields {
                let v = row.doc.get(&f.name).cloned().unwrap_or(Value::Null);
                let changed = live_copy.as_ref().is_none_or(|old| old.get(&f.name) != Some(&v));
                fields.push((f.name.clone(), v, f.kind == FieldType::Richtext, mark.filter(|_| changed)));
            }
            out.push(Rec(fields));
        }
        Ok(out)
    }
}

/// Markers a preview render leaves in its output for `finish` to turn into elements. Private-use characters,
/// removed from content before it reaches a template.
const VAL_OPEN: char = '\u{E000}';
const VAL_MID: char = '\u{E001}';
const VAL_CLOSE: char = '\u{E002}';
const REG_OPEN: char = '\u{E003}';
const REG_MID: char = '\u{E004}';
const REG_END_HEAD: char = '\u{E005}';
const REG_CLOSE: char = '\u{E006}';

fn is_marker(c: char) -> bool {
    ('\u{E000}'..='\u{E006}').contains(&c)
}

fn strip_marker_chars(v: &mut Value) {
    match v {
        Value::String(s) if s.chars().any(is_marker) => s.retain(|c| !is_marker(c)),
        Value::Array(a) => a.iter_mut().for_each(strip_marker_chars),
        Value::Object(o) => o.values_mut().for_each(strip_marker_chars),
        _ => {}
    }
}

/// The value a changed field produced last, until something is printed: lets the formatter mark exactly
/// `{{ item.field }}` while comparisons and filters see the plain value.
type LastRead = Arc<Mutex<Option<(minijinja::Value, usize)>>>;

/// A record in a template: a map whose changed fields remember being read.
#[derive(Debug)]
struct Record {
    values: BTreeMap<String, minijinja::Value>,
    marks: BTreeMap<String, usize>,
    last: LastRead,
}

impl Object for Record {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &minijinja::Value) -> Option<minijinja::Value> {
        let key = key.as_str()?;
        let v = self.values.get(key)?.clone();
        if let Some(n) = self.marks.get(key) {
            *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some((v.clone(), *n));
        }
        Some(v)
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Values(self.values.keys().map(|k| minijinja::Value::from(k.as_str())).collect())
    }
}

fn to_template(rec: Rec, last: &LastRead) -> minijinja::Value {
    let mut values = BTreeMap::new();
    let mut marks = BTreeMap::new();
    for (name, mut v, rich, mark) in rec.0 {
        strip_marker_chars(&mut v);
        // Rich text was cleaned when it was written: it renders as HTML.
        let value = match (&v, rich) {
            (Value::String(s), true) => minijinja::Value::from_safe_string(s.clone()),
            _ => minijinja::Value::from_serialize(&v),
        };
        if let Some(n) = mark {
            marks.insert(name.clone(), n);
        }
        values.insert(name, value);
    }
    minijinja::Value::from_object(Record { values, marks, last: last.clone() })
}

/// Sends lookups to the connection thread.
#[derive(Clone)]
struct Asker(mpsc::Sender<Line>);

impl Asker {
    fn ask(&self, ask: Ask) -> Result<Answer, minijinja::Error> {
        let (tx, rx) = mpsc::channel();
        let gone = || minijinja::Error::new(ErrorKind::InvalidOperation, "渲染已中止。");
        self.0.send((ask, tx)).map_err(|_| gone())?;
        rx.recv().map_err(|_| gone())?.map_err(|e| minijinja::Error::new(ErrorKind::InvalidOperation, e))
    }

    fn records(&self, ask: Ask) -> Result<Vec<Rec>, minijinja::Error> {
        match self.ask(ask)? {
            Answer::Records(r) => Ok(r),
            Answer::File(_) => Ok(vec![]),
        }
    }
}

/// Builds the environment and renders, on its own thread; every lookup goes through `asker`.
fn render_thread(asker: Asker, mode: Mode, page: &str, ctx: Value) -> Result<String, String> {
    let last = LastRead::default();
    let mut env = Environment::new();
    // A page is rendered with whatever context its route has: a value it does not get renders empty.
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    env.set_fuel(Some(FUEL));
    // Output is HTML whatever the template's own extension: an included `.svg` or `.txt` escapes values too.
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
    env.add_filter("t", crate::i18n::filter);
    let a = asker.clone();
    env.set_loader(move |name| {
        if let Some((_, source)) = FALLBACKS.iter().find(|(n, _)| *n == name) {
            return Ok(Some(source.to_string()));
        }
        match a.ask(Ask::File(name.to_string()))? {
            Answer::File(f) => Ok(f),
            Answer::Records(_) => Ok(None),
        }
    });
    let (a, l) = (asker.clone(), last.clone());
    env.add_function("collection", move |name: String, filter: Option<minijinja::Value>, sort: Option<String>, limit: Option<i64>, kwargs: Kwargs| {
        let filter = kwargs.get::<Option<minijinja::Value>>("filter")?.or(filter);
        let sort = kwargs.get::<Option<String>>("sort")?.or(sort);
        let limit = kwargs.get::<Option<i64>>("limit")?.or(limit).unwrap_or(20);
        let offset = kwargs.get::<Option<i64>>("offset")?.unwrap_or(0);
        kwargs.assert_all_used()?;
        let filter = serde_json::to_value(&filter).map_err(|e| minijinja::Error::new(ErrorKind::InvalidOperation, e.to_string()))?;
        let recs = a.records(Ask::Records { collection: name, filter, sort, limit, offset, id: None })?;
        Ok(minijinja::Value::from(recs.into_iter().map(|r| to_template(r, &l)).collect::<Vec<_>>()))
    });
    let (a, l) = (asker, last.clone());
    env.add_function("record", move |name: String, id: i64| {
        let recs = a.records(Ask::Records { collection: name, filter: Value::Null, sort: None, limit: 1, offset: 0, id: Some(id) })?;
        Ok(recs.into_iter().next().map_or(minijinja::Value::from(()), |r| to_template(r, &l)))
    });
    if mode == Mode::Preview {
        env.set_formatter(move |out, state, value| {
            let mark = last.lock().unwrap_or_else(|e| e.into_inner()).take().filter(|(v, _)| v == value).map(|(_, n)| n);
            if let Some(n) = mark {
                out.write_str(&format!("{VAL_OPEN}{n}{VAL_MID}"))?;
            }
            minijinja::escape_formatter(out, state, value)?;
            if mark.is_some() {
                out.write_str(&VAL_CLOSE.to_string())?;
            }
            Ok(())
        });
    }
    env.get_template(page).and_then(|t| t.render(minijinja::Value::from_serialize(&ctx))).map_err(|e| template_error(&e))
}

/// A rendered page. In preview, `page_changes` are the marks of changed files that shaped the whole page
/// (the page itself, its layouts, fragments that could not be shown as a region).
pub struct Rendered {
    pub html: String,
    pub page_changes: Vec<usize>,
}

/// Renders the template at `page` (a site file, or a built-in `_hc/` template) with `ctx` plus the site
/// functions `collection(name, filter?, sort?, limit?, offset?)` and `record(collection, id)`. Lookups run on
/// `conn`, the caller's connection, so a render inside the publish transaction sees what is about to go live.
pub fn render(conn: &Connection, mode: Mode, page: &str, ctx: Value) -> Result<Rendered, String> {
    let viewer = ctx["member"]["id"].as_i64().map_or(crate::access::Viewer::Anon, crate::access::Viewer::Member);
    let mut server = Server { conn, mode, page: page.to_string(), order: None, files_changed: vec![], asks: 0, viewer };
    let (tx, rx) = mpsc::channel::<Line>();
    // minijinja functions must be 'static, so they cannot borrow the connection: the render runs on a scoped
    // thread and asks this one, which answers until the render drops its end of the channel.
    let html = std::thread::scope(|s| {
        let render = s.spawn(move || render_thread(Asker(tx), mode, page, ctx));
        for (ask, reply) in rx {
            let _ = reply.send(server.answer(ask));
        }
        render.join().unwrap_or_else(|_| Err("渲染页面时出现内部错误。".into()))
    })?;
    if mode == Mode::Live {
        return Ok(Rendered { html, page_changes: vec![] });
    }
    let (html, regions) = finish(&html);
    let mut page_changes: Vec<usize> = server.files_changed.into_iter().filter(|n| !regions.contains(n)).collect();
    page_changes.sort();
    page_changes.dedup();
    Ok(Rendered { html, page_changes })
}

/// Removes every marker from text where no element may go.
fn strip_markers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut skip_to: Option<char> = None;
    for c in s.chars() {
        if let Some(end) = skip_to {
            if c == end {
                skip_to = None;
            }
            continue;
        }
        match c {
            VAL_OPEN => skip_to = Some(VAL_MID),
            REG_OPEN => skip_to = Some(REG_END_HEAD),
            c if is_marker(c) => {}
            c => out.push(c),
        }
    }
    out
}

fn find_ci(s: &str, needle: &str) -> Option<usize> {
    let lower = s.to_ascii_lowercase();
    lower.find(needle)
}

/// Turns a preview render's markers into elements where they may stand: in body text, a changed value becomes
/// `<span data-hc-change="N">` and a changed fragment `<hc-region …>`. Inside a tag, a comment, a raw-text
/// element (`script`, `style`, `title`, `textarea`) or `<head>`, and around whole documents, the markers are
/// dropped. Returns the page and the marks shown as regions.
fn finish(html: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(html.len() + 256);
    let mut regions = vec![];
    let (mut values, mut fences): (Vec<bool>, Vec<bool>) = (vec![], vec![]);
    let mut in_head = false;
    let mut i = 0;
    while i < html.len() {
        let rest = &html[i..];
        let c = rest.chars().next().unwrap_or_default();
        match c {
            VAL_OPEN => {
                let Some(mid) = rest.find(VAL_MID) else { i += c.len_utf8(); continue };
                let n = rest[c.len_utf8()..mid].to_string();
                let show = !in_head && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
                if show {
                    out.push_str(&format!("<span data-hc-change=\"{n}\">"));
                }
                values.push(show);
                i += mid + VAL_MID.len_utf8();
            }
            VAL_CLOSE => {
                if values.pop() == Some(true) {
                    out.push_str("</span>");
                }
                i += c.len_utf8();
            }
            REG_OPEN => {
                let (Some(mid), Some(head_end)) = (rest.find(REG_MID), rest.find(REG_END_HEAD)) else { i += c.len_utf8(); continue };
                let path = &rest[c.len_utf8()..mid];
                let n = &rest[mid + REG_MID.len_utf8()..head_end];
                let body_start = head_end + REG_END_HEAD.len_utf8();
                // The fragment up to its matching close.
                let mut depth = 1;
                let mut body_end = None;
                for (j, ch) in rest[body_start..].char_indices() {
                    match ch {
                        REG_OPEN => depth += 1,
                        REG_CLOSE => {
                            depth -= 1;
                            if depth == 0 {
                                body_end = Some(body_start + j);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let body = body_end.map(|e| rest[body_start..e].to_ascii_lowercase());
                let whole_document = body.is_none_or(|b| ["<html", "<head", "<body", "<!doctype"].iter().any(|t| b.contains(t)));
                let show = !in_head && !whole_document && n.bytes().all(|b| b.is_ascii_digit());
                if show {
                    out.push_str(&format!("<hc-region data-hc-file=\"{}\" data-hc-change=\"{n}\" style=\"display:contents\">", path.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")));
                    regions.push(n.parse().unwrap_or_default());
                }
                fences.push(show);
                i += body_start;
            }
            REG_CLOSE => {
                if fences.pop() == Some(true) {
                    out.push_str("</hc-region>");
                }
                i += c.len_utf8();
            }
            c if is_marker(c) => i += c.len_utf8(),
            '<' if rest.starts_with("<!--") => {
                let end = rest.find("-->").map_or(rest.len(), |e| e + 3);
                out.push_str(&strip_markers(&rest[..end]));
                i += end;
            }
            '<' if rest[1..].starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '/' || ch == '!' || ch == '?') => {
                // The whole tag, quotes respected.
                let mut quote: Option<char> = None;
                let mut end = rest.len();
                for (j, ch) in rest.char_indices().skip(1) {
                    match (quote, ch) {
                        (Some(q), ch) if ch == q => quote = None,
                        (Some(_), _) => {}
                        (None, '"' | '\'') => quote = Some(ch),
                        (None, '>') => {
                            end = j + 1;
                            break;
                        }
                        _ => {}
                    }
                }
                let tag = &rest[..end];
                out.push_str(&strip_markers(tag));
                i += end;
                let name: String = tag[1..].chars().take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '/').collect::<String>().to_ascii_lowercase();
                match name.as_str() {
                    "head" => in_head = true,
                    "/head" | "body" => in_head = false,
                    "script" | "style" | "title" | "textarea" => {
                        // Raw text up to the closing tag: no elements can go there.
                        let rest = &html[i..];
                        let close = find_ci(rest, &format!("</{name}")).unwrap_or(rest.len());
                        out.push_str(&strip_markers(&rest[..close]));
                        i += close;
                    }
                    _ => {}
                }
            }
            c => {
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    (out, regions)
}

/// Where tags injected into a page go: right after `<head>`, else after `<html>`, else after the doctype,
/// else at the start.
pub fn head_insert_at(html: &str) -> usize {
    for open in ["<head", "<html", "<!doctype"] {
        let mut from = 0;
        while let Some(at) = find_ci(&html[from..], open).map(|a| a + from) {
            let next = html[at + open.len()..].chars().next();
            if matches!(next, Some('>' | '/') | None) || next.is_some_and(char::is_whitespace) {
                if let Some(close) = html[at..].find('>') {
                    return at + close + 1;
                }
            }
            from = at + open.len();
        }
    }
    0
}

/// After a release, every live page renders and every live script transpiles, whether or not the release touched it.
// ponytail: renders and transpiles every live file on each publish and each review-page dry run; cache results by
// content hash if sites grow to hundreds of large files.
pub fn check_live(conn: &Connection, problems: &mut Vec<String>) -> Result<(), String> {
    let fail = |e: rusqlite::Error| format!("读取站点文件失败：{e}");
    let mut stmt = conn.prepare("SELECT path FROM site_files WHERE published IS NOT NULL ORDER BY path").map_err(fail)?;
    let paths: Vec<String> = stmt.query_map([], |r| r.get(0)).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
    for path in paths {
        let result = if ext(&path) == "html" {
            let ctx = json!({ "request": { "path": format!("/{path}"), "query": {} }, "member": null });
            render(conn, Mode::Live, &path, ctx).map(|_| ())
        } else if is_script(&path) {
            let source = file(conn, Mode::Live, &path)?.unwrap_or_default();
            transpile(&path, &source).map(|_| ())
        } else {
            continue;
        };
        if let Err(e) = result {
            problems.push(format!("file:{path} 上线后会出错：{}", e.trim_end_matches('。')));
        }
    }
    Ok(())
}

// ---- bundles ----

/// macOS archive metadata, dropped from bundles without comment.
fn is_junk(name: &str) -> bool {
    name.starts_with("__MACOSX/") || name.rsplit('/').next() == Some(".DS_Store")
}

/// The files of a zip bundle, each checked like a single write; any problem rejects the bundle, all of them
/// listed. Directory entries and macOS metadata are skipped; when the site sits in one top folder (it holds
/// `index.html` and nothing lies outside it), that folder is stripped.
pub fn read_bundle(bytes: &[u8]) -> Result<Vec<(String, String)>, String> {
    let bad_zip = |e: zip::result::ZipError| format!("压缩包读不出来：{e}。请上传 zip 格式的压缩包。");
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(bad_zip)?;
    // Headers first: names and declared sizes, nothing decompressed yet.
    let mut entries = vec![];
    let mut declared = 0u64;
    for i in 0..zip.len() {
        let f = zip.by_index_raw(i).map_err(bad_zip)?;
        let name = f.name().map_err(bad_zip)?.into_owned();
        if f.is_dir() || is_junk(&name) {
            continue;
        }
        declared += f.size();
        entries.push((i, name, f.size()));
    }
    if entries.len() > MAX_FILES {
        return Err(format!("压缩包里有 {} 个文件，站点最多 {MAX_FILES} 个文件。", entries.len()));
    }
    if declared > MAX_BUNDLE_BYTES as u64 {
        return Err(format!("压缩包解压后有 {} MB，一次最多 50 MB。", declared >> 20));
    }
    let top = entries.first().and_then(|(_, n, _)| n.split_once('/')).map(|(t, _)| format!("{t}/"));
    let strip = top.filter(|t| entries.iter().all(|(_, n, _)| n.starts_with(t.as_str())) && entries.iter().any(|(_, n, _)| *n == format!("{t}index.html")));

    let mut problems = vec![];
    let mut files: Vec<(String, String)> = vec![];
    let mut total = 0usize;
    for (i, name, size) in entries {
        let path = strip.as_deref().and_then(|t| name.strip_prefix(t)).unwrap_or(&name).to_string();
        if let Err(e) = check_path(&path) {
            problems.push(e);
            continue;
        }
        let (limit, label) = if is_binary(&path) { (MAX_BINARY_BYTES, "二进制文件最多 5 MB") } else { (MAX_FILE_BYTES, "文本文件最多 1 MB") };
        if size > limit as u64 {
            problems.push(format!("“{path}”太大：{label}。"));
            continue;
        }
        // Declared sizes can lie: read at most one byte past the limit.
        let mut data = vec![];
        let read = zip.by_index(i).and_then(|f| Ok(f.take(limit as u64 + 1).read_to_end(&mut data)?));
        if let Err(e) = read {
            problems.push(format!("“{path}”读不出来：{e}。"));
            continue;
        }
        total += data.len();
        if total > MAX_BUNDLE_BYTES {
            return Err("压缩包解压后超过 50 MB，一次最多 50 MB。".into());
        }
        if data.len() > limit {
            problems.push(format!("“{path}”太大：{label}。"));
            continue;
        }
        let content = if is_binary(&path) {
            base64::engine::general_purpose::STANDARD.encode(&data)
        } else {
            let Ok(text) = String::from_utf8(data) else {
                problems.push(format!("“{path}”不是 UTF-8 文本：二进制文件请用 {} 之一作扩展名。", BINARY_EXTENSIONS.join("、")));
                continue;
            };
            text
        };
        if files.iter().any(|(p, _)| *p == path) {
            problems.push(format!("“{path}”在压缩包里出现了不止一次。"));
            continue;
        }
        match check_file(&path, &content) {
            Ok(()) => files.push((path, content)),
            Err(e) => problems.push(e),
        }
    }
    if problems.is_empty() {
        return Ok(files);
    }
    let n = problems.len();
    let more = if n > 20 { format!("（另有 {} 个问题未列出）", n - 20) } else { String::new() };
    problems.truncate(20);
    Err(format!("压缩包里有 {n} 个问题，什么都没有写入：{}{more}", problems.join("；")))
}
