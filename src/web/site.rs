//! The built-in website: site files by path, the built-in templates for public collections, and the draft
//! preview. Every path no other route claims ends up here.

use super::address::Address;
use super::session;
use crate::site::{self, Mode};
use crate::{App, Shared};
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
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

/// The site: always the published version, whoever asks and whatever cookies or headers come along. Drafts are
/// only ever rendered under the admin's preview entry (`preview`).
pub async fn serve(State(app): State<Shared>, req: Request) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return (StatusCode::METHOD_NOT_ALLOWED, "网站页面只接受 GET 请求。").into_response();
    }
    // In 分开 mode the admin (on its own origin) frames the live site for 线上.
    let framers = match req.extensions().get::<Address>().and_then(|a| a.admin.clone()) {
        Some(admin) => format!("'self' {admin}"),
        None => "'self'".into(),
    };
    let admin_url = req.extensions().get::<Address>().and_then(|a| a.admin.clone()).map_or_else(|| "/admin".to_string(), |a| format!("{a}/admin"));
    let (path, query, if_none_match) = parts(&req);
    let lang = crate::i18n::for_visitor(req.headers(), req.uri().query());
    let picked = crate::i18n::from_query(req.uri().query());
    let member_token = session::cookie_value(req.headers(), crate::members::COOKIE);
    let a = app.clone();
    let resp = tokio::task::spawn_blocking(move || {
        let conn = a.conn();
        // The member cookie only personalises the live site when member login is live.
        let member = member_token
            .filter(|_| crate::members::login_enabled(&conn, false))
            .and_then(|t| crate::members::from_token(&conn, &t))
            .map(|m| crate::members::member_json(&m));
        let v = Visit { app: &a, conn: &conn, mode: Mode::Live, base: "", path: &path, query: &query, if_none_match, framers, member, lang, admin_url };
        v.respond()
    })
    .await
    .unwrap_or_else(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("服务器内部错误：{e}")).into_response());
    // The 中文 / EN switch: remember the pick for the next pages.
    match picked {
        Some(l) => ([(header::SET_COOKIE, crate::i18n::remember(l))], resp).into_response(),
        None => resp,
    }
}

/// `GET /_hc/preview/<token>/<path>`: the draft site for the admin's preview frame. The frame is sandboxed (an
/// opaque origin that sends no cookie), so the token in the path is the permission; see `session::issue_preview`.
/// The page's absolute paths are rewritten to stay under this prefix, so its stylesheets, scripts, module imports,
/// images and links load the drafts too.
pub async fn preview(State(app): State<Shared>, req: Request) -> Response {
    let rest = req.uri().path().strip_prefix("/_hc/preview/").unwrap_or_default().to_string();
    let Some((token, path)) = rest.split_once('/').map(|(t, p)| (t.to_string(), format!("/{p}"))) else {
        // Without the slash the frame's relative links would resolve one level up.
        return if rest.is_empty() { gone() } else { Redirect::to(&format!("/_hc/preview/{rest}/")).into_response() };
    };
    let (_, query, _) = parts(&req);
    let lang = crate::i18n::for_visitor(req.headers(), req.uri().query());
    let a = app.clone();
    tokio::task::spawn_blocking(move || {
        let conn = a.conn();
        if !session::use_preview(&conn, &token) {
            return gone();
        }
        let base = format!("/_hc/preview/{token}");
        let v = Visit { app: &a, conn: &conn, mode: Mode::Preview, base: &base, path: &path, query: &query, if_none_match: None, framers: "'self'".into(), member: None, lang, admin_url: "/admin".into() };
        v.respond()
    })
    .await
    .unwrap_or_else(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("服务器内部错误：{e}")).into_response())
}

/// An unknown, expired or ended preview token: nothing.
fn gone() -> Response {
    let headers = [(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CACHE_CONTROL, "no-store"), (header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'")];
    (StatusCode::NOT_FOUND, headers, "预览链接已失效：刷新后台页面就会拿到新的。").into_response()
}

fn parts(req: &Request) -> (String, Vec<(String, String)>, Option<String>) {
    let query = axum::extract::Query::try_from_uri(req.uri()).map(|q| q.0).unwrap_or_default();
    let inm = req.headers().get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).map(String::from);
    (req.uri().path().to_string(), query, inm)
}

/// Server routes the site never answers for.
fn reserved(rel: &str) -> bool {
    rel == "mcp" || ["admin", "api", "uploads", "_hc"].iter().any(|p| rel == *p || rel.starts_with(&format!("{p}/")))
}

/// One request: which mode, which file or built-in template.
struct Visit<'a> {
    app: &'a App,
    conn: &'a Connection,
    mode: Mode,
    /// The preview's prefix (`/_hc/preview/<token>`); empty on the live site.
    base: &'a str,
    /// The page's path as a visitor sees it.
    path: &'a str,
    query: &'a [(String, String)],
    if_none_match: Option<String>,
    /// Who may frame the page (`frame-ancestors`).
    framers: String,
    /// The logged-in member (live site only), as templates see it.
    member: Option<Value>,
    /// The visitor's language, for the built-in pages.
    lang: crate::i18n::Lang,
    /// Where the admin opens (its own origin in 分开 mode), for the empty site's welcome.
    admin_url: String,
}

