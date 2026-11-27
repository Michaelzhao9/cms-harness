//! The workbench: the site preview page, and the chat on every admin page — posting a message, starting over,
//! stopping, the event stream, and the view that both the templates and `admin.js` render.

use super::address::Address;
use super::admin::back_to;
use super::content::{self, Out};
use super::session::AdminUser;
use crate::changes::{self, Target};
use crate::chat::{self, Attachment, Change, Conversation};
use crate::llm::{Block, Role};
use crate::{App, Shared};
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Extension;
use serde_json::{json, Value};
use std::time::Duration;

/// How long one answer of the event stream waits for news before it says "nothing yet".
const LONGEST_WAIT: u64 = 25;
/// What the browser waits before asking the event stream again (`retry:`), in milliseconds.
const RETRY_MS: u32 = 300;

/// The site page a site file serves, when it is one: `index.html` → `/`, `news/index.html` → `/news/`, `about.html` → `/about`.
fn page_of(path: &str) -> Option<String> {
    if let Some(dir) = path.strip_suffix("index.html") {
        return Some(format!("/{dir}"));
    }
    path.strip_suffix(".html").filter(|p| !p.starts_with('_') && *p != "404").map(|p| format!("/{p}"))
}

/// One change line for the chat's change list: its number (its target's place in `list_pending`, none once
/// published or dropped), where it is, the old and new value, and where clicking goes.
fn change_line(app: &App, ch: &Change, order: &[String]) -> Value {
    let n = order.iter().position(|t| *t == ch.target).map(|i| i + 1);
    let mut line = json!({ "n": n, "crumb": [], "field": ch.field, "before": "", "after": "", "href": "/admin", "page": null });
    match Target::parse(&ch.target) {
        Ok(Target::Record(id)) => {
            line["href"] = json!(format!("/admin/r/{id}"));
            let found = content::load(app, id).ok().and_then(|row| {
                let def = content::collection(app, &row.collection).ok()?;
                let (current, _) = content::docs(app, &def, &row).ok()?;
                Some((def, current))
            });
            match found {
                Some((def, current)) => {
                    line["crumb"] = json!([def.title, content::record_title(&def, &current, id)]);
                    if let Some(f) = def.fields.iter().find(|f| f.name == ch.field) {
                        line["field"] = json!(f.title);
                        line["before"] = json!(content::show(f, &ch.before));
                        line["after"] = json!(content::show(f, &ch.after));
                    }
                }
                None => line["crumb"] = json!([format!("记录 #{id}")]),
            }
        }
        Ok(Target::Collection(name)) => {
            line["href"] = json!(format!("/admin/c/{name}"));
            line["crumb"] = json!(["集合", name]);
        }
        Ok(Target::View(name)) => {
            line["href"] = json!(format!("/admin/v/{name}"));
            line["crumb"] = json!(["界面", name]);
        }
        Ok(Target::File(path)) => {
            let page = page_of(&path);
            line["href"] = json!(page.as_ref().map_or_else(|| "/admin".to_string(), |p| format!("/admin?p={p}")));
            line["page"] = json!(page);
            line["crumb"] = json!(["页面文件", path]);
        }
        Ok(Target::Notes) => {
            line["href"] = json!("/admin/settings#s-notes");
            line["crumb"] = json!(["站点说明"]);
        }
        Ok(Target::Setting(key)) => {
            line["href"] = json!(if key == "member_login" { "/admin/members" } else { "/admin/settings" });
            line["crumb"] = json!(["设置", crate::changes::setting_title(&key)]);
        }
        Err(_) => line["crumb"] = json!([ch.target]),
    }
    line
}

/// The visible text of the assistant's messages.
fn replies(messages: &[crate::llm::Message]) -> Vec<String> {
    messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .map(|m| m.content.iter().filter_map(|b| if let Block::Text(t) = b { Some(t.trim()) } else { None }).collect::<Vec<_>>().join("\n\n"))
        .filter(|t| !t.is_empty())
        .collect()
}

