//! Server functions over HTTP: `/…/_fn/<name>` runs the published script `…/_fn/<name>.js`.

use crate::func::{self, Call};
use crate::site::Mode;
use crate::{App, Shared};
use axum::extract::Request;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use rusqlite::Connection;
use serde_json::{json, Value};

fn answer(status: u16, body: &Value) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    let headers = [(header::CONTENT_TYPE, "application/json; charset=utf-8"), (header::CACHE_CONTROL, "no-store")];
    (status, headers, body.to_string()).into_response()
}

fn refuse(status: StatusCode, why: &str) -> Response {
    answer(status.as_u16(), &json!({ "error": why }))
}

/// `a=1&b=%E5%B0%8F` as an object of text.
fn form(text: &str) -> Value {
    let decode = |s: &str| percent_encoding::percent_decode_str(&s.replace('+', " ")).decode_utf8_lossy().into_owned();
    text.split('&').filter(|p| !p.is_empty()).map(|p| p.split_once('=').unwrap_or((p, ""))).map(|(k, v)| (decode(k), json!(decode(v)))).collect::<serde_json::Map<_, _>>().into()
}

/// The body as the function sees it, by its content type; or why it cannot be read.
fn body_of(headers: &HeaderMap, bytes: &[u8]) -> Result<Value, &'static str> {
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    let kind = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default().to_ascii_lowercase();
    let text = std::str::from_utf8(bytes).map_err(|_| "请求的内容不是 UTF-8 文字。")?;
    if kind.starts_with("application/x-www-form-urlencoded") {
        Ok(form(text))
    } else if kind.starts_with("application/json") || kind.is_empty() {
        serde_json::from_str(text).map_err(|_| "请求的内容不是有效的 JSON。")
    } else {
        Ok(json!(text))
    }
}

/// Runs the function a request names and answers for it. `mode`: the live site runs published scripts and keeps
/// what they write; the admin's preview runs the drafts as trials.
pub fn respond(conn: &Connection, mode: Mode, call: Call) -> Response {
    let rel = call.path.trim_start_matches('/');
    // An 端 only members may enter: its functions are behind the same door as its pages.
    if mode == Mode::Live {
        match crate::ends::door(conn, &crate::ends::load(conn, false), rel, call.member.as_ref()) {
            crate::ends::Door::Open => {}
            crate::ends::Door::Login => return refuse(StatusCode::UNAUTHORIZED, "请先登录。"),
            crate::ends::Door::Refused => return refuse(StatusCode::FORBIDDEN, "你的账号不能用这个功能。"),
        }
    }
    let Some((name, source)) = func::file_of(conn, mode, &call.path) else { return refuse(StatusCode::NOT_FOUND, "没有这个功能。") };
    let out = func::run(conn, &name, &source, &call, mode == Mode::Preview);
    if mode == Mode::Live {
        func::record(conn, &call, &out);
    }
    match (&out.error, mode) {
        // The script's own words go to the log (and to the builder in preview), never to a visitor.
        (Some(_), Mode::Live) => refuse(StatusCode::INTERNAL_SERVER_ERROR, func::FAILED),
        (Some(e), Mode::Preview) => answer(500, &json!({ "error": e, "logs": out.logs })),
        (None, _) => answer(out.status, &out.body),
    }
}

/// A live request to a function: any method, a small body, the member from their cookie.
pub async fn live(app: Shared, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, func::MAX_BODY).await else {
        return refuse(StatusCode::PAYLOAD_TOO_LARGE, "请求的内容太大了。");
    };
    let body = match body_of(&parts.headers, &bytes) {
        Ok(b) => b,
        Err(why) => return refuse(StatusCode::BAD_REQUEST, why),
    };
    let query = axum::extract::Query::<Vec<(String, String)>>::try_from_uri(&parts.uri).map(|q| q.0).unwrap_or_default();
    let token = super::members::session_token(&parts.headers);
    let (method, path) = (parts.method.as_str().to_string(), parts.uri.path().to_string());
    tokio::task::spawn_blocking(move || {
        let conn = app.conn();
        let member = member(&app, &conn, token.as_deref());
        respond(&conn, Mode::Live, Call { method, path, query, body, member })
    })
    .await
    .unwrap_or_else(|e| refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("服务器内部错误：{e}")))
}

/// The logged-in member behind a cookie, while member login is live.
fn member(_app: &App, conn: &Connection, token: Option<&str>) -> Option<Value> {
    token.filter(|_| crate::members::login_enabled(conn, false)).and_then(|t| crate::members::from_token(conn, t)).map(|m| crate::members::member_json(&m))
}