/// The built-in sign-up or login page after a form post (`which` is "login" or "signup"), with an error.
pub fn account_page(app: &App, conn: &Connection, which: &str, form: Value, status: StatusCode, lang: crate::i18n::Lang) -> Response {
    let path = format!("/account/{which}");
    let v = Visit { app, conn, mode: Mode::Live, base: "", path: &path, query: &[], if_none_match: None, framers: "'self'".into(), member: None, lang, admin_url: "/admin".into() };
    let mut extra = json!({ "mode": which, "next": "", "email": "", "name": "", "error": null });
    if let (Some(e), Value::Object(f)) = (extra.as_object_mut(), form) {
        e.extend(f);
    }
    v.page(&v.account_template(which), extra, status)
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
    fn respond(&self) -> Response {
        let rel = self.path.strip_prefix('/').unwrap_or(self.path);
        if reserved(rel.trim_end_matches('/')) {
            return self.not_found();
        }
        match self.route(rel) {
            Ok(resp) => resp,
            Err(e) => self.failed(&e),
        }
    }

    fn route(&self, rel: &str) -> Result<Response, String> {
        for name in candidates(rel) {
            if site::check_path(&name).is_err() {
                continue;
            }
            if let Some(content) = site::file(self.conn, self.mode, &name)? {
                return self.file(&name, content);
            }
        }
        // No file: the one built-in page (the welcome on `/`) and, when member login is on, the account pages.
        let parts: Vec<&str> = rel.trim_end_matches('/').split('/').collect();
        if let ["account", which @ ("login" | "signup")] = parts.as_slice() {
            if !crate::members::login_enabled(self.conn, self.mode == Mode::Preview) {
                return Ok(self.not_found());
            }
            let next = self.query.iter().find(|(k, _)| k == "next").map(|(_, v)| safe_next(v)).unwrap_or_default();
            return Ok(self.page(&self.account_template(which), json!({ "mode": which, "next": next, "email": "", "name": "", "error": null }), StatusCode::OK));
        }
        if parts == [""] {
            return Ok(self.page("_hc/home.html", json!({}), StatusCode::OK));
        }
        Ok(self.not_found())
    }

    /// The common headers of a site response.
    fn finish(&self, mut resp: Response, script_hash: Option<&str>) -> Response {
        let h = resp.headers_mut();
        let csp = site_csp(&script_origins(self.conn, self.mode == Mode::Preview), script_hash, self.mode == Mode::Preview, &self.framers);
        if let Ok(v) = HeaderValue::from_str(&csp) {
            h.insert(header::CONTENT_SECURITY_POLICY, v);
        }
        let cache = if self.mode == Mode::Preview { "no-store" } else if self.member.is_some() { "private, no-cache" } else { "no-cache" };
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
        // Sandboxed frames (the preview, the 线上 view) have an opaque origin: module scripts, fetch() and fonts
        // they load are CORS requests. Nothing here depends on cookies, so anyone may read it.
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
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
        } else if self.mode == Mode::Preview && name.ends_with(".css") {
            rewrite_css(&content, self.base).into_bytes()
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
            "site": { "name": self.app.name(self.conn, self.mode == Mode::Preview), "origin": self.app.origin },
            "request": { "path": self.path, "query": query },
            "member": self.member,
            "lang": self.lang.code(),
            "html_lang": self.lang.html(),
        });
        // The built-in pages: where the admin opens, for the welcome.
        if template.starts_with("_hc/") {
            ctx["admin_url"] = json!(self.admin_url);
        }
        if let (Some(c), Value::Object(extra)) = (ctx.as_object_mut(), extra) {
            c.extend(extra);
        }
        let rendered = match site::render(self.conn, self.mode, template, ctx) {
            Ok(r) => r,
            Err(e) => return self.failed(&format!("{template}：{e}")),
        };
        let mut html = rendered.html;
        let mut inject = String::new();
        let map = if self.mode == Mode::Preview {
            html = rewrite_html(&html, self.base);
            let changes = rendered.page_changes.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(",");
            // Not deferred: it wraps fetch() before the page's own scripts run; the rest waits for the DOM.
            inject.push_str(&format!("<link rel=\"stylesheet\" href=\"/_hc/preview.css\"><script src=\"/_hc/preview.js\" data-hc-base=\"{}\" data-hc-page=\"{changes}\"></script>", self.base));
            Some(preview_map(self.conn, self.base))
        } else {
            import_map(self.conn)
        };
        if let Some(map) = &map {
            inject.push_str(&format!("<script type=\"importmap\">{map}</script>"));
        }
        html.insert_str(site::head_insert_at(&html), &inject);
        let hash = map.map(|m| base64::engine::general_purpose::STANDARD.encode(Sha256::digest(m.as_bytes())));
        self.finish((status, Html(html)).into_response(), hash.as_deref())
    }

    /// A site file `account/login.html` (or signup) replaces the built-in page when there is one.
    fn account_template(&self, which: &str) -> String {
        let own = format!("account/{which}.html");
        if matches!(site::file(self.conn, self.mode, &own), Ok(Some(_))) { own } else { "_hc/account.html".into() }
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

/// A drafted site option: the live value, or in preview the draft if there is one.
fn setting(conn: &Connection, key: &str, preview: bool) -> Option<Value> {
    Some(crate::changes::setting(conn, key, preview)).filter(|v| !v.is_null())
}

fn saved_map(conn: &Connection, preview: bool) -> Option<Value> {
    setting(conn, "import_map", preview).filter(Value::is_object)
}

/// The live `import_map` as the text of an inline script: a JSON object, `<` escaped so it cannot end the tag.
fn import_map(conn: &Connection) -> Option<String> {
    Some(saved_map(conn, false)?.to_string().replace('<', "\\u003c"))
}

/// The preview's import map: the saved one with its site paths moved under the preview, plus `"/"` mapped to the
/// preview so absolute module imports (`import "/assets/x.js"`, `import("/x.js")`) load drafts. `"/_hc/"` maps
/// to itself, so URLs already under the preview (relative imports resolve there) stay put: the longest key wins.
fn preview_map(conn: &Connection, base: &str) -> String {
    let mut map = saved_map(conn, true).unwrap_or_else(|| json!({}));
    let fix = |v: &mut Value| {
        if let Some(s) = v.as_str().filter(|s| site_path(s)) {
            *v = json!(format!("{base}{s}"));
        }
    };
    if let Some(scopes) = map.get_mut("scopes").and_then(Value::as_object_mut) {
        for (scope, mut list) in std::mem::take(scopes) {
            list.as_object_mut().into_iter().flat_map(|o| o.values_mut()).for_each(fix);
            scopes.insert(if site_path(&scope) { format!("{base}{scope}") } else { scope }, list);
        }
    }
    if !map.get("imports").is_some_and(Value::is_object) {
        map["imports"] = json!({});
    }
    if let Some(imports) = map["imports"].as_object_mut() {
        imports.values_mut().for_each(fix);
        imports.entry("/").or_insert_with(|| json!(format!("{base}/")));
        imports.entry("/_hc/").or_insert_with(|| json!("/_hc/"));
    }
    map.to_string().replace('<', "\\u003c")
}

// ---- keeping a preview's paths inside the preview ----

/// An absolute path to a site page or file: `/x`, but not `//host/x`, and not a path the server answers itself
/// (`/uploads/`, `/api/`, `/_hc/`, `/admin`, `/mcp`). Reads up to the end of the path in a longer text.
fn site_path(url: &str) -> bool {
    let Some(rel) = url.strip_prefix('/') else { return false };
    let first = rel.split(|c: char| "/?#)'\"".contains(c) || c.is_whitespace()).next().unwrap_or_default();
    !rel.starts_with(['/', '\\']) && !reserved(first)
}

/// Attributes that hold a URL.
const URL_ATTRS: [&str; 6] = ["src", "href", "action", "formaction", "poster", "srcset"];

/// A page with its site paths moved under the preview: URL attributes in tags, and `url()` / `@import` in inline
/// styles and `<style>`. Text and other attributes are left alone.
fn rewrite_html(html: &str, base: &str) -> String {
    let mut out = String::with_capacity(html.len() + 512);
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        rest = &rest[lt..];
        if !rest[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            out.push('<');
            rest = &rest[1..];
            continue;
        }
        // The whole tag, quotes respected.
        let mut quote = None;
        let mut end = rest.len();
        for (j, c) in rest.char_indices().skip(1) {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"' | '\'') => quote = Some(c),
                (None, '>') => {
                    end = j + 1;
                    break;
                }
                _ => {}
            }
        }
        out.push_str(&rewrite_tag(&rest[..end], base));
        rest = &rest[end..];
    }
    out.push_str(rest);
    rewrite_css(&out, base)
}

