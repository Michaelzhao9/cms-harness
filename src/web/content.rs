//! Content screens: collection and view lists, the record form, the media library.
//! Lists come from `query_records`; every write is a tool call with the logged-in admin's actor.

use super::admin::nav;
use super::page;
use super::session::{self, AdminUser};
use crate::auth::Scope;
use crate::schema::{project, strip_tags, Action, CollectionDef, FieldDef, FieldType, ViewDef};
use crate::{changes, media, tools, App, Shared};
use axum::extract::{FromRequest, Multipart, Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Form};
use rusqlite::OptionalExtension;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

const PER_PAGE: i64 = 20;
const EDIT_NOTE: &str = "后台手动修改";
const CREATE_NOTE: &str = "后台手动新建";
const DELETE_NOTE: &str = "后台手动删除";
pub(super) const CSRF_FAIL: &str = "表单已失效，请刷新页面后再试。";
const STALE: &str = "这条记录在你打开之后被改过：请刷新页面看到最新内容后再改。你填的内容还在下面。";

type Params = Vec<(String, String)>;

/// How a handler ends.
pub(super) enum Out {
    Page(StatusCode, &'static str, Value),
    Go(String),
    Fail(StatusCode, String),
    Json(Value),
}

impl IntoResponse for Out {
    fn into_response(self) -> Response {
        match self {
            Out::Page(status, name, ctx) => page(status, name, ctx),
            Out::Go(to) => Redirect::to(&to).into_response(),
            Out::Fail(status, msg) => (status, msg).into_response(),
            Out::Json(v) => axum::Json(v).into_response(),
        }
    }
}

pub(super) fn internal(e: String) -> Out {
    Out::Fail(StatusCode::INTERNAL_SERVER_ERROR, e)
}

pub(super) async fn blocking(f: impl FnOnce() -> Result<Out, Out> + Send + 'static) -> Response {
    let out = tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| Err(internal(format!("服务器内部错误：{e}"))));
    out.unwrap_or_else(|o| o).into_response()
}

pub(super) fn tool(app: &App, user: &AdminUser, name: &str, args: Value) -> Result<Value, String> {
    tools::call(app, &user.actor(), name, args)
}

/// The context `base.html` needs around a page's own.
pub(super) fn shell(app: &App, user: &AdminUser, current: &str, mut ctx: Value) -> Value {
    let (nav, name) = {
        let conn = app.conn();
        (nav(&conn, user, current), app.name(&conn, false))
    };
    ctx["chat"] = super::chat::view(app, user);
    ctx["site_name"] = json!(name);
    ctx["csrf"] = json!(user.csrf);
    ctx["nav"] = nav;
    let lang = crate::i18n::Lang::of_prefs(&user.prefs);
    ctx["lang"] = json!(lang.code());
    ctx["html_lang"] = json!(lang.html());
    ctx
}

fn param<'a>(params: &'a Params, key: &str) -> &'a str {
    params.iter().rev().find(|(k, _)| k == key).map_or("", |(_, v)| v.as_str())
}

/// `?a=1&b=2` from the non-empty pairs, percent-encoded; empty when there are none.
fn query_string(pairs: &[(&str, &str)]) -> String {
    let enc = |s: &str| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
    let parts: Vec<String> = pairs.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect();
    if parts.is_empty() { String::new() } else { format!("?{}", parts.join("&")) }
}

pub(super) fn collection(app: &App, name: &str) -> Result<CollectionDef, Out> {
    match changes::collection_view(&app.conn(), name) {
        Ok(Some((def, _))) => Ok(def),
        Ok(None) => Err(Out::Fail(StatusCode::NOT_FOUND, format!("集合“{name}”不存在或已删除。"))),
        Err(e) => Err(internal(e)),
    }
}

fn status_label(status: &str) -> &'static str {
    match status {
        "draft" => "草稿",
        "changed" => "有改动",
        "deleting" => "待删除",
        _ => "已发布",
    }
}

// ---- values: stored JSON, form strings, short text ----

/// A number as typed; text that is not one goes on as text, for the tool to refuse.
fn number(s: &str) -> Value {
    let s = s.trim();
    s.parse::<i64>()
        .map(Value::from)
        .ok()
        .or_else(|| s.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number))
        .unwrap_or_else(|| json!(s))
}

