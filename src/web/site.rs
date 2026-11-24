//! The built-in website: site files by path, the built-in templates for public collections, and the draft
//! preview. Every path no other route claims ends up here.

use super::session;
use crate::site::{self, Mode};
use crate::{App, Shared};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use base64::Engine;
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

pub const PREVIEW_JS: &str = include_str!("preview.js");
pub const PREVIEW_CSS: &str = include_str!("preview.css");

/// The preview's own script and style, served for preview pages only to load (the live site never links them).
pub async fn preview_js() -> Response {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], PREVIEW_JS).into_response()
}

pub async fn preview_css() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], PREVIEW_CSS).into_response()
}

pub async fn serve(State(app): State<Shared>, req: Request) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return (StatusCode::METHOD_NOT_ALLOWED, "网站页面只接受 GET 请求。").into_response();
    }
    let path = req.uri().path().to_string();
    let query: Vec<(String, String)> = axum::extract::Query::try_from_uri(req.uri()).map(|q| q.0).unwrap_or_default();
    let headers = req.headers().clone();
    let a = app.clone();
    tokio::task::spawn_blocking(move || respond(&a, &a.conn(), &path, &query, &headers))
        .await
        .unwrap_or_else(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("服务器内部错误：{e}")).into_response())
}

/// Server routes the site never answers for.
fn reserved(rel: &str) -> bool {
    rel == "mcp" || ["admin", "api", "uploads", "_hc"].iter().any(|p| rel == *p || rel.starts_with(&format!("{p}/")))
}

/// One request: who is asking, which mode, which file or built-in template.
struct Visit<'a> {
    app: &'a App,
    conn: &'a Connection,
    mode: Mode,
    path: &'a str,
    query: &'a [(String, String)],
    /// The preview key to hand to the browser (set on an admin's `?preview=1`).
    key: Option<String>,
    if_none_match: Option<String>,
}

fn respond(app: &App, conn: &Connection, path: &str, query: &[(String, String)], headers: &HeaderMap) -> Response {
    let rel = path.strip_prefix('/').unwrap_or(path);
    let asked = query.iter().any(|(k, v)| k == "preview" && v == "1");
    let admin = if asked { session::from_cookie(conn, headers) } else { None };
    let fetch = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    // Requests from the sandboxed preview frame come from an opaque origin: cross-site, without the session
    // cookie, with the preview key. A document is only previewed when it says so, so a link from another site
    // never opens drafts.
    let framed = fetch("sec-fetch-site") == "cross-site" && !matches!(fetch("sec-fetch-dest").as_str(), "" | "document");
    let preview = admin.is_some() || ((asked || framed) && session::has_preview_key(conn, headers));
    let key = admin.map(|u| session::preview_key(&u.token_hash));
    let if_none_match = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).map(String::from);
    let v = Visit { app, conn, mode: if preview { Mode::Preview } else { Mode::Live }, path, query, key, if_none_match };
    if reserved(rel.trim_end_matches('/')) {
        return v.not_found();
    }
    match v.route(rel) {
        Ok(resp) => resp,
        Err(e) => v.failed(&e),
    }
}

/// Which site files a path may name, in order.
fn candidates(rel: &str) -> Vec<String> {
    let last = rel.rsplit('/').next().unwrap_or_default();
    if rel.is_empty() {
        vec!["index.html".into()]
    } else if rel.ends_with('/') {
        vec![format!("{rel}index.html")]
    } else if last.contains('.') {
        vec![rel.to_string()]
    } else {
        vec![format!("{rel}.html"), format!("{rel}/index.html")]
    }
}