/// One tag with the values of its URL attributes moved under the preview.
fn rewrite_tag(tag: &str, base: &str) -> String {
    let b = tag.as_bytes();
    let (mut out, mut copied, mut i) = (String::with_capacity(tag.len() + 64), 0, 0);
    while i < b.len() {
        if b[i] != b'=' {
            i += 1;
            continue;
        }
        let name_end = tag[..i].trim_end().len();
        let name = &tag[tag[..name_end].rfind(|c: char| c.is_ascii_whitespace()).map_or(0, |s| s + 1)..name_end];
        let mut v = i + 1;
        while v < b.len() && b[v].is_ascii_whitespace() {
            v += 1;
        }
        let (start, end, next) = match b.get(v) {
            Some(&q @ (b'"' | b'\'')) => {
                let e = tag[v + 1..].find(q as char).map_or(b.len(), |e| v + 1 + e);
                (v + 1, e, e + 1)
            }
            _ => {
                let e = tag[v..].find(|c: char| c.is_ascii_whitespace() || c == '>').map_or(b.len(), |e| v + e);
                (v, e, e)
            }
        };
        if URL_ATTRS.iter().any(|a| a.eq_ignore_ascii_case(name)) {
            let value = &tag[start..end];
            let moved = if name.eq_ignore_ascii_case("srcset") {
                value.split(',').map(|c| {
                    let lead = c.len() - c.trim_start().len();
                    if site_path(c.trim_start()) { format!("{}{base}{}", &c[..lead], &c[lead..]) } else { c.to_string() }
                }).collect::<Vec<_>>().join(",")
            } else if site_path(value) {
                format!("{base}{value}")
            } else {
                value.to_string()
            };
            out.push_str(&tag[copied..start]);
            out.push_str(&moved);
            copied = end;
        }
        i = next.max(i + 1);
    }
    out.push_str(&tag[copied..]);
    out
}

