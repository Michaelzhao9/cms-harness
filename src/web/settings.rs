//! Settings: access (URLs and API keys), the chat model, site notes, look, and accounts.
//! Keys and admin management write system tables directly (the only admin writes outside `tools::call`);
//! they need the builder role and CSRF. Site notes go through `set_site_notes`.

use super::admin::MARKS;
use super::content::{self, Out, Posted};
use super::session::{self, AdminUser};
use crate::auth::{self, Scope};
use crate::{App, Shared};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Extension;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

const NOTES_NOTE: &str = "后台修改站点说明";
const LAST_BUILDER: &str = "至少要留一位搭建者（builder）：请先添加另一位搭建者，再删除这个账号。";

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| default.into())
}

/// The chat model as the settings page shows it. Task 7's app settings replace this; the API key is only
/// checked for presence, never read into the page.
fn model() -> Value {
    json!({
        "provider": env_or("LLM_PROVIDER", "anthropic"),
        "model": env_or("LLM_MODEL", "claude-opus-5-5"),
        "key_set": std::env::var_os("LLM_API_KEY").is_some_and(|v| !v.is_empty()),
    })
}

fn scope_label(s: Scope) -> &'static str {
    match s {
        Scope::Read => "读",
        Scope::Write => "写",
        Scope::Structure => "改结构",
        Scope::Publish => "发布",
    }
}

fn role_label(role: &str) -> &'static str {
    match role {
        "builder" => "搭建者，可以改结构和发布",
        _ => "编辑，可以改内容和发布",
    }
}

fn db(e: rusqlite::Error) -> Out {
    content::internal(format!("读取数据库失败：{e}"))
}

/// Things the page shows besides what it always reads.
#[derive(Default)]
struct Extra {
    error: Option<String>,
    /// (name, key): a key just created, shown this once.
    new_key: Option<(String, String)>,
    /// (email, password): a password just set, shown this once.
    revealed: Option<(String, String)>,
    /// Site notes as typed into a refused form.
    notes: Option<String>,
    saved: String,
}

fn settings_page(app: &App, user: &AdminUser, extra: Extra) -> Result<Out, Out> {
    let builder = user.role == "builder";
    let (keys, admins) = {
        let conn = app.conn();
        let mut stmt = conn.prepare("SELECT id, name, scopes, created_at, revoked FROM api_keys ORDER BY id DESC").map_err(db)?;
        let keys: Vec<Value> = stmt
            .query_map([], |r| {
                let scopes: Vec<Scope> = serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default();
                // Read is always included.
                let mut labels = vec![scope_label(Scope::Read)];
                labels.extend(scopes.into_iter().filter(|s| *s != Scope::Read).map(scope_label));
                Ok(json!({ "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "scopes": labels.join("、"),
                    "created": r.get::<_, String>(3)?.chars().take(10).collect::<String>(), "revoked": r.get::<_, bool>(4)? }))
            })
            .map_err(db)?
            .collect::<rusqlite::Result<_>>()
            .map_err(db)?;
        let mut stmt = conn.prepare("SELECT id, email, role FROM admins ORDER BY id").map_err(db)?;
        let admins: Vec<Value> = stmt
            .query_map([], |r| {
                let id: i64 = r.get(0)?;
                let role: String = r.get(2)?;
                Ok(json!({ "id": id, "email": r.get::<_, String>(1)?, "role": if role == "builder" { "搭建者" } else { "编辑" }, "me": id == user.id }))
            })
            .map_err(db)?
            .collect::<rusqlite::Result<_>>()
            .map_err(db)?;
        (keys, admins)
    };
    let notes = content::tool(app, user, "get_site_notes", json!({})).map_err(content::internal)?;
    let nav = super::admin::nav(&app.conn(), user, "settings");
    let status = if extra.error.is_some() { StatusCode::BAD_REQUEST } else { StatusCode::OK };
    let ctx = json!({
        "mcp_url": format!("{}/mcp", app.origin),
        "api_url": format!("{}/api/v1/content/", app.origin),
        "keys": keys,
        "builder": builder,
        "new_key": extra.new_key.map(|(name, key)| json!({ "name": name, "key": key })),
        "model": model(),
        "notes": extra.notes.unwrap_or_else(|| notes["text"].as_str().unwrap_or_default().to_string()),
        "notes_status": notes["status"],
        "can_notes": user.actor().can(Scope::Structure),
        "notes_note": NOTES_NOTE,
        "marks": MARKS.iter().map(|(k, label)| json!({ "key": k, "label": label, "on": nav["mark"] == *k })).collect::<Vec<_>>(),
        "chat": nav["chat"],
        "email": user.email,
        "role": role_label(&user.role),
        "admins": if builder { admins } else { vec![] },
        "revealed": extra.revealed.map(|(email, password)| json!({ "email": email, "password": password })),
        "error": extra.error,
        "saved": extra.saved,
    });
    Ok(Out::Page(status, "settings.html", content::shell(app, user, "settings", ctx)))
}