/// The conversation as the chat shows it: one entry per admin message with its steps, replies and changes;
/// the last one also carries what is happening right now.
fn turns(app: &App, c: &Conversation, order: &[String]) -> Vec<Value> {
    let count = c.turns.len();
    c.turns
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let next = c.turns.get(i + 1);
            let last = i + 1 == count;
            // This turn's part of a flat list: from its mark to the next turn's (clamped, so a bad mark never panics).
            fn part<T>(all: &[T], from: usize, to: Option<usize>) -> &[T] {
                let to = to.unwrap_or(all.len()).min(all.len());
                &all[from.min(to)..to]
            }
            let msgs = part(&c.messages, t.messages + 1, next.map(|n| n.messages));
            let steps = part(&c.steps, t.steps, next.map(|n| n.steps));
            let changes = part(&c.changes, t.changes, next.map(|n| n.changes));
            json!({
                "k": i + 1,
                "text": t.text,
                "images": t.images,
                "steps": steps,
                "replies": replies(msgs),
                "changes": changes.iter().map(|ch| change_line(app, ch, order)).collect::<Vec<_>>(),
                "streaming": if last { c.streaming.as_str() } else { "" },
                "running": last && c.running,
                "question": if last { json!(c.question) } else { Value::Null },
                "error": if last { json!(c.error) } else { Value::Null },
                "stopped": last && c.stopped,
                "ok": !(last && (c.running || c.error.is_some() || c.stopped || c.question.is_some())),
            })
        })
        .collect()
}

/// Everything the chat shows, for templates and the event stream.
pub fn view(app: &App, user: &AdminUser) -> Value {
    let c = app.chat.get(&user.token_hash);
    let order = changes::pending_order(&app.conn()).unwrap_or_default();
    let turns = turns(app, &c, &order);
    let current = c.turns.last();
    let steps_now = current.map_or(0, |t| c.steps.len().saturating_sub(t.steps));
    let label = match c.steps.last() {
        _ if !c.streaming.is_empty() => "正在回复".to_string(),
        Some(s) if steps_now > 0 => format!("正在{}", s.label),
        _ => "正在想…".to_string(),
    };
    let last_changes: Vec<&Value> = turns.last().and_then(|t| t["changes"].as_array()).into_iter().flatten().collect();
    let mut places: Vec<String> = vec![];
    for ch in &last_changes {
        let place = ch["crumb"].as_array().and_then(|c| c.last()).and_then(Value::as_str).unwrap_or_default().to_string();
        if !place.is_empty() && !places.contains(&place) {
            places.push(place);
        }
    }
    let lang = crate::i18n::Lang::of_prefs(&user.prefs);
    let t = |s: &str| crate::i18n::tr(lang, s).into_owned();
    let places: Vec<String> = places.iter().take(3).map(|p| t(p)).collect();
    json!({
        "id": format!("{}-{}", c.id, c.revision),
        "running": c.running,
        "stopped": c.stopped,
        "error": c.error.as_deref().map(t),
        "model": app.model().is_some(),
        "builder": user.role == "builder",
        "lang": lang.code(),
        "turns": turns,
        "tasks": c.tasks,
        "status": { "label": t(&label), "step": steps_now },
        "pending": order.len(),
        "summary": { "changes": last_changes.len(), "places": places.join(if lang == crate::i18n::Lang::En { ", " } else { "、" }) },
    })
}

/// The chat log as HTML, the part `admin.js` swaps in when the conversation moves.
fn log_html(view: &Value) -> String {
    crate::render::admin_page("chat_log.html", json!({ "chat": view, "lang": view["lang"] })).unwrap_or_else(|e| e)
}

// ---- the workbench page ----

/// The preview path asked for: a site path, never another origin or the admin.
fn preview_path(p: &str) -> String {
    let ok = p.starts_with('/')
        && !p.starts_with("//")
        && !p.starts_with("/admin")
        && p.len() <= 200
        && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"/-._~%".contains(&b));
    if ok { p.to_string() } else { "/".into() }
}

