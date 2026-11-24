//! The admin web UI: router, session gate, Origin check and security headers.

pub mod address;
pub mod members;
pub mod admin;
pub mod chat;
pub mod content;
pub mod session;
pub mod settings;
pub mod site;

use crate::{render, App, Shared};
use address::Address;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Form, Router};
use serde::Deserialize;
use serde_json::json;
use session::AdminUser;
use std::net::SocketAddr;

pub const ADMIN_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' https: data:; connect-src 'self'; frame-src 'self'; form-action 'self'; frame-ancestors 'self'; base-uri 'none'";

/// The admin's mark, also its favicon.
pub const LOGO_SVG: &str = include_str!("logo.svg");

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/admin", get(chat::home))
        .route("/admin/prefs", post(admin::prefs))
        .route("/admin/chat", post(chat::post).layer(DefaultBodyLimit::max(crate::media::UPLOAD_BODY_LIMIT)))
        .route("/admin/chat/new", post(chat::new))
        .route("/admin/chat/stop", post(chat::stop))
        .route("/admin/chat/events", get(chat::events))
        .route("/admin/c/{name}", get(content::collection_list))
        .route("/admin/c/{name}/new", post(content::create).layer(DefaultBodyLimit::max(crate::media::UPLOAD_BODY_LIMIT)))
        .route("/admin/v/{name}", get(content::view_list))
        .route("/admin/members", get(content::members_list))
        .route("/admin/members/{id}/status", post(content::member_status))
        .route("/admin/r/{id}", get(content::record).post(content::save).layer(DefaultBodyLimit::max(crate::media::UPLOAD_BODY_LIMIT)))
        .route("/admin/r/{id}/delete", post(content::delete))
        .route("/admin/r/{id}/discard", post(content::discard))
        .route("/admin/media", get(content::media_library).post(content::upload).layer(DefaultBodyLimit::max(crate::media::UPLOAD_BODY_LIMIT)))
        .route("/admin/publish", get(admin::publish_review).post(admin::publish))
        .route("/admin/releases", get(admin::release_history))
        .route("/admin/restore", post(admin::restore))
        .route("/admin/settings", get(settings::page))
        .route("/admin/settings/keys", post(settings::create_key))
        .route("/admin/settings/keys/{id}/revoke", post(settings::revoke_key))
        .route("/admin/settings/notes", post(settings::save_notes))
        .route("/admin/settings/password", post(settings::change_password))
        .route("/admin/settings/admins", post(settings::add_admin))
        .route("/admin/settings/admins/{id}/reset", post(settings::reset_admin))
        .route("/admin/settings/admins/{id}/delete", post(settings::remove_admin))
        .route("/admin/settings/appearance", post(settings::appearance))
        .route("/admin/settings/model", post(settings::save_model))
        .route("/admin/settings/model/detect", post(settings::detect_models))
        .route("/admin/settings/skills/install", post(settings::install_skill).layer(DefaultBodyLimit::max(settings::SKILL_BODY_LIMIT)))
        .route("/admin/settings/skills/{name}/{action}", post(settings::skill_action))
        .route("/admin/settings/address", post(address::switch))
        .route("/admin/login", get(login_page).post(login))
        .route("/admin/logout", post(logout))
        .route("/admin/static/admin.css", get(css))
        .route("/admin/static/admin.js", get(js))
        .route("/admin/static/logo.svg", get(logo))
        .route("/_hc/preview.js", get(site::preview_js))
        .route("/_hc/preview.css", get(site::preview_css))
        .route("/_hc/preview/", get(site::preview))
        .route("/_hc/preview/{*rest}", get(site::preview))
        .route("/_hc/ping", get(address::ping))
        .route("/account/{which}", get(site::serve).post(members::form))
        .route("/{collection}/new", get(site::serve).post(members::submit_form).layer(DefaultBodyLimit::max(crate::access::MAX_SUBMIT_BYTES)))
        .route("/api/v1/members/me", get(members::me))
        .route("/api/v1/members/{which}", post(members::api))
        .fallback(site::serve)
        .with_state(app)
}