fn refused(app: &App, user: &AdminUser, error: String) -> Result<Out, Out> {
    settings_page(app, user, Extra { error: Some(error), ..Default::default() })
}

pub async fn page(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Query(q): Query<Vec<(String, String)>>) -> Response {
    let saved = q.into_iter().find(|(k, _)| k == "saved").map(|(_, v)| v).unwrap_or_default();
    content::blocking(move || settings_page(&app, &user, Extra { saved, ..Default::default() })).await
}

/// Reads and checks the form, then the role, then runs `f`. Every answer is `no-store`: some carry a secret.
async fn handle(app: Shared, user: AdminUser, req: Request, builder_only: bool, f: impl FnOnce(&App, &AdminUser, Posted) -> Result<Out, Out> + Send + 'static) -> Response {
    let posted = match content::checked(&user, req).await {
        Ok(p) => p,
        Err(o) => return o.into_response(),
    };
    if builder_only && user.role != "builder" {
        return (StatusCode::FORBIDDEN, format!("“{}”是编辑：只有搭建者（builder）可以管理密钥和管理员。", user.email)).into_response();
    }
    let mut resp = content::blocking(move || f(&app, &user, posted)).await;
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

pub async fn create_key(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    handle(app, user, req, true, |app, user, posted| {
        let name = posted.one("name").trim().to_string();
        if name.is_empty() {
            return refused(app, user, "密钥名字不能为空：写一个能认出用途的名字，比如“我的 Claude Code”。".into());
        }
        if name.chars().count() > 60 {
            return refused(app, user, "密钥名字太长：最多 60 个字符。".into());
        }
        let mut scopes = vec![Scope::Read];
        for s in posted.fields.get("scope").into_iter().flatten() {
            match Scope::parse(s) {
                Some(Scope::Read) => {}
                Some(scope) if !scopes.contains(&scope) => scopes.push(scope),
                Some(_) => {}
                None => return refused(app, user, format!("不认识的权限“{s}”：可选 write、structure、publish（read 总是包含）。")),
            }
        }
        let made = auth::create_key(&app.conn(), &name, &scopes);
        match made {
            Ok(key) => settings_page(app, user, Extra { new_key: Some((name, key)), ..Default::default() }),
            Err(e) => refused(app, user, e),
        }
    })
    .await
}

pub async fn revoke_key(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Path(id): Path<i64>, req: Request) -> Response {
    handle(app, user, req, true, move |app, user, _| {
        let done = auth::revoke_key(&app.conn(), id);
        match done {
            Ok(()) => Ok(Out::Go("/admin/settings".into())),
            Err(e) => refused(app, user, e),
        }
    })
    .await
}

pub async fn save_notes(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    handle(app, user, req, false, |app, user, posted| {
        let text = posted.one("text").replace("\r\n", "\n");
        match content::tool(app, user, "set_site_notes", json!({ "text": text, "note": posted.note(NOTES_NOTE) })) {
            Ok(_) => Ok(Out::Go("/admin/settings?saved=notes".into())),
            Err(e) => settings_page(app, user, Extra { error: Some(e), notes: Some(text), ..Default::default() }),
        }
    })
    .await
}

pub async fn change_password(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    handle(app, user, req, false, |app, user, posted| {
        let done = session::change_password(&app.conn(), user.id, posted.one("old"), posted.one("new"), &user.token_hash);
        match done {
            Ok(()) => Ok(Out::Go("/admin/settings?saved=password".into())),
            Err(e) => refused(app, user, e),
        }
    })
    .await
}

pub async fn add_admin(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    handle(app, user, req, true, |app, user, posted| {
        let email = auth::normalize_email(posted.one("email"));
        let role = posted.one("role");
        // `create_admin` resets an existing admin (password and role): adding must never do that.
        let made = {
            let conn = app.conn();
            let exists = conn.query_row("SELECT 1 FROM admins WHERE lower(email) = ?1", [&email], |_| Ok(())).optional().map_err(db)?.is_some();
            if exists {
                Err(format!("“{email}”已经是管理员了：要给他新密码，请用列表里的“重设密码”。"))
            } else {
                auth::create_admin(&conn, &email, role, None)
            }
        };
        match made {
            Ok(password) => settings_page(app, user, Extra { revealed: Some((email, password)), ..Default::default() }),
            Err(e) => refused(app, user, e),
        }
    })
    .await
}

pub async fn reset_admin(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Path(id): Path<i64>, req: Request) -> Response {
    handle(app, user, req, true, move |app, user, _| {
        if id == user.id {
            return refused(app, user, "要改自己的密码，请用上面的“修改密码”，它需要原密码。".into());
        }
        let done = {
            let conn = app.conn();
            match conn.query_row("SELECT email, role FROM admins WHERE id = ?1", [id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).optional().map_err(db)? {
                // The role is the stored one, never the form's.
                Some((email, role)) => auth::create_admin(&conn, &email, &role, None).map(|p| (email, p)),
                None => Err(format!("管理员 {id} 不存在：请刷新页面。")),
            }
        };
        match done {
            Ok(revealed) => settings_page(app, user, Extra { revealed: Some(revealed), ..Default::default() }),
            Err(e) => refused(app, user, e),
        }
    })
    .await
}

pub async fn remove_admin(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Path(id): Path<i64>, req: Request) -> Response {
    let secure = app.secure_cookies;
    let mut resp = handle(app, user, req, true, move |app, user, _| {
        let removed = {
            let conn = app.conn();
            // One statement, so two removals at once cannot both pass the count.
            let n = conn
                .execute("DELETE FROM admins WHERE id = ?1 AND (role != 'builder' OR (SELECT count(*) FROM admins WHERE role = 'builder') > 1)", [id])
                .map_err(db)?;
            let exists = conn.query_row("SELECT 1 FROM admins WHERE id = ?1", [id], |_| Ok(())).optional().map_err(db)?.is_some();
            match (n, exists) {
                (0, true) => Err(LAST_BUILDER.to_string()),
                (0, false) => Err(format!("管理员 {id} 不存在：请刷新页面。")),
                _ => Ok(()),
            }
        };
        match removed {
            // Their sessions went with them (ON DELETE CASCADE), yours too if it was you.
            Ok(()) if id == user.id => Ok(Out::Go("/admin/login".into())),
            Ok(()) => Ok(Out::Go("/admin/settings".into())),
            Err(e) => refused(app, user, e),
        }
    })
    .await;
    if resp.headers().get(header::LOCATION).is_some_and(|l| l == "/admin/login") {
        if let Ok(v) = HeaderValue::from_str(&session::clear_cookie(secure)) {
            resp.headers_mut().insert(header::SET_COOKIE, v);
        }
    }
    resp
}

pub async fn appearance(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    let posted = match content::checked(&user, req).await {
        Ok(p) => p,
        Err(o) => return o.into_response(),
    };
    let (key, value) = match (posted.one("mark"), posted.one("chat")) {
        (m, "") if MARKS.iter().any(|(k, _)| *k == m) => ("mark", m.to_string()),
        ("", c @ ("docked" | "floating")) => ("chat", c.to_string()),
        _ => return (StatusCode::BAD_REQUEST, "只能选列出的标记颜色（yellow、blue、green、coral、purple）或对话样子（docked、floating）。").into_response(),
    };
    let saved = tokio::task::spawn_blocking(move || session::set_admin_pref(&app.conn(), user.id, key, &value))
        .await
        .unwrap_or_else(|e| Err(format!("服务器内部错误：{e}")));
    match saved {
        Ok(()) => Redirect::to("/admin/settings").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}