/// A stored value as the strings its form inputs hold.
fn to_raw(f: &FieldDef, v: &Value) -> Vec<String> {
    let one = |v: &Value| match v {
        // `datetime-local` has no zone; stored times are UTC and get their `Z` back on save.
        Value::String(s) if f.kind == FieldType::Date => s.trim_end_matches('Z').to_string(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    match v {
        Value::Null => vec![],
        Value::Array(a) => a.iter().map(one).collect(),
        v => vec![one(v)],
    }
}

/// What a field's form strings mean as a value. Empty strings are the "rendered but nothing chosen" markers.
fn from_raw(f: &FieldDef, raw: &[String]) -> Value {
    if f.kind == FieldType::Boolean {
        return json!(raw.last().is_some_and(|s| s == "true"));
    }
    let one = |s: &str| match f.kind {
        // Browsers send textarea line breaks as CRLF.
        FieldType::Text | FieldType::Richtext => json!(s.replace("\r\n", "\n")),
        FieldType::Number => number(s),
        FieldType::Relation => s.parse::<i64>().map_or_else(|_| json!(s), Value::from),
        FieldType::Date => match (s.len(), s.contains('T')) {
            (16, true) => json!(format!("{s}:00Z")),
            (19, true) => json!(format!("{s}Z")),
            _ => json!(s),
        },
        _ => json!(s),
    };
    if f.multiple {
        let items: Vec<Value> = raw.iter().filter(|s| !s.is_empty()).map(|s| one(s)).collect();
        if items.is_empty() { Value::Null } else { Value::Array(items) }
    } else {
        raw.last().filter(|s| !s.is_empty()).map_or(Value::Null, |s| one(s))
    }
}

/// A stored value as one short line for tables and change marks; empty for no value.
pub(super) fn show(f: &FieldDef, v: &Value) -> String {
    let one = |v: &Value| match (f.kind, v) {
        (_, Value::Null) => String::new(),
        (FieldType::Richtext, Value::String(s)) => strip_tags(s),
        (FieldType::Date, Value::String(s)) => s.trim_end_matches('Z').replace('T', " "),
        (FieldType::Media, Value::String(s)) => s.rsplit('/').next().unwrap_or(s).to_string(),
        (FieldType::Relation, v) => format!("#{v}"),
        (FieldType::Boolean, Value::Bool(b)) => (if *b { "是" } else { "否" }).to_string(),
        (_, Value::String(s)) => s.clone(),
        (_, v) => v.to_string(),
    };
    let text = match v {
        Value::Array(a) => a.iter().map(one).collect::<Vec<_>>().join("、"),
        v => one(v),
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > 60 { format!("{}…", text.chars().take(60).collect::<String>()) } else { text }
}

// ---- the list ----

/// The list part of a collection or view page. `current` is the record open beside it.
fn list_ctx(app: &App, user: &AdminUser, def: &CollectionDef, view: Option<&ViewDef>, params: &Params, current: Option<i64>) -> Result<Value, Out> {
    let base = match view {
        Some(v) => format!("/admin/v/{}", v.name),
        None => format!("/admin/c/{}", def.name),
    };
    let tab = match param(params, "status") {
        "changed" => "changed",
        "draft" => "draft",
        _ => "",
    };
    let statuses = match tab {
        "changed" => json!(["changed", "deleting"]),
        "draft" => json!(["draft"]),
        _ => Value::Null,
    };
    let page = param(params, "page").parse::<i64>().unwrap_or(1).max(1);
    let allows = |a: Action| view.is_none_or(|v| v.actions.contains(&a));
    let columns: Vec<&str> = match view {
        Some(v) => v.columns.iter().map(String::as_str).collect(),
        None => def.fields.iter().take(5).map(|f| f.name.as_str()).collect(),
    };

    // The view's filters: one control each, equality on the typed value.
    let mut filter = Map::new();
    let mut filters = vec![];
    let mut kept: Vec<(&str, &str)> = vec![("status", tab)];
    for name in view.map_or(&[][..], |v| &v.filters[..]) {
        let f = def.field(name);
        let value = param(params, name);
        if !value.is_empty() {
            let typed = match f.map(|f| f.kind) {
                None | Some(FieldType::Relation) | Some(FieldType::Number) => number(value),
                Some(FieldType::Boolean) => json!(value == "true"),
                _ => json!(value),
            };
            filter.insert(name.clone(), typed);
        }
        kept.push((name.as_str(), value));
        let options: Vec<Value> = match f.map(|f| (f.kind, &f.options)) {
            Some((FieldType::Select, opts)) => std::iter::once(json!({ "value": "", "label": "全部" }))
                .chain(opts.iter().map(|o| json!({ "value": o, "label": o })))
                .collect(),
            Some((FieldType::Boolean, _)) => vec![json!({ "value": "", "label": "全部" }), json!({ "value": "true", "label": "是" }), json!({ "value": "false", "label": "否" })],
            _ => vec![],
        };
        filters.push(json!({ "name": name, "title": f.map_or("编号", |f| f.title.as_str()), "value": value, "options": options }));
    }

    let mut args = json!({ "collection": def.name, "limit": PER_PAGE, "offset": (page - 1) * PER_PAGE, "status": statuses });
    if !filter.is_empty() {
        args["filter"] = Value::Object(filter);
    }
    if let Some(sort) = view.and_then(|v| v.sort.as_ref()) {
        args["sort"] = json!(sort);
    }
    let (items, total, list_error) = match tool(app, user, "query_records", args) {
        Ok(r) => (r["items"].as_array().cloned().unwrap_or_default(), r["total"].as_i64().unwrap_or(0), None),
        Err(e) => (vec![], 0, Some(e)),
    };
    let count = |status: Value| {
        tool(app, user, "query_records", json!({ "collection": def.name, "limit": 1, "status": status }))
            .map(|r| r["total"].as_i64().unwrap_or(0))
            .map_err(internal)
    };
    let (all, changed, drafts) = (count(Value::Null)?, count(json!(["changed", "deleting"]))?, count(json!(["draft"]))?);
    let mut parts = vec![];
    if changed > 0 {
        parts.push(format!("{changed} 条有改动"));
    }
    if drafts > 0 {
        parts.push(format!("{drafts} 条草稿"));
    }
    let summary = if parts.is_empty() { format!("{all} 条") } else { format!("{all} 条，其中 {}", parts.join("、")) };

    let rows: Vec<Value> = items
        .iter()
        .map(|r| {
            let id = r["id"].as_i64().unwrap_or_default();
            let status = r["status"].as_str().unwrap_or_default();
            let cells: Vec<Value> = columns
                .iter()
                .map(|c| match def.field(c) {
                    None => json!({ "text": format!("#{id}") }),
                    Some(f) => {
                        let v = &r["data"][*c];
                        let first = v.as_array().and_then(|a| a.first()).unwrap_or(v);
                        let thumb = (f.kind == FieldType::Media).then(|| first.as_str()).flatten().and_then(|p| p.strip_prefix("/uploads/")).filter(|n| !n.ends_with(".pdf"));
                        json!({ "text": show(f, v), "thumb": thumb })
                    }
                })
                .collect();
            // Cards and lists: a title, a subtitle and a picture, from the view's fields (else the first columns).
            let text_of = |name: Option<&str>| -> String {
                match name.and_then(|n| def.field(n)) {
                    Some(f) => show(f, &r["data"][&f.name]),
                    None if name == Some("id") => format!("#{id}"),
                    None => String::new(),
                }
            };
            let title = view.and_then(|v| v.title_field.as_deref()).map(|f| text_of(Some(f))).or_else(|| cells.first().and_then(|c| c["text"].as_str()).map(String::from)).unwrap_or_default();
            let subtitle = view.and_then(|v| v.subtitle_field.as_deref()).map(|f| text_of(Some(f))).or_else(|| cells.get(1).and_then(|c| c["text"].as_str()).map(String::from)).unwrap_or_default();
            let image = view
                .and_then(|v| v.image_field.as_deref())
                .and_then(|f| {
                    let v = &r["data"][f];
                    v.as_array().and_then(|a| a.first()).unwrap_or(v).as_str().and_then(|p| p.strip_prefix("/uploads/")).filter(|n| !n.ends_with(".pdf")).map(String::from)
                });
            let group = view.and_then(|v| v.group_by.as_deref()).map(|g| r["data"][g].as_str().unwrap_or_default().to_string());
            json!({ "id": id, "status": status, "label": status_label(status), "cells": cells, "current": current == Some(id), "link": allows(Action::Edit),
                "title": if title.is_empty() { format!("#{id}") } else { title }, "subtitle": subtitle, "image": image, "group": group })
        })
        .collect();
    // One section per option of `group_by`, in the options' order, then anything without a value; else one section.
    let sections: Vec<Value> = match view.and_then(|v| v.group_by.as_deref()).and_then(|g| def.field(g)) {
        Some(f) => {
            let mut out: Vec<Value> = f
                .options
                .iter()
                .map(|o| json!({ "title": o, "rows": rows.iter().filter(|r| r["group"] == *o).cloned().collect::<Vec<_>>() }))
                .filter(|s| s["rows"].as_array().is_some_and(|r| !r.is_empty()))
                .collect();
            let rest: Vec<Value> = rows.iter().filter(|r| !f.options.iter().any(|o| r["group"] == *o)).cloned().collect();
            if !rest.is_empty() {
                out.push(json!({ "title": "未分组", "rows": rest }));
            }
            out
        }
        None => vec![json!({ "title": "", "rows": rows })],
    };

    let tabs: Vec<Value> = [("", "全部"), ("changed", "有改动"), ("draft", "草稿")]
        .iter()
        .map(|(key, label)| {
            let mut q = kept.clone();
            q[0].1 = key;
            json!({ "label": label, "on": *key == tab, "href": format!("{base}{}", query_string(&q)) })
        })
        .collect();
    let pages = ((total + PER_PAGE - 1) / PER_PAGE).max(1);
    let at = |n: i64| {
        let n = n.to_string();
        let mut q = kept.clone();
        q.push(("page", if n == "1" { "" } else { &n }));
        format!("{base}{}", query_string(&q))
    };
    let pager = (pages > 1).then(|| json!({ "page": page, "pages": pages, "prev": (page > 1).then(|| at(page - 1)), "next": (page < pages).then(|| at(page + 1)) }));
    let titles: Vec<&str> = columns.iter().map(|c| def.field(c).map_or("编号", |f| f.title.as_str())).collect();

    Ok(json!({
        "heading": view.map_or(def.title.as_str(), |v| v.title.as_str()),
        "summary": summary,
        "collection": def.name,
        "base": base,
        "tab": tab,
        "tabs": tabs,
        "filters": filters,
        "columns": titles,
        "sections": sections,
        "layout": view.map_or(crate::schema::Layout::Table, |v| v.layout),
        "density": view.map_or(crate::schema::Density::Comfortable, |v| v.density),
        "list_error": list_error,
        "pager": pager,
        "can_create": allows(Action::Create) && user.actor().can(Scope::Write),
        "new_href": format!("{base}?new=1"),
        "row_delete": view.is_some() && allows(Action::Delete),
    }))
}

// ---- the form ----

/// What was typed into a form that the tool refused, to show again.
struct Typed {
    raw: HashMap<String, Vec<String>>,
    note: String,
    error: String,
    status: StatusCode,
    /// The `seen` the form was posted with: a refused save keeps it, so a stale form stays stale until reloaded.
    seen: Option<String>,
}

/// `(value, label)` for every record of a relation's target: id plus its first text field.
fn relation_choices(app: &App, user: &AdminUser, f: &FieldDef) -> Result<Vec<(String, String)>, String> {
    let target = f.target.as_deref().unwrap_or_default();
    let Some((tdef, _)) = changes::collection_view(&app.conn(), target)? else { return Ok(vec![]) };
    let label = tdef.fields.iter().find(|f| f.kind == FieldType::Text).map(|f| f.name.as_str());
    // ponytail: the newest 100 records only (query_records' page cap); a search field is the upgrade when targets grow.
    let found = tool(app, user, "query_records", json!({ "collection": target, "limit": 100 }))?;
    Ok(found["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let id = r["id"].to_string();
            let name = label.and_then(|n| r["data"][n].as_str()).unwrap_or_default();
            (id.clone(), format!("#{id} {name}").trim_end().to_string())
        })
        .collect())
}

/// One entry per field for `record.html`: which widget, what it holds, its choices and its unpublished change.
fn form_fields(app: &App, user: &AdminUser, def: &CollectionDef, raw: &HashMap<String, Vec<String>>, marks: &Map<String, Value>) -> Result<Vec<Value>, String> {
    let uploads: Vec<(String, String)> = if def.fields.iter().any(|f| f.kind == FieldType::Media) {
        media::list(app).into_iter().map(|(n, _)| (format!("/uploads/{n}"), n)).collect()
    } else {
        vec![]
    };
    let mut out = vec![];
    for f in &def.fields {
        let vals = raw.get(&f.name).cloned().unwrap_or_default();
        let value = vals.last().cloned().unwrap_or_default();
        let (kind, mut options) = match f.kind {
            FieldType::Text if value.chars().count() > 80 || value.contains('\n') => ("textarea", vec![]),
            FieldType::Text => ("text", vec![]),
            FieldType::Richtext => ("richtext", vec![]),
            FieldType::Number => ("number", vec![]),
            FieldType::Boolean => ("boolean", vec![]),
            FieldType::Date if value.contains('T') => ("datetime", vec![]),
            FieldType::Date => ("date", vec![]),
            FieldType::Select if f.multiple => ("checks", f.options.iter().map(|o| (o.clone(), o.clone())).collect()),
            FieldType::Select => ("select", f.options.iter().map(|o| (o.clone(), o.clone())).collect()),
            FieldType::Media => ("media", uploads.clone()),
            FieldType::Relation => ("relation", relation_choices(app, user, f)?),
        };
        // A held value missing from the choices (an old option, a record past the first hundred, an outside URL)
        // stays selectable, so saving the form does not drop it.
        if !options.is_empty() || matches!(kind, "media" | "relation") {
            for v in vals.iter().filter(|v| !v.is_empty()) {
                if !options.iter().any(|(o, _)| o == v) {
                    options.push((v.clone(), v.clone()));
                }
            }
        }
        let thumbs: Vec<&str> = if kind == "media" {
            vals.iter().filter_map(|v| v.strip_prefix("/uploads/")).filter(|n| !n.ends_with(".pdf")).collect()
        } else {
            vec![]
        };
        out.push(json!({
            "name": f.name, "title": f.title, "kind": kind, "required": f.required, "multiple": f.multiple,
            "value": value, "checked": value == "true", "selected": vals, "thumbs": thumbs, "mark": marks.get(&f.name),
            "options": options.into_iter().map(|(v, l)| json!({ "value": v, "label": l })).collect::<Vec<_>>(),
        }));
    }
    Ok(out)
}

/// The stored document's raw form strings, with what was typed laid over them.
fn raw_of(def: &CollectionDef, doc: &Value, typed: Option<&Typed>) -> HashMap<String, Vec<String>> {
    let mut raw: HashMap<String, Vec<String>> = def.fields.iter().map(|f| (f.name.clone(), to_raw(f, &doc[&f.name]))).collect();
    if let Some(t) = typed {
        for f in &def.fields {
            if let Some(v) = t.raw.get(&f.name) {
                raw.insert(f.name.clone(), v.clone());
            }
        }
    }
    raw
}

fn new_form(app: &App, user: &AdminUser, def: &CollectionDef, typed: Option<&Typed>) -> Result<Value, Out> {
    let raw = raw_of(def, &project(def, &json!({})), typed);
    let fields = form_fields(app, user, def, &raw, &Map::new()).map_err(internal)?;
    Ok(json!({ "id": null, "title": "新建记录", "fields": fields, "note": typed.map_or(CREATE_NOTE, |t| t.note.as_str()), "error": typed.map(|t| &t.error) }))
}

fn collection_page(app: &App, user: &AdminUser, name: &str, params: &Params, typed: Option<Typed>) -> Result<Out, Out> {
    let def = collection(app, name)?;
    let mut ctx = list_ctx(app, user, &def, None, params, None)?;
    let status = typed.as_ref().map_or(StatusCode::OK, |t| t.status);
    if typed.is_some() || (param(params, "new") == "1" && ctx["can_create"] == true) {
        ctx["form"] = new_form(app, user, &def, typed.as_ref())?;
    }
    Ok(Out::Page(status, "collection.html", shell(app, user, &format!("c:{name}"), ctx)))
}

fn view_page(app: &App, user: &AdminUser, name: &str, params: &Params) -> Result<Out, Out> {
    let views = tool(app, user, "list_views", json!({})).map_err(internal)?;
    let view: ViewDef = views
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["definition"]["name"] == name)
        .and_then(|v| serde_json::from_value(v["definition"].clone()).ok())
        .ok_or_else(|| Out::Fail(StatusCode::NOT_FOUND, format!("界面“{name}”不存在或已删除。")))?;
    if view.collection == crate::members::MEMBERS {
        return members_page(app, user, params, None);
    }
    let def = collection(app, &view.collection)?;
    let mut ctx = list_ctx(app, user, &def, Some(&view), params, None)?;
    if param(params, "new") == "1" && ctx["can_create"] == true {
        ctx["form"] = new_form(app, user, &def, None)?;
    }
    Ok(Out::Page(StatusCode::OK, "collection.html", shell(app, user, &format!("v:{name}"), ctx)))
}

/// A record's row as stored. `seen` is `updated_at`, which every write refreshes.
pub(super) struct Row {
    pub(super) collection: String,
    pub(super) published: Option<Value>,
    pub(super) draft: Option<Value>,
    deleting: bool,
    seen: String,
}

pub(super) fn load(app: &App, id: i64) -> Result<Row, Out> {
    let found = app
        .conn()
        .query_row(
            "SELECT collection, published, draft, draft_deleted, updated_at FROM records WHERE id = ?1 AND (published IS NOT NULL OR draft IS NOT NULL)",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, bool>(3)?, r.get::<_, String>(4)?)),
        )
        .optional();
    let parse = |s: Option<String>| s.and_then(|s| serde_json::from_str(&s).ok());
    match found {
        Ok(Some((collection, published, draft, deleting, seen))) => Ok(Row { collection, published: parse(published), draft: parse(draft), deleting, seen }),
        Ok(None) => Err(Out::Fail(StatusCode::NOT_FOUND, format!("记录 {id} 不存在或已被删除。"))),
        Err(e) => Err(internal(format!("读取记录 {id} 失败：{e}"))),
    }
}