impl Visit<'_> {
    fn route(&self, rel: &str) -> Result<Response, String> {
        for name in candidates(rel) {
            if site::check_path(&name).is_err() {
                continue;
            }
            if let Some(content) = site::file(self.conn, self.mode, &name)? {
                return self.file(&name, content);
            }
        }
        // No file: the built-in templates for public collections.
        let parts: Vec<&str> = rel.trim_end_matches('/').split('/').collect();
        match parts.as_slice() {
            [""] => {
                let mut list = vec![];
                for name in collection_names(self.conn)? {
                    if let Some(def) = site::public_def(self.conn, self.mode, &name)? {
                        list.push(json!({ "name": def.name, "title": def.title }));
                    }
                }
                Ok(self.page("_hc/home.html", json!({ "collections": list }), StatusCode::OK))
            }
            [name] => match site::public_def(self.conn, self.mode, name)? {
                Some(def) => Ok(self.page("_hc/list.html", json!({ "def": describe(&def) }), StatusCode::OK)),
                None => Ok(self.not_found()),
            },
            [name, id] => {
                let (Some(def), Ok(id)) = (site::public_def(self.conn, self.mode, name)?, id.parse::<i64>()) else { return Ok(self.not_found()) };
                if !self.record_exists(name, id)? {
                    return Ok(self.not_found());
                }
                Ok(self.page("_hc/item.html", json!({ "def": describe(&def), "id": id }), StatusCode::OK))
            }
            _ => Ok(self.not_found()),
        }
    }

    fn record_exists(&self, collection: &str, id: i64) -> Result<bool, String> {
        let visible = match self.mode {
            Mode::Live => "published IS NOT NULL",
            Mode::Preview => "(published IS NOT NULL OR draft IS NOT NULL) AND draft_deleted = 0",
        };
        self.conn
            .query_row(&format!("SELECT EXISTS(SELECT 1 FROM records WHERE id = ?1 AND collection = ?2 AND {visible})"), rusqlite::params![id, collection], |r| r.get(0))
            .map_err(|e| format!("读取记录失败：{e}"))
    }

    /// The common headers of a site response.
    fn finish(&self, mut resp: Response, script_hash: Option<&str>) -> Response {
        let h = resp.headers_mut();
        let csp = site_csp(&script_origins(self.conn), script_hash, self.mode == Mode::Preview);
        if let Ok(v) = HeaderValue::from_str(&csp) {
            h.insert(header::CONTENT_SECURITY_POLICY, v);
        }
        let cache = if self.mode == Mode::Preview { "no-store" } else { "no-cache" };
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
        if let Some(v) = self.key.as_deref().and_then(|k| HeaderValue::from_str(&session::preview_cookie(k)).ok()) {
            h.insert(header::SET_COOKIE, v);
        }
        resp
    }

    /// A site file as its type says: `.html` rendered, `.jsx/.ts/.tsx` transpiled, binaries decoded.
    fn file(&self, name: &str, content: String) -> Result<Response, String> {
        if name.ends_with(".html") {
            return Ok(self.page(name, json!({}), StatusCode::OK));
        }
        let body: Vec<u8> = if site::is_binary(name) {
            site::decode_binary(&content).ok_or_else(|| format!("站点文件“{name}”已损坏：不是有效的 base64。"))?
        } else if [".jsx", ".ts", ".tsx"].iter().any(|e| name.ends_with(e)) {
            transpiled(name, &content)?.as_bytes().to_vec()
        } else {
            content.into_bytes()
        };
        // A validator, so the browser revalidates instead of downloading again.
        let etag = format!("\"{}\"", &hex::encode(Sha256::digest(&body))[..16]);
        if self.mode == Mode::Live && self.if_none_match.as_deref() == Some(etag.as_str()) {
            return Ok(self.finish((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response(), None));
        }
        Ok(self.finish(([(header::CONTENT_TYPE, site::content_type(name)), (header::ETAG, etag.as_str())], body).into_response(), None))
    }

    /// Renders a template into a page: the import map and, in preview, the preview's assets go after `<head>`.
    fn page(&self, template: &str, extra: Value, status: StatusCode) -> Response {
        let query: serde_json::Map<String, Value> = self.query.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        let mut ctx = json!({
            "site": { "name": self.app.site_name, "origin": self.app.origin },
            "request": { "path": self.path, "query": query },
            "member": null,
        });
        if let (Some(c), Value::Object(extra)) = (ctx.as_object_mut(), extra) {
            c.extend(extra);
        }
        let rendered = match site::render(self.conn, self.mode, template, ctx) {
            Ok(r) => r,
            Err(e) => return self.failed(&format!("{template}：{e}")),
        };
        let mut html = rendered.html;
        let mut inject = String::new();
        let map = import_map(self.conn);
        if let Some(map) = &map {
            inject.push_str(&format!("<script type=\"importmap\">{map}</script>"));
        }
        if self.mode == Mode::Preview {
            let changes = rendered.page_changes.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(",");
            inject.push_str(&format!("<link rel=\"stylesheet\" href=\"/_hc/preview.css\"><script src=\"/_hc/preview.js\" defer data-hc-page=\"{changes}\"></script>"));
        }
        html.insert_str(site::head_insert_at(&html), &inject);
        let hash = map.map(|m| base64::engine::general_purpose::STANDARD.encode(Sha256::digest(m.as_bytes())));
        self.finish((status, Html(html)).into_response(), hash.as_deref())
    }

    fn not_found(&self) -> Response {
        let custom = matches!(site::file(self.conn, self.mode, "404.html"), Ok(Some(_)));
        self.page(if custom { "404.html" } else { "_hc/404.html" }, json!({}), StatusCode::NOT_FOUND)
    }

    /// A page that could not be built: a plain 500 on the live site, the reason in preview.
    fn failed(&self, reason: &str) -> Response {
        let detail = if self.mode == Mode::Preview {
            let esc = reason.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            format!("<pre style=\"white-space:pre-wrap\">{esc}</pre>")
        } else {
            eprintln!("网站页面 {} 出错：{reason}", self.path);
            String::new()
        };
        let html = format!("<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><title>页面出错了</title></head><body><h1>页面出错了</h1><p>请稍后再试。</p>{detail}</body></html>");
        self.finish((StatusCode::INTERNAL_SERVER_ERROR, Html(html)).into_response(), None)
    }
}

