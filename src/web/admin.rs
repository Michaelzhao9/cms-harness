//! Admin pages: the shared nav, the shell's home page, the nav preference, the publish review and the release history.

use super::content::{self, Out};
use super::session::{self, AdminUser};
use crate::auth::Scope;
use crate::changes::{self, Target};
use crate::{App, Shared};
use axum::extract::{Query, Request, State};
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

/// The change-mark colours an admin can pick: (key, name). `admin.css` maps each `mark-<key>` body class to a colour.
pub const MARKS: [(&str, &str); 5] = [("yellow", "黄"), ("blue", "蓝"), ("green", "绿"), ("coral", "珊瑚"), ("purple", "紫")];

/// The nav context every admin page passes to `base.html`. `current` names the page: `web`, `c:<name>`, `v:<view>`,
/// `media`, `publish`, `releases` or `settings`. It also carries the admin's look: `mark` (a `MARKS` key) and
/// `chat` (`docked` or `floating`).
pub fn nav(conn: &Connection, user: &AdminUser, current: &str) -> Value {
    let mark = user.prefs["mark"].as_str().filter(|m| MARKS.iter().any(|(k, _)| k == m)).unwrap_or("yellow");
    json!({
        "current": current,
        "collapsed": user.prefs["nav"] == "collapsed",
        "mark": mark,
        "chat": user.prefs["chat"].as_str().filter(|c| CHAT_MODES.contains(c)).unwrap_or("docked"),
        // 用户管理 is there exactly while member login is on (live or drafted) and gone when it is off.
        "members": crate::members::login_enabled(conn, true),
        // A database error leaves the nav without collections rather than failing the page.
        "collections": nav_collections(conn).unwrap_or_default().into_iter().map(|mut c| {
            let name = c["name"].as_str().unwrap_or_default().to_string();
            // Visitor and member submissions this admin has not looked at yet.
            c["fresh"] = json!(crate::access::new_submissions(conn, &name, user.prefs[format!("seen_{name}")].as_str()));
            c
        }).collect::<Vec<_>>(),
    })
}

#[derive(Deserialize)]
pub struct PrefsForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    nav: String,
    #[serde(default)]
    chat: String,
}

/// The chat's sizes: the left panel, the bar at the bottom of the work area, the round button.
pub const CHAT_MODES: [&str; 3] = ["docked", "floating", "mini"];

/// The path to go back to: the `Referer` when it is one of our own `/admin` pages, else `/admin`.
pub(super) fn back_to(origin: &str, headers: &HeaderMap) -> String {
    headers
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.strip_prefix(origin))
        .filter(|p| *p == "/admin" || p.starts_with("/admin/") || p.starts_with("/admin?"))
        .unwrap_or("/admin")
        .to_string()
}