/// (draft-else-live document, live document), both in the current definition's shape.
pub(super) fn docs(app: &App, def: &CollectionDef, row: &Row) -> Result<(Value, Option<Value>), Out> {
    let live = match &row.published {
        Some(p) => Some(project(def, &changes::to_current(&app.conn(), &row.collection, p).map_err(internal)?)),
        None => None,
    };
    let current = match (&row.draft, &live) {
        (Some(d), _) => project(def, d),
        (None, Some(l)) => l.clone(),
        (None, None) => json!({}),
    };
    Ok((current, live))
}

/// A record's name: its first non-empty text field, else its number.
pub(super) fn record_title(def: &CollectionDef, doc: &Value, id: i64) -> String {
    def.fields
        .iter()
        .filter(|f| f.kind == FieldType::Text)
        .find_map(|f| doc[&f.name].as_str().filter(|s| !s.is_empty()))
        .map_or_else(|| format!("记录 #{id}"), str::to_string)
}

fn record_page(app: &App, user: &AdminUser, id: i64, params: &Params, typed: Option<Typed>) -> Result<Out, Out> {
    let row = load(app, id)?;
    let def = collection(app, &row.collection)?;
    let (current, live) = docs(app, &def, &row)?;
    let status = changes::status(row.published.is_some(), row.draft.is_some(), row.deleting);
    // Fields whose draft differs from the live copy: old struck through, new in bold.
    let mut marks = Map::new();
    if let (Some(live), Some(_)) = (&live, &row.draft) {
        for f in def.fields.iter().filter(|f| live[&f.name] != current[&f.name]) {
            let or_unset = |s: String| if s.is_empty() { "未填".to_string() } else { s };
            marks.insert(f.name.clone(), json!({ "old": or_unset(show(f, &live[&f.name])), "new": or_unset(show(f, &current[&f.name])) }));
        }
    }
    let raw = raw_of(&def, &current, typed.as_ref());
    let fields = form_fields(app, user, &def, &raw, &marks).map_err(internal)?;
    let title = record_title(&def, &current, id);
    let (slug, url) = {
        let conn = app.conn();
        let slug: Option<String> = conn.query_row("SELECT coalesce(draft_slug, slug) FROM records WHERE id = ?1", [id], |r| r.get(0)).ok().flatten();
        (slug, crate::routes::url_for(&conn, crate::site::Mode::Preview, &def, id, &current))
    };
    let typed_slug = typed.as_ref().and_then(|t| t.raw.get("_slug")).and_then(|v| v.last()).cloned();
    let mut ctx = list_ctx(app, user, &def, None, &vec![], Some(id))?;
    ctx["form"] = json!({
        "id": id, "title": title, "status": status, "status_label": status_label(status), "fields": fields,
        "slug": typed_slug.or(slug), "url": url,
        "seen": typed.as_ref().and_then(|t| t.seen.as_deref()).unwrap_or(&row.seen),
        "note": typed.as_ref().map_or(EDIT_NOTE, |t| t.note.as_str()), "error": typed.as_ref().map(|t| &t.error),
        "saved": typed.is_none() && param(params, "saved") == "1",
    });
    let code = typed.as_ref().map_or(StatusCode::OK, |t| t.status);
    Ok(Out::Page(code, "collection.html", shell(app, user, &format!("c:{}", row.collection), ctx)))
}