fn collection_names(conn: &Connection) -> Result<Vec<String>, String> {
    let fail = |e: rusqlite::Error| format!("读取集合失败：{e}");
    let mut stmt = conn.prepare("SELECT name FROM collections ORDER BY name").map_err(fail)?;
    let names = stmt.query_map([], |r| r.get(0)).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
    Ok(names)
}

/// What the built-in templates need of a definition: names, titles, types, and the field that names a record.
fn describe(def: &crate::schema::CollectionDef) -> Value {
    use crate::schema::FieldType;
    let label = def.fields.iter().find(|f| f.kind == FieldType::Text && !f.multiple).map(|f| f.name.clone());
    let fields: Vec<Value> = def.fields.iter().map(|f| json!({ "name": f.name, "title": f.title, "type": f.kind, "multiple": f.multiple })).collect();
    json!({ "name": def.name, "title": def.title, "label": label, "fields": fields })
}

/// Transpiled scripts by content hash: a file is transpiled once per version, not per request.
// ponytail: an unbounded-ish map cleared at 256 entries; an LRU if sites churn through many script versions.
fn transpiled(name: &str, source: &str) -> Result<Arc<str>, String> {
    static CACHE: LazyLock<Mutex<HashMap<String, Arc<str>>>> = LazyLock::new(Default::default);
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    let key = hex::encode(Sha256::digest(format!("{ext}\0{source}")));
    if let Some(hit) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Ok(hit.clone());
    }
    let out: Arc<str> = site::transpile(name, source)?.into();
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= 256 {
        cache.clear();
    }
    cache.insert(key, out.clone());
    Ok(out)
}

fn setting(conn: &Connection, key: &str) -> Option<String> {
    use rusqlite::OptionalExtension;
    conn.query_row("SELECT value FROM site_settings WHERE key = ?1", [key], |r| r.get(0)).optional().ok().flatten()
}

/// The `import_map` setting as the text of an inline script: a JSON object, `<` escaped so it cannot end the tag.
fn import_map(conn: &Connection) -> Option<String> {
    let map: Value = serde_json::from_str(&setting(conn, "import_map")?).ok().filter(Value::is_object)?;
    Some(map.to_string().replace('<', "\\u003c"))
}

/// `scheme://host[:port]` with http(s) and nothing else: safe to put in a header.
fn valid_origin(s: &str) -> bool {
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://"));
    rest.is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_alphanumeric() || b".-:".contains(&b)))
}

/// Where site scripts may come from besides the site: the `script_origins` setting, else `https://esm.sh`.
fn script_origins(conn: &Connection) -> Vec<String> {
    match setting(conn, "script_origins") {
        None => vec!["https://esm.sh".into()],
        Some(json) => serde_json::from_str::<Vec<String>>(&json).unwrap_or_default().into_iter().filter(|o| valid_origin(o)).collect(),
    }
}

/// The site's policy: scripts from the site, the allowed origins and the one inline import map; inline styles
/// allowed so templates can style themselves. A preview also runs sandboxed, so its scripts never share the
/// admin's origin, even when the preview is opened in its own tab.
pub fn site_csp(origins: &[String], script_hash: Option<&str>, preview: bool) -> String {
    let sandbox = if preview { "sandbox allow-scripts allow-forms allow-popups; " } else { "" };
    let mut scripts = String::from("'self'");
    for o in origins {
        scripts.push(' ');
        scripts.push_str(o);
    }
    if let Some(h) = script_hash {
        scripts.push_str(&format!(" 'sha256-{h}'"));
    }
    format!("{sandbox}default-src 'self'; img-src 'self' https: data:; style-src 'self' 'unsafe-inline'; script-src {scripts}; frame-ancestors 'self'; base-uri 'none'; form-action 'self'")
}