/// Pages a site file serves, for the address bar's suggestions.
fn routes(app: &App) -> Vec<String> {
    let conn = app.conn();
    let Ok(mut st) = conn.prepare("SELECT path FROM site_files WHERE path LIKE '%.html' AND (published IS NOT NULL OR draft IS NOT NULL) ORDER BY path") else { return vec![] };
    let paths: Vec<String> = st.query_map([], |r| r.get(0)).map(|rows| rows.filter_map(Result::ok).collect()).unwrap_or_default();
    let mut out: Vec<String> = paths.iter().filter_map(|p| page_of(p)).collect();
    if !out.iter().any(|p| p == "/") {
        out.insert(0, "/".into());
    }
    out
}

pub async fn home(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Extension(addr): Extension<Address>, Query(q): Query<Vec<(String, String)>>) -> Response {
    let get = |k: &str| q.iter().rev().find(|(key, _)| key == k).map_or("", |(_, v)| v.as_str()).to_string();
    let (p, w, v, marks) = (preview_path(&get("p")), get("w"), get("v"), get("marks") != "0");
    let width = if matches!(w.as_str(), "tablet" | "mobile") { w } else { "desktop".into() };
    let version = if matches!(v.as_str(), "live" | "split") { v } else { "draft".into() };
    let published = get("published") == "1";
    let chat_ctx = Some(get("ctx")).filter(|c| c.len() <= 200 && !c.contains(['<', '>', '"'])).filter(|c| !c.is_empty()).unwrap_or_else(|| format!("page:{p}"));
    content::blocking(move || {
        let link = |w: &str, v: &str, m: bool| format!("/admin?p={p}&w={w}&v={v}&marks={}", if m { 1 } else { 0 });
        // Drafts come from the admin's own preview entry; 线上 is the live site itself (on its own origin in 分开).
        let token = super::session::issue_preview(&app.conn(), &user.token_hash).map_err(content::internal)?;
        let preview_base = format!("/_hc/preview/{token}");
        // Only a checked origin goes into the page unescaped.
        let live_base = if addr.admin.is_some() { super::address::normalize(&app.origin).unwrap_or_default() } else { String::new() };
        let ctx = json!({
            "workbench": true,
            "path": p,
            "home": p == "/",
            "preview_base": preview_base,
            "live_base": live_base,
            "draft_src": format!("{preview_base}{p}"),
            "live_src": format!("{live_base}{p}"),
            "width": width,
            "version": version,
            "marks": marks,
            "routes": routes(&app),
            "published": published,
            "chat_ctx": chat_ctx,
            "q": { "marks": if marks { "1" } else { "0" } },
            // Links that change one setting of the view and keep the rest.
            "links": {
                "desktop": link("desktop", &version, marks), "tablet": link("tablet", &version, marks), "mobile": link("mobile", &version, marks),
                "draft": link(&width, "draft", marks), "live": link(&width, "live", marks), "split": link(&width, "split", marks),
                "marks": link(&width, &version, !marks), "reload": link(&width, &version, marks),
            },
        });
        let mut ctx = content::shell(&app, &user, "web", ctx);
        // Which 端 the preview is in: the one whose folder the path is under, else the site.
        if let Some(ends) = ctx["nav"]["ends"].as_array_mut() {
            let inside = ends.iter().position(|e| e["name"].as_str().is_some_and(|n| !n.is_empty() && p.trim_start_matches('/').split('/').next() == Some(n)));
            ends[inside.unwrap_or(0)]["here"] = json!(true);
        }
        Ok(Out::Page(StatusCode::OK, "home.html", ctx))
    })
    .await
}

// ---- posts ----

/// `admin.js` posts with this header and gets 204 or a plain error instead of a page.
fn from_script(headers: &HeaderMap) -> bool {
    headers.get("x-hc-fetch").is_some_and(|v| v == "1")
}

fn done(addr: &Address, headers: &HeaderMap) -> Response {
    if from_script(headers) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        Redirect::to(&back_to(&addr.here, headers)).into_response()
    }
}