// ---- posted forms ----

/// A posted form, urlencoded or multipart, read whole before anything is checked or stored.
#[derive(Default)]
pub(super) struct Posted {
    pub(super) fields: HashMap<String, Vec<String>>,
    /// (input name, file name, bytes); empty file inputs are left out.
    pub(super) files: Vec<(String, String, Vec<u8>)>,
}

impl Posted {
    pub(super) fn one(&self, key: &str) -> &str {
        self.fields.get(key).and_then(|v| v.last()).map_or("", String::as_str)
    }

    pub(super) fn note(&self, default: &str) -> String {
        let n = self.one("note").trim();
        if n.is_empty() { default.to_string() } else { n.to_string() }
    }
}

async fn read_posted(req: Request) -> Result<Posted, Out> {
    let multipart = req.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("multipart/form-data"));
    let mut posted = Posted::default();
    if !multipart {
        let Form(pairs) = Form::<Vec<(String, String)>>::from_request(req, &()).await.map_err(|e| Out::Fail(e.status(), e.body_text()))?;
        for (k, v) in pairs {
            posted.fields.entry(k).or_default().push(v);
        }
        return Ok(posted);
    }
    let mut form = Multipart::from_request(req, &()).await.map_err(|e| Out::Fail(e.status(), e.body_text()))?;
    let bad = |e: axum::extract::multipart::MultipartError| Out::Fail(e.status(), format!("读取表单失败：{}。", e.body_text()));
    while let Some(field) = form.next_field().await.map_err(bad)? {
        let name = field.name().unwrap_or_default().to_string();
        match field.file_name().map(str::to_string) {
            Some(file) => {
                let bytes = field.bytes().await.map_err(bad)?;
                if !bytes.is_empty() {
                    posted.files.push((name, file, bytes.to_vec()));
                }
            }
            None => {
                let text = field.text().await.map_err(bad)?;
                posted.fields.entry(name).or_default().push(text);
            }
        }
    }
    Ok(posted)
}