/// A stylesheet with the site paths in `url(…)` and `@import "…"` moved under the preview.
fn rewrite_css(css: &str, base: &str) -> String {
    let lower = css.to_ascii_lowercase();
    let b = css.as_bytes();
    let mut at: Vec<usize> = vec![];
    for needle in ["url(", "@import"] {
        for (i, _) in lower.match_indices(needle) {
            let mut p = i + needle.len();
            while p < b.len() && (b[p].is_ascii_whitespace() || b[p] == b'"' || b[p] == b'\'') {
                p += 1;
            }
            if site_path(&css[p..]) {
                at.push(p);
            }
        }
    }
    at.sort_unstable();
    let mut out = String::with_capacity(css.len() + at.len() * base.len());
    let mut copied = 0;
    for p in at {
        out.push_str(&css[copied..p]);
        out.push_str(base);
        copied = p;
    }
    out.push_str(&css[copied..]);
    out
}

/// `scheme://host[:port]` with http(s) and nothing else: safe to put in a header.
fn valid_origin(s: &str) -> bool {
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://"));
    rest.is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_alphanumeric() || b".-:".contains(&b)))
}

/// Where site scripts may come from besides the site: the `script_origins` setting, else `https://esm.sh`.
fn script_origins(conn: &Connection, preview: bool) -> Vec<String> {
    match setting(conn, "script_origins", preview) {
        None => vec!["https://esm.sh".into()],
        Some(v) => serde_json::from_value::<Vec<String>>(v).unwrap_or_default().into_iter().filter(|o| valid_origin(o)).collect(),
    }
}

/// The site's policy: scripts from the site, the allowed origins and the one inline import map; inline styles
/// allowed so templates can style themselves. A preview also runs sandboxed, so its scripts never share the
/// admin's origin, even when the preview is opened in its own tab.
pub fn site_csp(origins: &[String], script_hash: Option<&str>, preview: bool, framers: &str) -> String {
    let sandbox = if preview { "sandbox allow-scripts allow-forms allow-popups; " } else { "" };
    let mut scripts = String::from("'self'");
    for o in origins {
        scripts.push(' ');
        scripts.push_str(o);
    }
    if let Some(h) = script_hash {
        scripts.push_str(&format!(" 'sha256-{h}'"));
    }
    format!("{sandbox}default-src 'self'; img-src 'self' https: data:; style-src 'self' 'unsafe-inline'; script-src {scripts}; frame-ancestors {framers}; base-uri 'none'; form-action 'self'")
}

/// Where to go after logging in: a path on this site only, never another host.
pub fn safe_next(next: &str) -> String {
    let n = next.trim();
    if n.starts_with('/') && !n.starts_with("//") && !n.contains('\\') && !n.chars().any(char::is_control) { n.to_string() } else { String::new() }
}