pub async fn prefs(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Extension(addr): Extension<super::address::Address>, headers: HeaderMap, Form(f): Form<PrefsForm>) -> Response {
    if let Err(s) = session::check_csrf(&user, &f.csrf) {
        return (s, "表单已失效，请刷新页面后再试。").into_response();
    }
    let (key, value) = match (f.nav.as_str(), f.chat.as_str()) {
        ("collapsed" | "open", "") => ("nav", f.nav),
        ("", c) if CHAT_MODES.contains(&c) => ("chat", f.chat),
        _ => return (StatusCode::BAD_REQUEST, "导航只能收起（collapsed）或展开（open）；对话只能是 docked、floating 或 mini。").into_response(),
    };
    let a = app.clone();
    let saved = tokio::task::spawn_blocking(move || session::set_pref(&a.conn(), &user.token_hash, key, &value))
        .await
        .unwrap_or_else(|e| Err(format!("服务器内部错误：{e}")));
    match saved {
        Ok(()) => Redirect::to(&back_to(&addr.here, &headers)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

// ---- publish review ----

const PUBLISH_NOTE: &str = "后台发布";
const RESTORE_NOTE: &str = "后台恢复历史版本";
const UNSET: &str = "未填";

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "created" => "新建",
        "deleted" => "删除",
        _ => "修改",
    }
}

/// A definition key as people say it.
fn key_label(key: &str) -> &str {
    match key {
        "title" => "标题",
        "fields" => "字段",
        "public_read" | "read" => "谁能看",
        "create" => "谁能提交",
        "single" => "单条",
        "columns" => "列",
        "filters" => "筛选",
        "sort" => "排序",
        "actions" => "操作",
        "collection" => "集合",
        k => k,
    }
}

/// One line, at most 60 characters; `未填` when empty.
fn line(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match s.chars().count() {
        0 => UNSET.into(),
        n if n > 60 => format!("{}…", s.chars().take(60).collect::<String>()),
        _ => s,
    }
}

/// Where a pending change sits, and old against new for each field it changes. Anything that cannot be read
/// any more (a collection gone meanwhile) falls back to the pending entry's own title.
fn describe(app: &App, p: &Value, target: &Target) -> (Vec<String>, Vec<Value>, Option<String>) {
    let title = p["title"].as_str().unwrap_or_default().to_string();
    let changed = p["kind"] == "changed";
    match target {
        Target::Record(id) => {
            let found = content::load(app, *id).ok().and_then(|row| {
                let def = content::collection(app, &row.collection).ok()?;
                let (current, live) = content::docs(app, &def, &row).ok()?;
                Some((row.draft.is_some(), def, current, live))
            });
            let Some((has_draft, def, current, live)) = found else { return (vec![title], vec![], None) };
            let mut pairs = vec![];
            if let (true, true, Some(live)) = (changed, has_draft, &live) {
                for f in def.fields.iter().filter(|f| live[&f.name] != current[&f.name]) {
                    let (old, new) = (content::show(f, &live[&f.name]), content::show(f, &current[&f.name]));
                    pairs.push(json!({ "field": f.title, "old": if old.is_empty() { UNSET.into() } else { old }, "new": if new.is_empty() { UNSET.into() } else { new } }));
                }
            }
            (vec![def.title.clone(), content::record_title(&def, &current, *id)], pairs, None)
        }
        Target::Notes => {
            let texts: Option<(Option<String>, Option<String>)> = app.conn().query_row("SELECT published, draft FROM notes WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?))).ok();
            let text = |j: Option<String>| j.and_then(|j| serde_json::from_str::<String>(&j).ok()).unwrap_or_default();
            let (old, new) = texts.map(|(o, n)| (text(o), text(n))).unwrap_or_default();
            // A first version has nothing to strike through.
            let old = changed.then(|| line(&old));
            (vec![title], vec![json!({ "field": "内容", "old": old, "new": line(&new) })], None)
        }
        Target::Collection(_) | Target::View(_) => {
            let what = if matches!(target, Target::Collection(_)) { "集合" } else { "界面" };
            let keys: Vec<&str> = p["fields"].as_array().into_iter().flatten().filter_map(Value::as_str).map(key_label).collect();
            let summary = (changed && !keys.is_empty()).then(|| format!("改了{}", keys.join("、")));
            (vec![what.to_string(), title], vec![], summary)
        }
        Target::Setting(key) if key == "member_login" => {
            let on = |v: Value| if v == json!(true) { "开" } else { "关" };
            let conn = app.conn();
            let (old, new) = (on(crate::changes::setting(&conn, key, false)), on(crate::changes::setting(&conn, key, true)));
            (vec!["设置".into(), title], vec![json!({ "field": "状态", "old": changed.then_some(old), "new": new })], None)
        }
        // The import map and script origins decide what code the live site runs: the reviewer sees every entry that
        // changes, each in full, never a cut-off dump that padding could push the dangerous line out of.
        Target::Setting(key) => {
            let conn = app.conn();
            let (old, new) = (crate::changes::setting(&conn, key, false), crate::changes::setting(&conn, key, true));
            (vec!["设置".into(), title], setting_pairs(key, &old, &new), None)
        }
        // Path and line counts only: a file can be a megabyte long.
        Target::File(path) => {
            let n = |i: usize| p["lines"][i].as_i64().unwrap_or_default();
            let (old, new) = (n(0), n(1));
            let summary = match p["kind"].as_str() {
                _ if p["lines"].is_null() => "二进制文件".to_string(),
                Some("created") => format!("共 {new} 行"),
                Some("deleted") => format!("原有 {old} 行"),
                _ if new == old => format!("{old} 行，行数不变"),
                _ => format!("{old} 行 → {new} 行（{:+}）", new - old),
            };
            (vec!["站点文件".into(), path.clone()], vec![], Some(summary))
        }
    }
}

/// The scopes the actor lacks to publish this target, for the "needs" line.
fn missing_scopes(actor: &crate::auth::Actor, target: &Target) -> String {
    let need = [changes::own_scope(target), Scope::Publish];
    need.iter()
        .filter(|s| !actor.can(**s))
        .filter_map(|s| serde_json::to_value(s).ok()?.as_str().map(String::from))
        .collect::<Vec<_>>()
        .join(" 和 ")
}

/// The review: every pending change, a box for each one the admin may publish, and the dry run of the default
/// release (everything they may publish). `checked` is what a refused form had ticked; `None` ticks all.
fn publish_page(app: &App, user: &AdminUser, checked: Option<&[String]>, note: &str, error: Option<String>) -> Result<Out, Out> {
    let actor = user.actor();
    let pending = content::tool(app, user, "list_pending", json!({})).map_err(content::internal)?;
    let mut items = vec![];
    let mut publishable = vec![];
    for p in pending.as_array().into_iter().flatten() {
        let name = p["target"].as_str().unwrap_or_default();
        let Ok(target) = Target::parse(name) else { continue };
        let can = changes::may_publish(&actor, &target);
        let (crumb, pairs, summary) = describe(app, p, &target);
        items.push(json!({
            "target": name, "kind": p["kind"], "kind_label": kind_label(p["kind"].as_str().unwrap_or_default()),
            "crumb": crumb, "pairs": pairs, "summary": summary, "actor": p["actor"], "note": p["note"], "can": can,
            "checked": can && checked.is_none_or(|c| c.iter().any(|t| t == name)), "need": missing_scopes(&actor, &target),
        }));
        if can {
            publishable.push(target);
        }
    }
    let check = if items.is_empty() {
        Value::Null
    } else if publishable.is_empty() {
        json!({ "ok": false, "problems": ["这些改动你都没有权限发布。"] })
    } else {
        match changes::dry_run_publish(&app.conn(), &actor, Some(&publishable)) {
            Ok(_) => json!({ "ok": true }),
            Err(e) => {
                let rest = e.strip_prefix("发布被拦下，什么都没有上线。").unwrap_or(&e);
                json!({ "ok": false, "problems": rest.split('；').collect::<Vec<_>>() })
            }
        }
    };
    let status = if error.is_some() { StatusCode::BAD_REQUEST } else { StatusCode::OK };
    let schedules = crate::schedule::list(&app.conn()).map_err(content::internal)?;
    let can_publish = actor.can(crate::auth::Scope::Publish);
    let ctx = json!({ "items": items, "check": check, "publishable": publishable.len(), "note": note, "error": error,
        "schedules": schedules, "can_publish": can_publish });
    Ok(Out::Page(status, "publish.html", content::shell(app, user, "publish", ctx)))
}

/// A human name for a version: its document's title, else what it is.
fn version_title(target: &Target, doc: &Value) -> String {
    let named = || ["title", "name"].iter().find_map(|k| doc.get(*k).and_then(Value::as_str)).filter(|s| !s.is_empty()).map(String::from);
    match target {
        Target::Notes => "站点说明".into(),
        Target::Record(id) => named().unwrap_or_else(|| format!("记录 {id}")),
        Target::Collection(n) | Target::View(n) => named().unwrap_or_else(|| n.clone()),
        Target::File(p) => p.clone(),
        Target::Setting(k) => crate::changes::setting_title(k).into(),
    }
}

/// Versions shown per release; a definition change can rewrite thousands of records in one.
const VERSIONS_SHOWN: i64 = 20;

/// Releases newest first (the latest 50, or just `only`), each with its versions.
// ponytail: the latest 50 releases, no paging; add `?before=<id>` when history grows long.
fn releases(conn: &Connection, actor: &crate::auth::Actor, only: Option<i64>) -> rusqlite::Result<Vec<Value>> {
    // Visitor and member submissions are history too (versions keep them) but not releases a person made.
    let mut stmt = conn.prepare(
        "SELECT id, actor, note, created_at FROM releases WHERE (?1 IS NULL OR id = ?1) AND actor != '访客' AND actor NOT LIKE 'member:%' AND actor NOT LIKE 'public:%' ORDER BY id DESC LIMIT 50",
    )?;
    let heads: Vec<(i64, String, String, String)> = stmt.query_map([only], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut versions = conn.prepare("SELECT id, target, doc, prev, actor, note FROM versions WHERE release_id = ?1 ORDER BY id LIMIT ?2")?;
    let mut out = vec![];
    for (id, who, note, at) in heads {
        let total: i64 = conn.query_row("SELECT count(*) FROM versions WHERE release_id = ?1", [id], |r| r.get(0))?;
        let parse = |j: &Option<String>| j.as_deref().and_then(|j| serde_json::from_str::<Value>(j).ok());
        let items: Vec<Value> = versions
            .query_map(rusqlite::params![id, VERSIONS_SHOWN], |r| {
                let (target, doc, prev): (String, Option<String>, Option<String>) = (r.get(1)?, r.get(2)?, r.get(3)?);
                let Ok(t) = Target::parse(&target) else { return Ok(None) };
                let shown = parse(&doc).or_else(|| parse(&prev)).unwrap_or(Value::Null);
                let kind = if doc.is_none() { "deleted" } else if prev.is_none() { "created" } else { "changed" };
                let can_restore = doc.is_some() && actor.can(Scope::Write) && actor.can(changes::own_scope(&t));
                Ok(Some(json!({ "id": r.get::<_, i64>(0)?, "target": target, "title": version_title(&t, &shown), "kind_label": kind_label(kind),
                    "actor": r.get::<_, String>(4)?, "note": r.get::<_, String>(5)?, "can_restore": can_restore })))
            })?
            .filter_map(Result::transpose)
            .collect::<rusqlite::Result<_>>()?;
        out.push(json!({ "id": id, "actor": who, "note": note, "at": at, "versions": items, "more": (total - VERSIONS_SHOWN).max(0) }));
    }
    Ok(out)
}

fn releases_page(app: &App, user: &AdminUser, restored: bool, error: Option<String>) -> Result<Out, Out> {
    let list = releases(&app.conn(), &user.actor(), None).map_err(|e| content::internal(format!("读取发布记录失败：{e}")))?;
    let status = if error.is_some() { StatusCode::BAD_REQUEST } else { StatusCode::OK };
    let ctx = json!({ "releases": list, "restored": restored, "error": error, "restore_note": RESTORE_NOTE });
    Ok(Out::Page(status, "releases.html", content::shell(app, user, "releases", ctx)))
}

pub async fn publish_review(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Query(q): Query<Vec<(String, String)>>) -> Response {
    let done = q.iter().find(|(k, _)| k == "done").and_then(|(_, v)| v.parse::<i64>().ok());
    content::blocking(move || match done {
        None => publish_page(&app, &user, None, PUBLISH_NOTE, None),
        Some(id) => {
            let list = releases(&app.conn(), &user.actor(), Some(id)).map_err(|e| content::internal(format!("读取发布记录失败：{e}")))?;
            let release = list.into_iter().next().ok_or_else(|| Out::Fail(StatusCode::NOT_FOUND, format!("没有编号为 {id} 的发布记录。")))?;
            Ok(Out::Page(StatusCode::OK, "publish.html", content::shell(&app, &user, "publish", json!({ "done": release }))))
        }
    })
    .await
}

pub async fn publish(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    let posted = match content::checked(&user, req).await {
        Ok(p) => p,
        Err(o) => return o.into_response(),
    };
    content::blocking(move || {
        let targets = posted.fields.get("target").cloned().unwrap_or_default();
        let note = posted.note(PUBLISH_NOTE);
        // Always a list: an absent `targets` would mean "everything".
        match content::tool(&app, &user, "publish", json!({ "targets": targets, "note": note })) {
            Ok(r) => Ok(Out::Go(format!("/admin/publish?done={}", r["release_id"]))),
            Err(e) => publish_page(&app, &user, Some(&targets), &note, Some(e)),
        }
    })
    .await
}

/// `POST /admin/publish/schedule`: the checked changes go live at the time given (in the browser's zone, `tz` minutes
/// ahead of UTC; admin.js fills it in, UTC without it).
pub async fn schedule(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    let posted = match content::checked(&user, req).await {
        Ok(p) => p,
        Err(o) => return o.into_response(),
    };
    content::blocking(move || {
        let targets = posted.fields.get("target").cloned().unwrap_or_default();
        let note = posted.note(PUBLISH_NOTE);
        let tz = posted.one("tz").trim().parse::<i64>().unwrap_or(0).clamp(-14 * 60, 14 * 60);
        let made = {
            let conn = app.conn();
            crate::schedule::normalize(&conn, posted.one("at"), tz).and_then(|at| crate::schedule::create(&conn, &user.actor(), &at, Some(targets.clone()), &note))
        };
        match made {
            Ok(_) => Ok(Out::Go("/admin/publish?scheduled=1".into())),
            Err(e) => publish_page(&app, &user, Some(&targets), &note, Some(e)),
        }
    })
    .await
}

/// `POST /admin/publish/schedule/<id>/cancel`.
pub async fn cancel_schedule(State(app): State<Shared>, Extension(user): Extension<AdminUser>, axum::extract::Path(id): axum::extract::Path<i64>, req: Request) -> Response {
    if let Err(o) = content::checked(&user, req).await {
        return o.into_response();
    }
    content::blocking(move || {
        if !user.actor().can(crate::auth::Scope::Publish) {
            return publish_page(&app, &user, None, PUBLISH_NOTE, Some("你没有发布权限，不能取消定时发布。".into()));
        }
        // The lock is released before the page below takes it again.
        let cancelled = crate::schedule::cancel(&app.conn(), id);
        match cancelled {
            Ok(()) => Ok(Out::Go("/admin/publish".into())),
            Err(e) => publish_page(&app, &user, None, PUBLISH_NOTE, Some(e)),
        }
    })
    .await
}

pub async fn release_history(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Query(q): Query<Vec<(String, String)>>) -> Response {
    let restored = q.iter().any(|(k, v)| k == "restored" && v == "1");
    content::blocking(move || releases_page(&app, &user, restored, None)).await
}

pub async fn restore(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    let posted = match content::checked(&user, req).await {
        Ok(p) => p,
        Err(o) => return o.into_response(),
    };
    content::blocking(move || {
        let Ok(version_id) = posted.one("version_id").parse::<i64>() else {
            return releases_page(&app, &user, false, Some(format!("版本编号“{}”不对：请从发布记录里选一个版本。", posted.one("version_id"))));
        };
        let args = json!({ "target": posted.one("target"), "version_id": version_id, "note": posted.note(RESTORE_NOTE) });
        match content::tool(&app, &user, "restore_version", args) {
            Ok(_) => Ok(Out::Go("/admin/releases?restored=1".into())),
            Err(e) => releases_page(&app, &user, false, Some(e)),
        }
    })
    .await
}

/// Old against new for each entry of an import map (`imports.<specifier>`, `scopes.<scope>.<specifier>`) or each
/// script origin that is added or removed. Unchanged entries are left out; changed ones are shown in full.
fn setting_pairs(key: &str, old: &Value, new: &Value) -> Vec<Value> {
    let gone = || Some("（去掉）".to_string());
    if key == "languages" {
        let names = |v: &Value| v["list"].as_array().into_iter().flatten().filter_map(|l| l["name"].as_str()).collect::<Vec<_>>().join("、");
        let sync = |v: &Value| if v["sync"].as_bool() == Some(true) { "开" } else { "关" };
        let mut out = vec![json!({ "field": "网站语言", "old": Some(names(old)).filter(|s| !s.is_empty()), "new": Some(names(new)).filter(|s| !s.is_empty()).or_else(gone) })];
        if old["sync"] != new["sync"] {
            out.push(json!({ "field": "翻译自动同步", "old": sync(old), "new": sync(new) }));
        }
        return out;
    }
    if key == "strings" {
        let count = |v: &Value| v.as_object().map_or(0, |o| o.values().map(|t| t.as_object().map_or(0, |t| t.len())).sum::<usize>());
        return vec![json!({ "field": "站点文字翻译", "old": format!("{} 条", count(old)), "new": format!("{} 条", count(new)) })];
    }
    if key == "site_name" {
        return vec![json!({ "field": "站点名称", "old": old.as_str(), "new": new.as_str().map(String::from).or_else(gone) })];
    }
    if key == crate::canvas::KEY {
        // One row per changed property: element (its file and position), width, property.
        let flat = |v: &Value| {
            let mut m = std::collections::BTreeMap::new();
            for (el, e) in v["els"].as_object().into_iter().flatten() {
                let scopes = std::iter::once((el.clone(), &e["style"]))
                    .chain(e["pages"].as_object().into_iter().flatten().map(|(page, s)| (format!("{el} · {page}"), s)));
                for (name, style) in scopes {
                    for (tier, props) in style.as_object().into_iter().flatten() {
                        for (p, val) in props.as_object().into_iter().flatten() {
                            m.insert((name.clone(), tier.clone(), p.clone()), val.as_str().unwrap_or_default().to_string());
                        }
                    }
                }
            }
            m
        };
        let (o, n) = (flat(old), flat(new));
        let label = |(el, tier, p): &(String, String, String)| {
            let tier = match tier.as_str() { "desktop" => "电脑", "tablet" => "平板", "mobile" => "手机", _ => "所有宽度" };
            format!("{el} · {tier} · {p}")
        };
        let mut out: Vec<Value> = n.iter().filter(|(k, v)| o.get(*k) != Some(*v)).map(|(k, v)| json!({ "field": label(k), "old": o.get(k), "new": v })).collect();
        out.extend(o.iter().filter(|(k, _)| !n.contains_key(*k)).map(|(k, v)| json!({ "field": label(k), "old": v, "new": gone() })));
        return out;
    }
    if key == "script_origins" {
        let list = |v: &Value| v.as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect::<Vec<_>>();
        let (o, n) = (list(old), list(new));
        let mut out: Vec<Value> = n.iter().filter(|x| !o.contains(x)).map(|x| json!({ "field": "允许加载脚本", "old": null, "new": x })).collect();
        out.extend(o.iter().filter(|x| !n.contains(x)).map(|x| json!({ "field": "不再允许", "old": x, "new": gone() })));
        if new.is_null() {
            out.push(json!({ "field": "脚本来源", "old": null, "new": "恢复默认（https://esm.sh）" }));
        }
        return out;
    }
    let flat = |v: &Value| {
        let mut m = std::collections::BTreeMap::new();
        for (k, url) in v["imports"].as_object().into_iter().flatten() {
            m.insert(k.clone(), url.as_str().map(String::from).unwrap_or_else(|| url.to_string()));
        }
        for (scope, entries) in v["scopes"].as_object().into_iter().flatten() {
            for (k, url) in entries.as_object().into_iter().flatten() {
                m.insert(format!("{k}（只在 {scope} 下）"), url.as_str().map(String::from).unwrap_or_else(|| url.to_string()));
            }
        }
        m
    };
    let (o, n) = (flat(old), flat(new));
    let mut out = vec![];
    for (k, url) in &n {
        if o.get(k) != Some(url) {
            out.push(json!({ "field": k, "old": o.get(k), "new": url }));
        }
    }
    for (k, url) in &o {
        if !n.contains_key(k) {
            out.push(json!({ "field": k, "old": url, "new": gone() }));
        }
    }
    out
}