/// Reads the form and checks its CSRF token; nothing else happens before this passes.
pub(super) async fn checked(user: &AdminUser, req: Request) -> Result<Posted, Out> {
    let posted = read_posted(req).await?;
    session::check_csrf(user, posted.one("csrf")).map_err(|s| Out::Fail(s, CSRF_FAIL.into()))?;
    Ok(posted)
}

/// Stores the files sent for media fields and puts their paths into `data`: a single field takes the new
/// path, a multiple one gets it appended. The first refused file is the error.
fn store_files(app: &App, user: &AdminUser, def: &CollectionDef, posted: &Posted, current: &Value, data: &mut Map<String, Value>) -> Result<(), String> {
    for (input, file, bytes) in &posted.files {
        let Some(f) = input.strip_prefix("upload:").and_then(|n| def.field(n)).filter(|f| f.kind == FieldType::Media) else { continue };
        if !user.actor().can(Scope::Write) {
            return Err(format!("“{}”没有权限上传：需要 write 权限。", user.email));
        }
        let path = json!(media::store_upload(app, file, bytes)?);
        let value = if f.multiple {
            let mut list = match data.get(&f.name).unwrap_or(&current[&f.name]) {
                Value::Array(a) => a.clone(),
                _ => vec![],
            };
            list.push(path);
            Value::Array(list)
        } else {
            path
        };
        data.insert(f.name.clone(), value);
    }
    Ok(())
}