pub async fn post(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Extension(addr): Extension<Address>, req: Request) -> Response {
    let headers = req.headers().clone();
    let posted = match content::checked(&user, req).await {
        Ok(p) => p,
        Err(o) => return o.into_response(),
    };
    let attachments = posted.files.iter().filter(|(input, _, _)| input == "files").map(|(_, name, bytes)| Attachment { filename: name.clone(), bytes: bytes.clone() }).collect();
    let ctx = Some(posted.one("ctx").trim().to_string()).filter(|c| !c.is_empty());
    match chat::start(app.clone(), &user, posted.one("text").to_string(), attachments, ctx) {
        Ok(()) => done(&addr, &headers),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

pub async fn new(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Extension(addr): Extension<Address>, req: Request) -> Response {
    let headers = req.headers().clone();
    if let Err(o) = content::checked(&user, req).await {
        return o.into_response();
    }
    chat::reset(&app, &user.token_hash);
    done(&addr, &headers)
}

pub async fn stop(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Extension(addr): Extension<Address>, req: Request) -> Response {
    let headers = req.headers().clone();
    if let Err(o) = content::checked(&user, req).await {
        return o.into_response();
    }
    chat::stop(&app, &user.token_hash);
    done(&addr, &headers)
}

// ---- the event stream ----

/// `"<conversation id>-<revision>"` as sent in an event's `id:`.
fn parse_seen(s: &str) -> Option<(u64, u64)> {
    let (a, b) = s.trim().split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// Server-sent events: one held-open stream per tab while a turn runs. It sends the conversation view at once
/// (unless the browser already has this revision) and again every time it changes, so streamed text arrives as
/// it is written. `since` (or `Last-Event-ID` after a reconnect) is the revision the browser has.
pub async fn events(State(app): State<Shared>, Extension(user): Extension<AdminUser>, headers: HeaderMap, Query(q): Query<Vec<(String, String)>>) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    let get = |k: &str| q.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    let seen = headers.get("last-event-id").and_then(|v| v.to_str().ok()).or_else(|| get("since")).and_then(parse_seen);
    let state = (app, user, seen);
    let stream = futures_util::stream::unfold(state, |(app, user, seen)| async move {
        loop {
            if let Some(seen) = seen {
                app.chat.wait_change(&user.token_hash, seen, Duration::from_secs(LONGEST_WAIT)).await;
            }
            let (id, rev, _) = app.chat.version(&user.token_hash);
            if seen == Some((id, rev)) {
                continue; // nothing new yet: keep waiting (the keep-alive comment holds the line open)
            }
            let (a, u) = (app.clone(), user.clone());
            let mut view = tokio::task::spawn_blocking(move || view(&a, &u)).await.unwrap_or(Value::Null);
            view["html"] = json!(log_html(&view));
            view["turns"] = Value::Null;
            let event = Event::default().id(view["id"].as_str().unwrap_or_default()).retry(Duration::from_millis(RETRY_MS as u64)).data(view.to_string());
            return Some((Ok::<_, std::convert::Infallible>(event), (app, user, Some((id, rev)))));
        }
    });
    let mut resp = Sse::new(stream).keep_alive(KeepAlive::default()).into_response();
    resp.headers_mut().insert(header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-store"));
    // nginx buffers proxied responses by default, which would hold streamed text back.
    resp.headers_mut().insert("x-accel-buffering", axum::http::HeaderValue::from_static("no"));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_files_map_to_pages_and_preview_paths_stay_on_the_site() {
        assert_eq!(page_of("index.html").as_deref(), Some("/"));
        assert_eq!(page_of("news/index.html").as_deref(), Some("/news/"));
        assert_eq!(page_of("about.html").as_deref(), Some("/about"));
        assert_eq!(page_of("style.css"), None);
        assert_eq!(page_of("404.html"), None);
        for bad in ["", "https://x.example/", "//x.example", "/admin/settings", "/a\"b", "/a b"] {
            assert_eq!(preview_path(bad), "/", "{bad}");
        }
        assert_eq!(preview_path("/news/2026-11?x"), "/");
        assert_eq!(preview_path("/news/a-b_c.html"), "/news/a-b_c.html");
        assert_eq!(parse_seen("3-17"), Some((3, 17)));
        assert_eq!(parse_seen("x"), None);
    }
}
