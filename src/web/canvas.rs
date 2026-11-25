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
    /// A page address: the edit applies on that page only. Empty: on every page using the element.
    #[serde(default)]
    page: String,
    /// The tag name the admin saw there (checked when given).
    #[serde(default)]
    tag: String,
}

fn all() -> String {
    "all".into()
}

#[derive(Deserialize)]
pub struct TextForm {
    csrf: String,
    src: String,
    text: String,
    #[serde(default)]
    tag: String,
}

fn bad(msg: impl Into<String>) -> Out {
    Out::Fail(StatusCode::BAD_REQUEST, msg.into())
}

/// The draft source of the element named by `src` and its tag, or why it cannot be edited.
/// `expect` is the tag name the admin saw (empty: not checked): when the file changed meanwhile and another
/// element now sits there, nothing is edited.
fn element(conn: &Connection, src: &str, expect: &str) -> Result<(String, String, canvas::Tag), Out> {
    let (path, n) = canvas::parse_src(src).ok_or_else(|| bad("不认识这个元素：请刷新预览再选。"))?;
    let source = site::file(conn, Mode::Preview, path).map_err(internal)?.ok_or_else(|| bad("这个元素所在的文件已经不在了：请刷新预览。"))?;
    let tag = canvas::tags(&source).into_iter().nth(n - 1).ok_or_else(|| bad("这个元素已经不在文件里了：请刷新预览。"))?;
    if !expect.is_empty() && !expect.eq_ignore_ascii_case(&tag.name) {
        return Err(bad("这个元素已经变了（文件刚被改过）：请刷新预览再选。"));
    }
    Ok((path.to_string(), source, tag))
}

/// The one spelling of an element's key.
fn key(path: &str, tag: &canvas::Tag) -> String {
    format!("{path}#{}", tag.n)
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

fn done(conn: &Connection, extra: Value) -> Result<Out, Out> {
    let pending = changes::pending_order(conn).map_err(internal)?.len();
    let mut out = json!({ "pending": pending });
    if let (Some(o), Value::Object(e)) = (out.as_object_mut(), extra) {
        o.extend(e);
    }
    Ok(Out::Json(out))
}

fn allowed(user: &AdminUser, csrf: &str) -> Result<(), Out> {
    session::check_csrf(user, csrf).map_err(|s| Out::Fail(s, "表单已失效，请刷新页面后再试。".into()))?;
    may_edit(user)
}

fn may_edit(user: &AdminUser) -> Result<(), Out> {
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
        let (path, source, tag) = element(&conn, &f.src, &f.tag)?;
        let k = key(&path, &tag);
        let fp = canvas::fingerprint(&source, &tag);
        let doc = changes::setting(&conn, canvas::KEY, true);
        // What it was, so the admin can undo this edit.
        let page = Some(f.page.as_str()).filter(|p| !p.is_empty());
        let old = canvas::current(&doc, &k, page, &f.tier, &f.prop);
        let value = Some(f.value.as_str()).filter(|v| !v.trim().is_empty());
        let doc = canvas::set_style(&doc, &k, &fp, page, &f.tier, &f.prop, value).map_err(bad)?;
        save_canvas(&conn, &user, &doc)?;
        done(&conn, json!({ "old": old }))
    })
    .await
}