fn save_blocking(app: &App, user: &AdminUser, id: i64, posted: Posted) -> Result<Out, Out> {
    let row = load(app, id)?;
    let def = collection(app, &row.collection)?;
    let note = posted.note(EDIT_NOTE);
    let retry = |status: StatusCode, error: String| {
        record_page(app, user, id, &vec![], Some(Typed { raw: posted.fields.clone(), note: note.clone(), error, status, seen: Some(posted.one("seen").to_string()) }))
    };
    if posted.one("seen") != row.seen {
        return retry(StatusCode::CONFLICT, STALE.into());
    }
    let (current, _) = docs(app, &def, &row)?;
    // Only what the form changed; a field the form did not carry is left alone.
    let mut data = Map::new();
    for f in &def.fields {
        let Some(raw) = posted.fields.get(&f.name) else { continue };
        let new = from_raw(f, raw);
        if new != from_raw(f, &to_raw(f, &current[&f.name])) {
            data.insert(f.name.clone(), new);
        }
    }
    if let Err(e) = store_files(app, user, &def, &posted, &current, &mut data) {
        return retry(StatusCode::BAD_REQUEST, e);
    }
    // The address segment, when the form carries one and it changed.
    let slug_now: Option<String> = app.conn().query_row("SELECT coalesce(draft_slug, slug) FROM records WHERE id = ?1", [id], |r| r.get(0)).ok().flatten();
    let slug = Some(posted.one("_slug").trim().to_string()).filter(|s| !s.is_empty() && Some(s) != slug_now.as_ref());
    if data.is_empty() && slug.is_none() {
        return Ok(Out::Go(format!("/admin/r/{id}")));
    }
    let mut args = json!({ "id": id, "data": data, "note": note });
    if let Some(s) = slug {
        args["slug"] = json!(s);
    }
    match tool(app, user, "update_record", args) {
        Ok(_) => Ok(Out::Go(format!("/admin/r/{id}?saved=1"))),
        Err(e) => retry(StatusCode::BAD_REQUEST, e),
    }
}

fn create_blocking(app: &App, user: &AdminUser, name: &str, posted: Posted) -> Result<Out, Out> {
    let def = collection(app, name)?;
    let note = posted.note(CREATE_NOTE);
    let retry = |error: String| {
        collection_page(app, user, name, &vec![], Some(Typed { raw: posted.fields.clone(), note: note.clone(), error, status: StatusCode::BAD_REQUEST, seen: None }))
    };
    // Empty inputs are left out, so fields with a default get it.
    let mut data = Map::new();
    for f in &def.fields {
        if let Some(v) = posted.fields.get(&f.name).map(|raw| from_raw(f, raw)).filter(|v| !v.is_null()) {
            data.insert(f.name.clone(), v);
        }
    }
    if let Err(e) = store_files(app, user, &def, &posted, &json!({}), &mut data) {
        return retry(e);
    }
    match tool(app, user, "create_record", json!({ "collection": name, "data": data, "note": note })) {
        Ok(r) => Ok(Out::Go(format!("/admin/r/{}?saved=1", r["id"]))),
        Err(e) => retry(e),
    }
}

