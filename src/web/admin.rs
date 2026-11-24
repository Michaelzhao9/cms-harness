//! Admin pages: the shared nav, the shell's home page and the nav preference.

use super::page;
use super::session::{self, AdminUser};
use crate::Shared;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Form};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};

/// Collections (with live record count and pending flag) and their views, newest data, one query each.
fn nav_collections(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    // Pending: the definition has a draft or a pending deletion, or any of its records does.
    // Counted like `changes::record_count`: drafts count, tombstones do not.
    let mut stmt = conn.prepare(
        "SELECT c.name, json_extract(coalesce(c.draft, c.published), '$.title'),
                coalesce(sum(r.published IS NOT NULL OR r.draft IS NOT NULL), 0),
                c.draft IS NOT NULL OR c.draft_deleted = 1 OR coalesce(max(r.draft IS NOT NULL OR r.draft_deleted = 1), 0)
         FROM collections c LEFT JOIN records r ON r.collection = c.name
         WHERE c.deleted = 0 OR c.draft IS NOT NULL
         GROUP BY c.name ORDER BY c.name",
    )?;
    let mut out: Vec<Value> = stmt
        .query_map([], |r| {
            let name: String = r.get(0)?;
            let title: Option<String> = r.get(1)?;
            Ok(json!({ "name": name, "title": title.unwrap_or_else(|| name.clone()), "count": r.get::<_, i64>(2)?,
                "pending": r.get::<_, bool>(3)?, "views": [] }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn.prepare(
        "SELECT name, json_extract(coalesce(draft, published), '$.title'), json_extract(coalesce(draft, published), '$.collection')
         FROM views WHERE published IS NOT NULL OR draft IS NOT NULL ORDER BY name",
    )?;
    let views = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?)))?;
    for view in views {
        let (name, title, collection) = view?;
        // A view over a collection that is not listed (gone, or the members pseudo-collection) has no place here.
        if let Some(c) = out.iter_mut().find(|c| collection.as_deref() == c["name"].as_str()) {
            c["views"].as_array_mut().expect("views is an array").push(json!({ "title": title.unwrap_or_else(|| name.clone()), "name": name }));
        }
    }
    Ok(out)
}

/// The nav context every admin page passes to `base.html`. `current` names the page: `web`, `c:<name>`, `v:<view>`,
/// `media`, `releases` or `settings`.
pub fn nav(conn: &Connection, user: &AdminUser, current: &str) -> Value {
    json!({
        "current": current,
        "collapsed": user.prefs["nav"] == "collapsed",
        // Task 11 turns this on with member login.
        "members": false,
        // A database error leaves the nav without collections rather than failing the page.
        "collections": nav_collections(conn).unwrap_or_default(),
    })
}

/// The workbench: chat column and the site preview.
pub async fn home(State(app): State<Shared>, Extension(user): Extension<AdminUser>) -> Response {
    let a = app.clone();
    let u = user.clone();
    let nav = tokio::task::spawn_blocking(move || nav(&a.conn(), &u, "web")).await.unwrap_or(Value::Null);
    page(StatusCode::OK, "home.html", json!({ "site_name": app.site_name, "origin": app.origin, "csrf": user.csrf, "nav": nav }))
}

#[derive(Deserialize)]
pub struct PrefsForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    nav: String,
}

/// The path to go back to: the `Referer` when it is one of our own `/admin` pages, else `/admin`.
fn back_to(origin: &str, headers: &HeaderMap) -> String {
    headers
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.strip_prefix(origin))
        .filter(|p| *p == "/admin" || p.starts_with("/admin/") || p.starts_with("/admin?"))
        .unwrap_or("/admin")
        .to_string()
}

pub async fn prefs(State(app): State<Shared>, Extension(user): Extension<AdminUser>, headers: HeaderMap, Form(f): Form<PrefsForm>) -> Response {
    if let Err(s) = session::check_csrf(&user, &f.csrf) {
        return (s, "表单已失效，请刷新页面后再试。").into_response();
    }
    if !matches!(f.nav.as_str(), "collapsed" | "open") {
        return (StatusCode::BAD_REQUEST, "导航只能收起（collapsed）或展开（open）。").into_response();
    }
    let a = app.clone();
    let saved = tokio::task::spawn_blocking(move || session::set_pref(&a.conn(), &user.token_hash, "nav", &f.nav))
        .await
        .unwrap_or_else(|e| Err(format!("服务器内部错误：{e}")));
    match saved {
        Ok(()) => Redirect::to(&back_to(&app.origin, &headers)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}