/// Replaces the text of one element whose text is written in its template.
pub async fn text(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Form(f): Form<TextForm>) -> Response {
    blocking(move || {
        allowed(&user, &f.csrf)?;
        let conn = app.conn();
        let (path, source, tag) = element(&conn, &f.src, &f.tag)?;
        let (new, from, by) = canvas::set_text(&source, tag.n, &f.text).map_err(bad)?;
        site::check_file(&path, &new).map_err(bad)?;
        changes::stage_file(&conn, &user.actor(), &path, &new, NOTE).map_err(internal)?;
        // Line breaks are tags: the edits after it move. Its fingerprint holds its text: its own edits follow.
        let old = changes::setting(&conn, canvas::KEY, true);
        let mut doc = canvas::shift(&old, &path, from, by);
        let k = key(&path, &tag);
        if doc["els"].get(&k).is_some() {
            let tag = canvas::tags(&new).into_iter().nth(tag.n - 1).ok_or_else(|| internal("改完文字后找不到这个元素。".into()))?;
            doc["els"][&k]["fp"] = json!(canvas::fingerprint(&new, &tag));
        }
        if doc != old {
            save_canvas(&conn, &user, &doc)?;
        }
        // The preview's positions moved too: it reloads.
        done(&conn, json!({ "reload": by != 0 }))
    })
    .await
}

#[derive(Deserialize)]
pub struct ElementForm {
    csrf: String,
    src: String,
    /// `remove` or `duplicate`.
    action: String,
    #[serde(default)]
    tag: String,
}

/// Removes an element from its template, or puts a copy of it right after itself. Hand edits of the elements
/// after it move with them.
pub async fn element_action(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Form(f): Form<ElementForm>) -> Response {
    blocking(move || {
        allowed(&user, &f.csrf)?;
        let copy = match f.action.as_str() {
            "duplicate" => true,
            "remove" => false,
            _ => return Err(bad("只能复制（duplicate）或删除（remove）。")),
        };
        let conn = app.conn();
        let (path, source, tag) = element(&conn, &f.src, &f.tag)?;
        let (new, from, by) = canvas::restructure(&source, tag.n, copy).map_err(bad)?;
        site::check_file(&path, &new).map_err(bad)?;
        changes::stage_file(&conn, &user.actor(), &path, &new, NOTE).map_err(internal)?;
        let doc = changes::setting(&conn, canvas::KEY, true);
        let moved = canvas::shift(&doc, &path, from, by);
        if moved != doc {
            save_canvas(&conn, &user, &moved)?;
        }
        done(&conn, json!({}))
    })
    .await
}

#[derive(Deserialize)]
pub struct AddForm {
    csrf: String,
    /// The element clicked: the new one goes into it, or into the nearest block around it.
    src: String,
    /// `text`, `rect`, `circle`, `line` or `button`.
    kind: String,
}

/// Adds an element with the pen. Returns its `src`, so the preview can put it where the click was.
pub async fn add(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Form(f): Form<AddForm>) -> Response {
    blocking(move || {
        allowed(&user, &f.csrf)?;
        let en = crate::i18n::Lang::of_prefs(&user.prefs) == crate::i18n::Lang::En;
        let conn = app.conn();
        let (path, source, tag) = element(&conn, &f.src, "")?;
        let (new, n) = canvas::add(&source, tag.n, &f.kind, en).map_err(bad)?;
        site::check_file(&path, &new).map_err(bad)?;
        changes::stage_file(&conn, &user.actor(), &path, &new, NOTE).map_err(internal)?;
        let doc = changes::setting(&conn, canvas::KEY, true);
        let moved = canvas::shift(&doc, &path, n, 1);
        if moved != doc {
            save_canvas(&conn, &user, &moved)?;
        }
        done(&conn, json!({ "src": format!("{path}#{n}") }))
    })
    .await
}

