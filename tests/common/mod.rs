#![allow(dead_code)]
use harness_cms::{store, App, Shared};
use rand::Rng;

pub fn app() -> Shared {
    let dir = std::env::temp_dir().join(format!("harness-cms-test-{:016x}", rand::thread_rng().gen::<u64>()));
    std::fs::create_dir_all(&dir).unwrap();
    App::new(
        store::open(":memory:").unwrap(),
        dir,
        "http://127.0.0.1:4200".into(),
        "测试站".into(),
    )
}

use harness_cms::auth::{Actor, Scope};
use serde_json::{json, Value};

pub fn builder() -> Actor {
    Actor { name: "builder@test".into(), scopes: vec![Scope::Read, Scope::Write, Scope::Structure, Scope::Publish], admin: true }
}

pub fn editor() -> Actor {
    Actor { name: "editor@test".into(), scopes: vec![Scope::Read, Scope::Write, Scope::Publish], admin: false }
}

pub fn reader() -> Actor {
    Actor { name: "reader@test".into(), scopes: vec![Scope::Read], admin: false }
}

pub fn ok(app: &Shared, who: &Actor, tool: &str, args: Value) -> Value {
    harness_cms::tools::call(app, who, tool, args).unwrap_or_else(|e| panic!("{tool} failed: {e}"))
}

pub fn err(app: &Shared, who: &Actor, tool: &str, args: Value) -> String {
    match harness_cms::tools::call(app, who, tool, args) {
        Ok(v) => panic!("{tool} should have failed but returned {v}"),
        Err(e) => e,
    }
}

/// A draft `news` collection with six fields.
pub fn news(app: &Shared) {
    ok(app, &builder(), "create_collection", json!({ "name": "news", "title": "新闻", "note": "建新闻", "fields": [
        { "name": "title", "title": "标题", "type": "text", "required": true },
        { "name": "slug", "title": "网址名", "type": "text", "unique": true },
        { "name": "body", "title": "正文", "type": "richtext" },
        { "name": "date", "title": "日期", "type": "date", "index": true },
        { "name": "tag", "title": "标签", "type": "select", "options": ["公司", "行业"], "index": true },
        { "name": "cover", "title": "封面", "type": "media" }
    ] }));
}

/// Publishes everything pending, as the builder.
pub fn publish_all(app: &Shared) -> Value {
    ok(app, &builder(), "publish", json!({ "note": "发布" }))
}

use axum::http::{header, HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn send(app: &Shared, req: axum::http::request::Builder, key: Option<&str>, body: axum::body::Body) -> (StatusCode, HeaderMap, Value) {
    let req = match key {
        Some(k) => req.header(header::AUTHORIZATION, format!("Bearer {k}")),
        None => req,
    };
    let resp = harness_cms::api::router(app.clone()).oneshot(req.body(body).unwrap()).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

pub async fn get(app: &Shared, path: &str, key: Option<&str>) -> (StatusCode, HeaderMap, Value) {
    send(app, Request::get(path), key, axum::body::Body::empty()).await
}

pub async fn post_json(app: &Shared, path: &str, key: Option<&str>, body: Value) -> (StatusCode, Value) {
    let req = Request::post(path).header(header::CONTENT_TYPE, "application/json");
    let (s, _, v) = send(app, req, key, axum::body::Body::from(body.to_string())).await;
    (s, v)
}

pub fn key(app: &Shared, scopes: &[Scope]) -> String {
    harness_cms::auth::create_key(&app.conn(), "test", scopes).unwrap()
}

pub async fn post_raw(app: &Shared, path: &str, key: Option<&str>, body: &str) -> (StatusCode, Value) {
    let req = Request::post(path).header(header::CONTENT_TYPE, "application/json");
    let (s, _, v) = send(app, req, key, axum::body::Body::from(body.to_string())).await;
    (s, v)
}

/// The body of a GET as text.
pub async fn get_text(app: &Shared, path: &str) -> String {
    let resp = harness_cms::api::router(app.clone()).oneshot(Request::get(path).body(axum::body::Body::empty()).unwrap()).await.unwrap();
    String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
}

// ---- admin UI helpers ----

async fn send_text(app: &Shared, req: Request<axum::body::Body>) -> (StatusCode, HeaderMap, String) {
    let resp = harness_cms::api::router(app.clone()).oneshot(req).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}

/// Creates an admin `<role>@test` with password `secret-pass` and logs in through the route.
/// Returns the `hc_admin=<token>` cookie pair and the session's CSRF token.
pub async fn admin_session(app: &Shared, role: &str) -> (String, String) {
    let email = format!("{role}@test");
    harness_cms::auth::create_admin(&app.conn(), &email, role, Some("secret-pass")).unwrap();
    let (status, headers, body) = post_form(app, "/admin/login", None, &[("email", &email), ("password", "secret-pass")]).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "login failed: {body}");
    let cookie = headers[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
    let mut h = HeaderMap::new();
    h.insert(header::COOKIE, cookie.parse().unwrap());
    let csrf = harness_cms::web::session::from_cookie(&app.conn(), &h).unwrap().csrf;
    (cookie, csrf)
}

/// GET with an optional `Cookie` header; the body as text.
pub async fn get_with(app: &Shared, path: &str, cookie: Option<&str>) -> (StatusCode, HeaderMap, String) {
    let mut req = Request::get(path);
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    send_text(app, req.body(axum::body::Body::empty()).unwrap()).await
}

fn form_encode(fields: &[(&str, &str)]) -> String {
    let enc = |s: &str| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
    fields.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

/// A form-urlencoded POST. `session` is `(cookie, csrf)` from `admin_session`: it sends the cookie and adds a `csrf` field.
pub async fn post_form(app: &Shared, path: &str, session: Option<&(String, String)>, fields: &[(&str, &str)]) -> (StatusCode, HeaderMap, String) {
    post_form_with(app, path, session, fields, &[]).await
}

/// `post_form` with extra request headers.
pub async fn post_form_with(app: &Shared, path: &str, session: Option<&(String, String)>, fields: &[(&str, &str)], extra: &[(&str, &str)]) -> (StatusCode, HeaderMap, String) {
    let mut all = fields.to_vec();
    let mut req = Request::post(path).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some((cookie, csrf)) = session {
        req = req.header(header::COOKIE, cookie.as_str());
        all.push(("csrf", csrf.as_str()));
    }
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    send_text(app, req.body(axum::body::Body::from(form_encode(&all))).unwrap()).await
}