/// Runs `delete_record` or `discard_draft`; a record that is gone afterwards sends you back to its list.
/// Deleting or discarding a record that was never published removes it for good, so both ask first: the
/// confirmation posts to `/delete` with `confirm=1`.
fn finish_blocking(app: &App, user: &AdminUser, id: i64, posted: Posted, delete: bool) -> Result<Out, Out> {
    let row = load(app, id)?;
    if row.published.is_none() && posted.one("confirm") != "1" {
        let def = collection(app, &row.collection)?;
        let (current, _) = docs(app, &def, &row)?;
        let ctx = json!({
            "heading": "删除这条草稿？",
            "message": format!("“{}”从没发布过，删除后不会留下历史版本，也不能找回。", record_title(&def, &current, id)),
            "action": format!("/admin/r/{id}/delete"),
            "back": format!("/admin/r/{id}"),
            "note": posted.note(DELETE_NOTE),
            "button": "确认删除",
        });
        return Ok(Out::Page(StatusCode::OK, "confirm.html", shell(app, user, &format!("c:{}", row.collection), ctx)));
    }
    let done = if delete {
        tool(app, user, "delete_record", json!({ "id": id, "note": posted.note(DELETE_NOTE) }))
    } else {
        tool(app, user, "discard_draft", json!({ "target": format!("record:{id}") }))
    };
    match done {
        Ok(r) if r["status"] == "deleted" => Ok(Out::Go(format!("/admin/c/{}", row.collection))),
        Ok(_) => Ok(Out::Go(format!("/admin/r/{id}"))),
        Err(error) => record_page(app, user, id, &vec![], Some(Typed { raw: HashMap::new(), note: EDIT_NOTE.into(), error, status: StatusCode::BAD_REQUEST, seen: None })),
    }
}

fn media_page(app: &App, user: &AdminUser, error: Option<String>) -> Result<Out, Out> {
    let files: Vec<Value> = media::list(app).into_iter().map(|(name, image)| json!({ "name": name, "image": image })).collect();
    let status = if error.is_some() { StatusCode::BAD_REQUEST } else { StatusCode::OK };
    Ok(Out::Page(status, "media.html", shell(app, user, "media", json!({ "files": files, "error": error, "max_mb": media::MAX_BYTES >> 20 }))))
}

fn upload_blocking(app: &App, user: &AdminUser, posted: Posted) -> Result<Out, Out> {
    if !user.actor().can(Scope::Write) {
        return Err(Out::Fail(StatusCode::FORBIDDEN, format!("“{}”没有权限上传：需要 write 权限。", user.email)));
    }
    let files: Vec<_> = posted.files.iter().filter(|(input, _, _)| input == "file").collect();
    if files.is_empty() {
        return media_page(app, user, Some("没有收到文件：请先选择要上传的图片或 PDF。".into()));
    }
    for (_, name, bytes) in files {
        if let Err(e) = media::store_upload(app, name, bytes) {
            return media_page(app, user, Some(e));
        }
    }
    Ok(Out::Go("/admin/media".into()))
}

// ---- routes ----

pub async fn collection_list(State(app): State<Shared>, Extension(mut user): Extension<AdminUser>, Path(name): Path<String>, Query(params): Query<Params>) -> Response {
    blocking(move || {
        // Opening the list marks its submissions as seen, so the nav's "N 条新" clears.
        if crate::schema::valid_name(&name) {
            mark_seen(&app, &mut user, &name);
        }
        collection_page(&app, &user, &name, &params, None)
    })
    .await
}

/// `GET /admin/members`: 用户管理.
pub async fn members_list(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Query(params): Query<Params>) -> Response {
    blocking(move || members_page(&app, &user, &params, None)).await
}

pub async fn view_list(State(app): State<Shared>, Extension(mut user): Extension<AdminUser>, Path(name): Path<String>, Query(params): Query<Params>) -> Response {
    blocking(move || {
        // A screen over a collection shows its submissions: opening it clears that collection's "N 条新" too.
        let over: Option<String> = app
            .conn()
            .query_row("SELECT json_extract(coalesce(draft, published), '$.collection') FROM views WHERE name = ?1", [&name], |r| r.get(0))
            .ok()
            .flatten();
        if let Some(c) = over.filter(|c| crate::schema::valid_name(c)) {
            mark_seen(&app, &mut user, &c);
        }
        view_page(&app, &user, &name, &params)
    })
    .await
}

/// Records that this admin has looked at `collection`'s submissions.
fn mark_seen(app: &App, user: &mut AdminUser, collection: &str) {
    let conn = app.conn();
    if let Ok(now) = conn.query_row("SELECT datetime('now')", [], |r| r.get::<_, String>(0)) {
        if super::session::set_admin_pref(&conn, user.id, &format!("seen_{collection}"), &now).is_ok() {
            user.prefs[format!("seen_{collection}")] = json!(now);
        }
    }
}

pub async fn record(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Path(id): Path<i64>, Query(params): Query<Params>) -> Response {
    blocking(move || record_page(&app, &user, id, &params, None)).await
}

pub async fn save(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Path(id): Path<i64>, req: Request) -> Response {
    match checked(&user, req).await {
        Ok(posted) => blocking(move || save_blocking(&app, &user, id, posted)).await,
        Err(o) => o.into_response(),
    }
}

pub async fn create(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Path(name): Path<String>, req: Request) -> Response {
    match checked(&user, req).await {
        Ok(posted) => blocking(move || create_blocking(&app, &user, &name, posted)).await,
        Err(o) => o.into_response(),
    }
}

pub async fn delete(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Path(id): Path<i64>, req: Request) -> Response {
    match checked(&user, req).await {
        Ok(posted) => blocking(move || finish_blocking(&app, &user, id, posted, true)).await,
        Err(o) => o.into_response(),
    }
}