/// Every browser POST must come from our own origin when it says where it comes from.
/// Every `/admin` path except the login page and static files needs a session; the user lands in the request extensions.
/// Mounted in `api::router` around everything, so routes merged by later tasks are covered too.
pub async fn guard(State(app): State<Shared>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path();
    // Every browser POST (admin, member forms, site submissions) must come from the address it was sent to when the
    // browser says where it comes from. Only the bearer-key API is exempt: keys, not cookies, authorise it.
    let keyed = ["/api/v1/tools", "/api/v1/site", "/api/v1/media"].iter().any(|p| path == *p || path.starts_with(&format!("{p}/"))) || path == "/mcp";
    if req.method() == Method::POST && !keyed {
        let here = req.extensions().get::<Address>().map_or(app.origin.as_str(), |a| a.here.as_str());
        if let Some(origin) = req.headers().get(header::ORIGIN) {
            if origin.as_bytes() == b"null" {
                return (StatusCode::FORBIDDEN, "这里是后台里隔离的预览框，不能提交表单：请在新标签页打开网站再提交。").into_response();
            }
            if origin.as_bytes() != here.as_bytes() {
                return (StatusCode::FORBIDDEN, format!("请求来源与这个网址（{here}）不一致，已拒绝。请在这个网址的页面上提交。")).into_response();
            }
        }
    }
    if !(path == "/admin" || path.starts_with("/admin/")) {
        return next.run(req).await;
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

/// The front door, around everything. Reads where the admin lives, routes by Host in 分开 mode
/// (`address::route`), hands the `Address` to the handlers, and sets the headers every response gets: the admin
/// CSP on `/admin` pages, HSTS on https.
pub async fn security_headers(State(app): State<Shared>, mut req: Request, next: Next) -> Response {
    // ponytail: one small query per request, so `cms-harness admin-origin` applies to a running server at once;
    // cache it for a second if this ever shows up in a profile.
    let a = app.clone();
    let admin = tokio::task::spawn_blocking(move || address::current(&a)).await.ok().flatten();
    let path = req.uri().path().to_string();
    let addr = match address::route(&app, admin, &req) {
        Ok(addr) => addr,
        Err(resp) => return headers(&app, &Address { admin: None, here: app.origin.clone() }, &path, *resp),
    };
    req.extensions_mut().insert(addr.clone());
    let resp = next.run(req).await;
    headers(&app, &addr, &path, resp)
}

fn headers(app: &App, addr: &Address, path: &str, mut resp: Response) -> Response {
    let h = resp.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("strict-origin-when-cross-origin"));
    // In 分开 mode the admin frames the live site from its own origin: there the CSP's frame-ancestors decides
    // (it names the admin), and X-Frame-Options, which can only say "same origin", is left out.
    let csp_frames = h.get(header::CONTENT_SECURITY_POLICY).is_some_and(|v| v.to_str().unwrap_or_default().contains("frame-ancestors"));
    if addr.admin.is_none() || !csp_frames {
        h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
    }
    h.insert("permissions-policy", HeaderValue::from_static("camera=(), microphone=(), geolocation=()"));
    if app.secure_cookies || addr.secure() {
        h.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
    }
    if path.starts_with("/admin") {
        // 分开: the 线上 view frames the site's origin.
        let csp = match &addr.admin {
            Some(_) => HeaderValue::from_str(&ADMIN_CSP.replace("frame-src 'self';", &format!("frame-src 'self' {};", app.origin))).ok(),
            None => None,
        };
        h.insert(header::CONTENT_SECURITY_POLICY, csp.unwrap_or(HeaderValue::from_static(ADMIN_CSP)));
    }
    resp
}

/// Who is calling, for rate limiting: the peer address, or with `trust_proxy` the address the proxy in front
/// appended last to `X-Forwarded-For` (earlier entries are whatever the client sent).
pub fn client_key(headers: &HeaderMap, peer: Option<SocketAddr>, trust_proxy: bool) -> String {
    let forwarded = || headers.get("x-forwarded-for")?.to_str().ok()?.rsplit(',').next().map(str::trim).filter(|s| !s.is_empty());
    // The server listens on loopback only, so a loopback peer is the reverse proxy in front: the address it appended
    // last is the visitor. Without this every visitor would share one rate-limit key.
    let via_local_proxy = peer.is_some_and(|p| p.ip().is_loopback());
    match (trust_proxy || via_local_proxy, forwarded()) {
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
    // Pages link it with `?v=<content hash>`, so it can be cached for good.
    let headers = [(header::CONTENT_TYPE, "text/css; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")];
    (headers, render::ADMIN_CSS).into_response()
}

async fn js() -> Response {
    let headers = [(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")];
    (headers, render::ADMIN_JS).into_response()
}

async fn logo() -> Response {
    ([(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "public, max-age=86400")], LOGO_SVG).into_response()
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

async fn login(State(app): State<Shared>, Extension(addr): Extension<Address>, peer: Option<Extension<ConnectInfo<SocketAddr>>>, headers: HeaderMap, Form(f): Form<LoginForm>) -> Response {
    let trust = std::env::var("HC_TRUST_PROXY").is_ok_and(|v| v == "1");
    let client = client_key(&headers, peer.map(|Extension(ConnectInfo(a))| a), trust);
    let a = app.clone();
    // Argon2 runs between the two short database steps, never under the lock, and only a few at a time.
    let permit = crate::auth::HASHING.clone().acquire_owned().await;
    let done = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let attempt = session::login_begin(&a.conn(), &f.email, &client)?;
        let ok = crate::auth::verify_password(attempt.found.as_ref().map_or(session::dummy_hash(), |x| x.1.as_str()), &f.password);
        session::login_finish(&a.conn(), &f.email, attempt, ok)
    })
        .await
        .unwrap_or_else(|e| Err(format!("服务器内部错误：{e}")));
    match done {
        Ok(token) => {
            // Host-only (no Domain): in 分开 mode the cookie stays on the admin's host.
            let cookie = session::session_cookie(&token, addr.secure());
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

async fn logout(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Extension(addr): Extension<Address>, Form(f): Form<CsrfForm>) -> Response {
    if let Err(s) = session::check_csrf(&user, &f.csrf) {
        return (s, "表单已失效，请刷新页面后再试。").into_response();
    }
    crate::chat::reset(&app, &user.token_hash);
    let a = app.clone();
    let _ = tokio::task::spawn_blocking(move || session::logout(&a.conn(), &user.token_hash)).await;
    ([(header::SET_COOKIE, session::clear_cookie(addr.secure()))], Redirect::to("/admin/login")).into_response()
}