/// Uploads an image and adds it with the pen (`mode=add`, into the element `src` like `add`), or points the
/// `<img>` at `src` to it (`mode=replace`). Multipart: `csrf`, `src`, `mode`, `file`.
pub async fn image(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: axum::extract::Request) -> Response {
    let posted = match super::content::checked(&user, req).await {
        Ok(p) => p,
        Err(o) => return axum::response::IntoResponse::into_response(o),
    };
    blocking(move || {
        // `checked` has seen the CSRF token.
        may_edit(&user)?;
        let Some((_, name, bytes)) = posted.files.iter().find(|(input, _, _)| input == "file") else {
            return Err(bad("没有收到图片：请先选一张。"));
        };
        let src = posted.one("src").to_string();
        let replace = match posted.one("mode") {
            "replace" => true,
            "add" => false,
            _ => return Err(bad("只能添加（add）或替换（replace）图片。")),
        };
        // Nothing is stored for an element that is not there, or that is not an image to replace.
        {
            let (_, _, tag) = element(&app.conn(), &src, posted.one("tag"))?;
            if replace && tag.name != "img" {
                return Err(bad("只有图片能换图片。"));
            }
        }
        // Uploads are checked by their content: images and PDFs. Only an image fits here.
        let url = crate::media::store_upload(&app, name, bytes).map_err(bad)?;
        if url.ends_with(".pdf") {
            return Err(bad("这里只能用图片，不能用 PDF。"));
        }
        // Read and write the template under one lock, so nothing written meanwhile is lost.
        let conn = app.conn();
        let (path, source, tag) = element(&conn, &src, posted.one("tag"))?;
        let (new, n) = if replace {
            (canvas::replace_image(&source, tag.n, &url).map_err(bad)?, tag.n)
        } else {
            canvas::add_markup(&source, tag.n, &canvas::image_element(&url).map_err(bad)?).map_err(bad)?
        };
        site::check_file(&path, &new).map_err(bad)?;
        changes::stage_file(&conn, &user.actor(), &path, &new, NOTE).map_err(internal)?;
        // A replaced image keeps its place (its fingerprint ignores `src`); an added one moves those after it.
        if !replace {
            let doc = changes::setting(&conn, canvas::KEY, true);
            let moved = canvas::shift(&doc, &path, n, 1);
            if moved != doc {
                save_canvas(&conn, &user, &moved)?;
            }
        }
        done(&conn, json!({ "src": format!("{path}#{n}") }))
    })
    .await
}

#[derive(Deserialize)]
pub struct InfoQuery {
    src: String,
    /// The address of the page being looked at.
    #[serde(default)]
    page: String,
}

/// What the bar shows about the selected element: how many pages share it (its file is a layout or a
/// fragment), and the widths at which it has edits of its own.
pub async fn info(State(app): State<Shared>, axum::extract::Query(q): axum::extract::Query<InfoQuery>) -> Response {
    blocking(move || {
        let conn = app.conn();
        let (path, _, tag) = element(&conn, &q.src, "")?;
        let k = key(&path, &tag);
        let mut stmt = conn
            .prepare("SELECT path, coalesce(draft, published) FROM site_files WHERE draft_deleted = 0 AND path LIKE '%.html'")
            .map_err(|e| internal(e.to_string()))?;
        let files: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))
            .map_err(|e| internal(e.to_string()))?
            .filter_map(|r| r.ok())
            .map(|(p, j)| (p, j.and_then(|j| serde_json::from_str::<String>(&j).ok()).unwrap_or_default()))
            .collect();
        let doc = changes::setting(&conn, canvas::KEY, true);
        Ok(Out::Json(json!({ "pages": canvas::pages_using(&files, &path), "tiers": canvas::tiers_of(&doc, &k, &q.page) })))
    })
    .await
}

#[derive(Deserialize)]
pub struct ResetForm {
    csrf: String,
    src: String,
    tier: String,
}

/// Drops the element's edits for one width, so it looks as the wider widths make it ("reset to desktop").
pub async fn reset(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Form(f): Form<ResetForm>) -> Response {
    blocking(move || {
        allowed(&user, &f.csrf)?;
        if !["tablet", "mobile"].contains(&f.tier.as_str()) {
            return Err(bad("只能恢复平板（tablet）或手机（mobile）宽度。"));
        }
        let conn = app.conn();
        let (path, _, tag) = element(&conn, &f.src, "")?;
        let doc = changes::setting(&conn, canvas::KEY, true);
        save_canvas(&conn, &user, &canvas::reset_tier(&doc, &key(&path, &tag), &f.tier))?;
        done(&conn, json!({}))
    })
    .await
}
