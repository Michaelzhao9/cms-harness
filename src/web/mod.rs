//! The admin web UI: router, session gate, Origin check and security headers.

pub mod session;

use crate::{render, Shared};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Form, Router};
use serde::Deserialize;
use serde_json::json;
use session::AdminUser;
use std::net::SocketAddr;

pub const ADMIN_CSP: &str =
    "default-src 'none'; style-src 'self'; img-src 'self' https: data:; frame-src 'self'; form-action 'self'; frame-ancestors 'self'; base-uri 'none'";

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/admin", get(home))
        .route("/admin/login", get(login_page).post(login))
        .route("/admin/logout", post(logout))
        .route("/admin/static/admin.css", get(css))
        .with_state(app)
}

/// Every POST under `/admin` must come from our own origin when it says where it comes from.
/// Every `/admin` path except the login page and static files needs a session; the user lands in the request extensions.
/// Mounted in `api::router` around everything, so routes merged by later tasks are covered too.
pub async fn guard(State(app): State<Shared>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if !(path == "/admin" || path.starts_with("/admin/")) {
        return next.run(req).await;
    }
    if req.method() == Method::POST {
        if let Some(origin) = req.headers().get(header::ORIGIN) {
            if origin.as_bytes() != app.origin.as_bytes() {
                return (StatusCode::FORBIDDEN, "请求来源与站点地址（HC_ORIGIN）不一致，已拒绝。请用 HC_ORIGIN 里的地址打开后台后再提交。").into_response();
            }
        }
    }
    if path == "/admin/login" || path.starts_with("/admin/static/") {
        return next.run(req).await;
    }
    let headers = req.headers().clone();
    let a = app.clone();
    match tokio::task::spawn_blocking(move || session::from_cookie(&a.conn(), &headers)).await.ok().flatten() {
        Some(user) => {
            req.extensions_mut().insert(user);
            next.run(req).await
        }
        None if matches!(*req.method(), Method::GET | Method::HEAD) => Redirect::to("/admin/login").into_response(),
        None => (StatusCode::UNAUTHORIZED, "没有登录或登录已过期，请重新登录后再提交。").into_response(),
    }
}

/// Headers on every response; the admin CSP on `/admin` pages; HSTS on https.
pub async fn security_headers(State(app): State<Shared>, req: Request, next: Next) -> Response {
    let admin = req.uri().path().starts_with("/admin");
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("strict-origin-when-cross-origin"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
    h.insert("permissions-policy", HeaderValue::from_static("camera=(), microphone=(), geolocation=()"));
    if app.secure_cookies {
        h.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
    }
    if admin {
        h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(ADMIN_CSP));
    }
    resp
}

/// Who is calling, for rate limiting: the peer address, or with `trust_proxy` the address the proxy in front
/// appended last to `X-Forwarded-For` (earlier entries are whatever the client sent).
pub fn client_key(headers: &HeaderMap, peer: Option<SocketAddr>, trust_proxy: bool) -> String {
    let forwarded = || headers.get("x-forwarded-for")?.to_str().ok()?.rsplit(',').next().map(str::trim).filter(|s| !s.is_empty());
    match (trust_proxy, forwarded()) {
        (true, Some(ip)) => ip.to_string(),
        _ => peer.map_or_else(|| "unknown".into(), |a| a.ip().to_string()),
    }
}

fn page(status: StatusCode, name: &str, ctx: serde_json::Value) -> Response {
    match render::admin_page(name, ctx) {
        Ok(html) => (status, Html(html)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn css() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], include_str!("admin.css")).into_response()
}

async fn login_page(State(app): State<Shared>) -> Response {
    page(StatusCode::OK, "login.html", json!({ "site_name": app.site_name }))
}

#[derive(Deserialize)]
struct LoginForm {
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
}

async fn login(State(app): State<Shared>, peer: Option<Extension<ConnectInfo<SocketAddr>>>, headers: HeaderMap, Form(f): Form<LoginForm>) -> Response {
    let trust = std::env::var("HC_TRUST_PROXY").is_ok_and(|v| v == "1");
    let client = client_key(&headers, peer.map(|Extension(ConnectInfo(a))| a), trust);
    let a = app.clone();
    let done = tokio::task::spawn_blocking(move || session::login(&a.conn(), &f.email, &f.password, &client))
        .await
        .unwrap_or_else(|e| Err(format!("服务器内部错误：{e}")));
    match done {
        Ok(token) => {
            let cookie = session::session_cookie(&token, app.secure_cookies);
            ([(header::SET_COOKIE, cookie)], Redirect::to("/admin")).into_response()
        }
        Err(e) => {
            let status = match e.as_str() {
                session::LOCKED => StatusCode::TOO_MANY_REQUESTS,
                session::BAD_LOGIN => StatusCode::UNAUTHORIZED,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            page(status, "login.html", json!({ "site_name": app.site_name, "error": e }))
        }
    }
}

#[derive(Deserialize)]
struct CsrfForm {
    #[serde(default)]
    csrf: String,
}

async fn logout(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Form(f): Form<CsrfForm>) -> Response {
    if let Err(s) = session::check_csrf(&user, &f.csrf) {
        return (s, "表单已失效，请刷新页面后再试。").into_response();
    }
    let a = app.clone();
    let _ = tokio::task::spawn_blocking(move || session::logout(&a.conn(), &user.token_hash)).await;
    ([(header::SET_COOKIE, session::clear_cookie(app.secure_cookies))], Redirect::to("/admin/login")).into_response()
}

/// Placeholder admin home until the workbench lands.
async fn home(State(app): State<Shared>, Extension(user): Extension<AdminUser>) -> Response {
    page(StatusCode::OK, "home.html", json!({ "site_name": app.site_name, "email": user.email, "csrf": user.csrf }))
}
