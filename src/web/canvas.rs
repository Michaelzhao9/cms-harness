//! The preview's canvas: saving what an admin changes by hand. The preview frame only reports what was pointed
//! at; the admin page sends the edit here with its CSRF token, and everything in it is checked as if it came
//! from anyone: the element must exist in the file's draft, properties and values come from fixed lists, text
//! is escaped. Edits are drafts like any other change.

use super::content::{blocking, internal, Out};
use super::session::{self, AdminUser};
use crate::canvas;
use crate::changes::{self, Target};
use crate::site::{self, Mode};
use crate::Shared;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::{Extension, Form};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};

const NOTE: &str = "在画板上手动调整";

#[derive(Deserialize)]
pub struct StyleForm {
    csrf: String,
    src: String,
    #[serde(default = "all")]
    tier: String,
    prop: String,
    /// Empty clears the property.
    #[serde(default)]
    value: String,
}

fn all() -> String {
    "all".into()
}

#[derive(Deserialize)]
pub struct TextForm {
    csrf: String,
    src: String,
    text: String,
}

fn bad(msg: impl Into<String>) -> Out {
    Out::Fail(StatusCode::BAD_REQUEST, msg.into())
}

/// The draft source of the element named by `src` and its tag, or why it cannot be edited.
fn element(conn: &Connection, src: &str) -> Result<(String, String, canvas::Tag), Out> {
    let (path, n) = canvas::parse_src(src).ok_or_else(|| bad("不认识这个元素：请刷新预览再选。"))?;
    let source = site::file(conn, Mode::Preview, path).map_err(internal)?.ok_or_else(|| bad("这个元素所在的文件已经不在了：请刷新预览。"))?;
    let tag = canvas::tags(&source).into_iter().nth(n - 1).ok_or_else(|| bad("这个元素已经不在文件里了：请刷新预览。"))?;
    Ok((path.to_string(), source, tag))
}

/// Saves the canvas document as a draft, or drops the draft when it matches what is live.
fn save_canvas(conn: &Connection, user: &AdminUser, doc: &Value) -> Result<(), Out> {
    let target = Target::Setting(canvas::KEY.into());
    let empty = |v: &Value| v["els"].as_object().is_none_or(|m| m.is_empty());
    let live = changes::setting(conn, canvas::KEY, false);
    if live == *doc || (empty(&live) && empty(doc)) {
        return changes::discard(conn, &target).map_err(internal);
    }
    changes::save_draft(conn, &user.actor(), &target, doc, NOTE).map_err(internal)
}

fn done(conn: &Connection) -> Result<Out, Out> {
    let pending = changes::pending_order(conn).map_err(internal)?.len();
    Ok(Out::Json(json!({ "pending": pending })))
}

fn allowed(user: &AdminUser, csrf: &str) -> Result<(), Out> {
    session::check_csrf(user, csrf).map_err(|s| Out::Fail(s, "表单已失效，请刷新页面后再试。".into()))?;
    if !user.actor().can(crate::auth::Scope::Structure) {
        return Err(Out::Fail(StatusCode::FORBIDDEN, "你的角色不能改网页。".into()));
    }
    Ok(())
}

/// Sets or clears one style property of one element, for one width tier.
pub async fn style(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Form(f): Form<StyleForm>) -> Response {
    blocking(move || {
        allowed(&user, &f.csrf)?;
        let conn = app.conn();
        let (_, source, tag) = element(&conn, &f.src)?;
        let fp = canvas::fingerprint(&source, &tag);
        let doc = changes::setting(&conn, canvas::KEY, true);
        let value = Some(f.value.as_str()).filter(|v| !v.trim().is_empty());
        let doc = canvas::set_style(&doc, &f.src, &fp, &f.tier, &f.prop, value).map_err(bad)?;
        save_canvas(&conn, &user, &doc)?;
        done(&conn)
    })
    .await
}

/// Replaces the text of one element whose text is written in its template.
pub async fn text(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Form(f): Form<TextForm>) -> Response {
    blocking(move || {
        allowed(&user, &f.csrf)?;
        let conn = app.conn();
        let (path, source, tag) = element(&conn, &f.src)?;
        let new = canvas::set_text(&source, tag.n, &f.text).map_err(bad)?;
        site::check_file(&path, &new).map_err(bad)?;
        changes::stage_file(&conn, &user.actor(), &path, &new, NOTE).map_err(internal)?;
        // The element's fingerprint includes its text: style edits on it keep following it.
        let doc = changes::setting(&conn, canvas::KEY, true);
        if doc["els"].get(&f.src).is_some() {
            let tag = canvas::tags(&new).into_iter().nth(tag.n - 1).ok_or_else(|| internal("改完文字后找不到这个元素。".into()))?;
            let mut doc = doc;
            doc["els"][&f.src]["fp"] = json!(canvas::fingerprint(&new, &tag));
            save_canvas(&conn, &user, &doc)?;
        }
        done(&conn)
    })
    .await
}