pub async fn discard(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Path(id): Path<i64>, req: Request) -> Response {
    match checked(&user, req).await {
        Ok(posted) => blocking(move || finish_blocking(&app, &user, id, posted, false)).await,
        Err(o) => o.into_response(),
    }
}

pub async fn media_library(State(app): State<Shared>, Extension(user): Extension<AdminUser>) -> Response {
    blocking(move || media_page(&app, &user, None)).await
}

pub async fn upload(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    match checked(&user, req).await {
        Ok(posted) => blocking(move || upload_blocking(&app, &user, posted)).await,
        Err(o) => o.into_response(),
    }
}

const MEMBER_PAGE: i64 = 20;

/// 用户管理: the site's members. Its title and columns come from a view named `members` when the chat made one.
pub(super) fn members_page(app: &App, user: &AdminUser, params: &Params, error: Option<String>) -> Result<Out, Out> {
    let (live, on) = {
        let conn = app.conn();
        (crate::members::login_enabled(&conn, false), crate::members::login_enabled(&conn, true))
    };
    if !on {
        return Err(Out::Fail(StatusCode::NOT_FOUND, "网站没有打开会员登录，所以没有用户管理。".into()));
    }
    let def = crate::members::members_def();
    let views = tool(app, user, "list_views", json!({})).map_err(internal)?;
    let custom: Option<ViewDef> = views
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["definition"]["collection"] == crate::members::MEMBERS)
        .and_then(|v| serde_json::from_value(v["definition"].clone()).ok());
    let heading = custom.as_ref().map_or_else(|| "用户管理".to_string(), |v| v.title.clone());
    let names: Vec<String> = custom.as_ref().map_or_else(|| def.fields.iter().map(|f| f.name.clone()).collect(), |v| v.columns.clone());
    let columns: Vec<&crate::schema::FieldDef> = names.iter().filter_map(|n| def.field(n)).collect();
    let tab = match param(params, "status") {
        s @ ("active" | "disabled") => s,
        _ => "",
    };
    let q = param(params, "q").trim().to_string();
    let page = param(params, "page").parse::<i64>().unwrap_or(1).max(1);
    let mut args = json!({ "limit": MEMBER_PAGE, "offset": (page - 1) * MEMBER_PAGE });
    if !tab.is_empty() {
        args["status"] = json!(tab);
    }
    if !q.is_empty() {
        args["query"] = json!(q);
    }
    let list = tool(app, user, "list_members", args).map_err(internal)?;
    let total = list["total"].as_i64().unwrap_or(0);
    let shown = |name: &str, v: &Value| -> String {
        match (name, v.as_str()) {
            ("status", Some("active")) => "正常".into(),
            ("status", Some("disabled")) => "已停用".into(),
            ("created_at" | "last_login", Some(t)) => t.chars().take(16).collect(),
            (_, Some(t)) => t.to_string(),
            _ => String::new(),
        }
    };
    let rows: Vec<Value> = list["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| json!({ "id": m["id"], "disabled": m["status"] == "disabled", "cells": columns.iter().map(|f| shown(&f.name, &m[&f.name])).collect::<Vec<_>>() }))
        .collect();
    let href = |status: &str, page: i64| {
        let mut parts = vec![];
        if !status.is_empty() {
            parts.push(format!("status={status}"));
        }
        if !q.is_empty() {
            parts.push(format!("q={}", url_escape(&q)));
        }
        if page > 1 {
            parts.push(format!("page={page}"));
        }
        if parts.is_empty() { "/admin/members".to_string() } else { format!("/admin/members?{}", parts.join("&")) }
    };
    let pages = ((total + MEMBER_PAGE - 1) / MEMBER_PAGE).max(1);
    let tabs: Vec<Value> = [("", "全部"), ("active", "正常"), ("disabled", "已停用")].iter().map(|(k, l)| json!({ "href": href(k, 1), "label": l, "on": *k == tab })).collect();
    let ctx = json!({
        "heading": heading,
        "summary": format!("共 {total} 人"),
        "live": live,
        "tab": tab,
        "q": q,
        "error": error,
        "tabs": tabs,
        "columns": columns.iter().map(|f| json!({ "name": f.name, "title": f.title })).collect::<Vec<_>>(),
        "rows": rows,
        "pager": (pages > 1).then(|| json!({ "page": page, "pages": pages,
            "prev": (page > 1).then(|| href(tab, page - 1)), "next": (page < pages).then(|| href(tab, page + 1)) })),
    });
    Ok(Out::Page(StatusCode::OK, "members.html", shell(app, user, "members", ctx)))
}

fn url_escape(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

#[derive(serde::Deserialize)]
pub struct MemberStatusForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    status: String,
}

/// `POST /admin/members/<id>/status`: 停用 or 恢复 a member, then back to the list.
pub async fn member_status(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Path(id): Path<i64>, axum::Form(f): axum::Form<MemberStatusForm>) -> Response {
    if let Err(s) = super::session::check_csrf(&user, &f.csrf) {
        return s.into_response();
    }
    blocking(move || {
        let note = if f.status == "disabled" { "后台停用会员" } else { "后台恢复会员" };
        match tool(&app, &user, "set_member_status", json!({ "id": id, "status": f.status, "note": note })) {
            Ok(_) => Ok(Out::Go("/admin/members".into())),
            Err(e) => members_page(&app, &user, &vec![], Some(e)),
        }
    })
    .await
}
