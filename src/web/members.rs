//! The website's member sign-up, login and logout: HTML forms at /account/* and JSON at /api/v1/members/*.
//! Both answer only while member login is live. Cross-site posts are already refused by the Origin check, and the
//! member cookie is SameSite=Lax, so no other site can act with it.

use super::address::Address;
use super::{client_key, session, site};
use crate::members::{self, COOKIE};
use crate::Shared;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Form, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;

#[derive(Deserialize, Default)]
pub struct AccountForm {
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    next: String,
}

fn cookie(token: &str, secure: bool) -> String {
    format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000{}", if secure { "; Secure" } else { "" })
}

fn clear_cookie(secure: bool) -> String {
    format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}", if secure { "; Secure" } else { "" })
}

fn client(headers: &HeaderMap, peer: Option<Extension<ConnectInfo<SocketAddr>>>) -> String {
    let trust = std::env::var("HC_TRUST_PROXY").is_ok_and(|v| v == "1");
    client_key(headers, peer.map(|Extension(ConnectInfo(a))| a), trust)
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "这个网站没有开放会员登录。" }))).into_response()
}

fn status_of(e: &str) -> StatusCode {
    match e {
        members::LOCKED => StatusCode::TOO_MANY_REQUESTS,
        members::BAD_LOGIN => StatusCode::UNAUTHORIZED,
        _ if e.contains("太频繁") => StatusCode::TOO_MANY_REQUESTS,
        _ if e.contains("停用") => StatusCode::FORBIDDEN,
        _ if e.contains("已经注册") => StatusCode::CONFLICT,
        _ if e.starts_with("读取数据库") => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_REQUEST,
    }
}

/// Runs sign-up or login: Ok(token) or Err(message), None when member login is off.
async fn enter(app: &Shared, which: &str, f: &AccountForm, who: String) -> Option<Result<String, String>> {
    let (a, which) = (app.clone(), which.to_string());
    let (email, password, name) = (f.email.clone(), f.password.clone(), f.name.clone());
    tokio::task::spawn_blocking(move || {
        let conn = a.conn();
        if !members::login_enabled(&conn, false) {
            return None;
        }
        Some(if which == "signup" { members::signup(&conn, &email, &password, &name, &who) } else { members::login(&conn, &email, &password, &who) })
    })
    .await
    .unwrap_or_else(|e| Some(Err(format!("服务器内部错误：{e}"))))
}

/// `POST /account/login` and `/account/signup` (form posts from the site).
pub async fn form(
    State(app): State<Shared>,
    Path(which): Path<String>,
    Extension(addr): Extension<Address>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    Form(f): Form<AccountForm>,
) -> Response {
    if which == "logout" {
        return logout_form(app, addr, headers).await;
    }
    if which != "login" && which != "signup" {
        return StatusCode::NOT_FOUND.into_response();
    }
    match enter(&app, &which, &f, client(&headers, peer)).await {
        None => StatusCode::NOT_FOUND.into_response(),
        Some(Ok(token)) => {
            let next = site::safe_next(&f.next);
            ([(header::SET_COOKIE, cookie(&token, addr.secure()))], Redirect::to(if next.is_empty() { "/" } else { &next })).into_response()
        }
        Some(Err(e)) => {
            let a = app.clone();
            let form = json!({ "error": e, "email": f.email, "name": f.name, "next": site::safe_next(&f.next) });
            let status = status_of(&e);
            tokio::task::spawn_blocking(move || site::account_page(&a, &a.conn(), &which, form, status)).await.unwrap_or_else(|e| e.to_string().into_response())
        }
    }
}

async fn logout_form(app: Shared, addr: Address, headers: HeaderMap) -> Response {
    if let Some(t) = session::cookie_value(&headers, COOKIE) {
        let _ = tokio::task::spawn_blocking(move || members::logout(&app.conn(), &t)).await;
    }
    ([(header::SET_COOKIE, clear_cookie(addr.secure()))], Redirect::to("/")).into_response()
}

/// `POST /api/v1/members/{signup|login|logout}` with a JSON body; `GET /api/v1/members/me`.
pub async fn api(
    State(app): State<Shared>,
    Path(which): Path<String>,
    Extension(addr): Extension<Address>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    let a = app.clone();
    let on = tokio::task::spawn_blocking(move || members::login_enabled(&a.conn(), false)).await.unwrap_or(false);
    if !on {
        return not_found();
    }
    match which.as_str() {
        "logout" => {
            if let Some(t) = session::cookie_value(&headers, COOKIE) {
                let _ = tokio::task::spawn_blocking(move || members::logout(&app.conn(), &t)).await;
            }
            ([(header::SET_COOKIE, clear_cookie(addr.secure()))], Json(json!({ "ok": true }))).into_response()
        }
        "signup" | "login" => {
            let v = body.map(|Json(v)| v).unwrap_or_default();
            let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
            let f = AccountForm { email: s("email"), password: s("password"), name: s("name"), next: String::new() };
            match enter(&app, &which, &f, client(&headers, peer)).await {
                None => not_found(),
                Some(Ok(token)) => {
                    let a = app.clone();
                    let me = tokio::task::spawn_blocking(move || members::from_token(&a.conn(), &token).map(|m| (members::member_json(&m), token))).await.ok().flatten();
                    match me {
                        Some((m, token)) => ([(header::SET_COOKIE, cookie(&token, addr.secure()))], Json(json!({ "ok": true, "member": m }))).into_response(),
                        None => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "ok": false, "error": "登录后没找到这个会员。" }))).into_response(),
                    }
                }
                Some(Err(e)) => (status_of(&e), Json(json!({ "ok": false, "error": e }))).into_response(),
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn me(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let token = session::cookie_value(&headers, COOKIE);
    let found = tokio::task::spawn_blocking(move || {
        let conn = app.conn();
        if !members::login_enabled(&conn, false) {
            return None;
        }
        Some(token.and_then(|t| members::from_token(&conn, &t)).map(|m| members::member_json(&m)))
    })
    .await
    .ok()
    .flatten();
    match found {
        None => not_found(),
        Some(m) => ([(header::CACHE_CONTROL, "no-store")], Json(json!({ "ok": true, "member": m }))).into_response(),
    }
}
