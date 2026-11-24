//! Settings: access (URLs and API keys), the chat model, skills, site notes, look, and accounts.
//! Keys, admin management, the chat model and skills write system tables directly (the only admin writes
//! outside `tools::call`); they need the builder role and CSRF. Site notes go through `set_site_notes`.

use super::admin::MARKS;
use super::content::{self, Out, Posted};
use super::session::{self, AdminUser};
use crate::auth::{self, Scope};
use crate::{llm, skills, App, Shared};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Extension;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

const NOTES_NOTE: &str = "后台修改站点说明";
const ENV_MODEL: &str = "对话用的模型由服务器的环境变量（LLM_PROVIDER、LLM_MODEL、LLM_API_KEY）设置，这里不能改：请改环境变量后重启。";
const NEW_URL_NEEDS_KEY: &str = "换了接口地址：请重新填一次密钥。保存过的密钥只会发给原来的地址。";
/// A skill zip upload: the 20 MB download cap plus room for the form.
pub const SKILL_BODY_LIMIT: usize = skills::MAX_DOWNLOAD + 1024 * 1024;
const LAST_BUILDER: &str = "至少要留一位搭建者（builder）：请先添加另一位搭建者，再删除这个账号。";

/// The chat model section: the environment's setting (read-only) or what a builder saved, the services to pick
/// from, and the models a detect found. The key is never read into the page, only whether one is saved.
fn model_section(app: &App, form: &ModelForm) -> Value {
    let provider = |p: llm::Provider| if p == llm::Provider::Anthropic { "Anthropic" } else { "兼容 OpenAI 的接口" };
    if let Some(env) = app.env_model.get() {
        return json!({ "env": { "provider": provider(env.provider), "model": env.model, "base_url": env.base_url, "key_set": env.api_key.is_some() } });
    }
    let saved = llm::load_saved(&app.conn()).ok().flatten();
    let preset = form.preset.clone().or_else(|| saved.as_ref().map(|(p, _)| p.clone())).unwrap_or_else(|| "anthropic".into());
    let base_url = form.base_url.clone().unwrap_or_else(|| saved.as_ref().filter(|(p, _)| *p == preset).map(|(_, s)| s.base_url.clone()).unwrap_or_default());
    let model = saved.as_ref().map(|(_, s)| s.model.clone()).unwrap_or_default();
    let chosen = form.models.as_ref().and_then(|m| llm::default_model(llm::preset(&preset).map_or(llm::Provider::OpenAi, |p| p.provider), m));
    json!({
        "presets": llm::PRESETS.iter().map(|p| json!({ "id": p.id, "name": p.name, "base_url": p.base_url, "on": p.id == preset })).collect::<Vec<_>>(),
        "preset": preset,
        "base_url": base_url,
        "model": model,
        "key_set": saved.as_ref().is_some_and(|(_, s)| s.api_key.is_some()),
        "saved": saved.is_some(),
        "models": form.models.as_ref().map(|list| list.iter().map(|m| json!({ "id": m.id, "name": m.name,
            "on": chosen.as_deref() == Some(m.id.as_str()) || (chosen.is_none() && m.id == model) })).collect::<Vec<_>>()),
    })
}

/// What a model form posted, for showing the page again with the admin's choice (never the key).
#[derive(Default)]
struct ModelForm {
    preset: Option<String>,
    base_url: Option<String>,
    models: Option<Vec<llm::ModelInfo>>,
}

fn skill_list(app: &App) -> Vec<Value> {
    skills::list(&app.conn())
        .unwrap_or_default()
        .into_iter()
        .map(|s| json!({ "name": s.name, "description": s.description, "source": s.source, "enabled": s.enabled,
            "installed": s.installed_at.chars().take(10).collect::<String>(), "files": s.files }))
        .collect()
}

/// The admin's own default chat size (the 外观 choice), not this session's.
fn chat_default(app: &App, user: &AdminUser) -> String {
    app.conn()
        .query_row("SELECT json_extract(prefs, '$.chat') FROM admins WHERE id = ?1", [user.id], |r| r.get::<_, Option<String>>(0))
        .ok()
        .flatten()
        .filter(|c| c == "floating")
        .unwrap_or_else(|| "docked".into())
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
    /// The model form as posted (a detect, or a refused save).
    model: ModelForm,
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
        "model": model_section(app, &extra.model),
        "skills": skill_list(app),
        "notes": extra.notes.unwrap_or_else(|| notes["text"].as_str().unwrap_or_default().to_string()),
        "notes_status": notes["status"],
        "can_notes": user.actor().can(Scope::Structure),
        "notes_note": NOTES_NOTE,
        "marks": MARKS.iter().map(|(k, label)| json!({ "key": k, "label": label, "on": nav["mark"] == *k })).collect::<Vec<_>>(),
        "chat_default": chat_default(app, user),
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
        return (StatusCode::FORBIDDEN, format!("“{}”是编辑：只有搭建者（builder）可以改这项设置。", user.email)).into_response();
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
    // The chat size also applies to this session at once; the next login starts with it.
    let saved = tokio::task::spawn_blocking(move || {
        let conn = app.conn();
        session::set_admin_pref(&conn, user.id, key, &value)?;
        if key == "chat" { session::set_pref(&conn, &user.token_hash, key, &value) } else { Ok(()) }
    })
        .await
        .unwrap_or_else(|e| Err(format!("服务器内部错误：{e}")));
    match saved {
        Ok(()) => Redirect::to("/admin/settings").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

// ---- the chat model ----

/// The form's service, address and key as settings for the model client. A saved key is only reused for the
/// address it was saved with, so a changed address cannot carry it elsewhere.
fn form_settings(app: &App, posted: &Posted) -> Result<(String, llm::Settings), String> {
    let id = posted.one("preset");
    let p = llm::preset(id).ok_or("不认识这个模型服务。")?;
    let base_url = Some(posted.one("base_url").trim()).filter(|u| !u.is_empty()).unwrap_or(p.base_url).trim_end_matches('/').to_string();
    let typed = Some(posted.one("key").trim()).filter(|k| !k.is_empty()).map(str::to_string);
    let saved = llm::load_saved(&app.conn())?.map(|(_, s)| s);
    let api_key = match (typed, saved) {
        (Some(k), _) => Some(k),
        (None, Some(s)) if s.api_key.is_some() && s.base_url != base_url => return Err(NEW_URL_NEEDS_KEY.into()),
        (None, s) => s.and_then(|s| s.api_key),
    };
    Ok((id.to_string(), llm::Settings { provider: p.provider, api_key, model: String::new(), base_url }))
}

fn posted_form(posted: &Posted) -> ModelForm {
    ModelForm { preset: Some(posted.one("preset").to_string()), base_url: Some(posted.one("base_url").trim().to_string()), models: None }
}

pub async fn save_model(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    handle(app, user, req, true, |app, user, posted| {
        let refuse = |error: String| settings_page(app, user, Extra { error: Some(error), model: posted_form(&posted), ..Default::default() });
        if app.env_model.get().is_some() {
            return refuse(ENV_MODEL.into());
        }
        let (preset, settings) = match form_settings(app, &posted) {
            Ok(s) => s,
            Err(e) => return refuse(e),
        };
        let model = Some(posted.one("model_name").trim()).filter(|m| !m.is_empty()).unwrap_or(posted.one("model"));
        let saved = llm::save(&app.conn(), &preset, &settings.base_url, model, settings.api_key.as_deref());
        match saved {
            Ok(s) => {
                app.set_model(s.client());
                Ok(Out::Go("/admin/settings?saved=model#s-model".into()))
            }
            Err(e) => refuse(e),
        }
    })
    .await
}

/// Asks the service which models the key can use and shows them to pick from. Spends the key, so builders only.
pub async fn detect_models(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    let posted = match content::checked(&user, req).await {
        Ok(p) => p,
        Err(o) => return o.into_response(),
    };
    if user.role != "builder" {
        return (StatusCode::FORBIDDEN, format!("“{}”是编辑：只有搭建者（builder）可以改这项设置。", user.email)).into_response();
    }
    let a = app.clone();
    let p = posted_form(&posted);
    let found = if app.env_model.get().is_some() {
        Err(ENV_MODEL.to_string())
    } else {
        match tokio::task::spawn_blocking(move || form_settings(&a, &posted)).await.unwrap_or_else(|e| Err(e.to_string())) {
            Ok((_, settings)) => llm::list_models(&settings).await,
            Err(e) => Err(e),
        }
    };
    let mut resp = content::blocking(move || match found {
        Ok(models) => settings_page(&app, &user, Extra { model: ModelForm { models: Some(models), ..p }, saved: "detected".into(), ..Default::default() }),
        Err(e) => settings_page(&app, &user, Extra { error: Some(e), model: p, ..Default::default() }),
    })
    .await;
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

// ---- skills ----

pub async fn install_skill(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    let posted = match content::checked(&user, req).await {
        Ok(p) => p,
        Err(o) => return o.into_response(),
    };
    if user.role != "builder" {
        return (StatusCode::FORBIDDEN, format!("“{}”是编辑：只有搭建者（builder）可以安装技能。", user.email)).into_response();
    }
    let upload = posted.files.iter().find(|(input, _, _)| input == "zip");
    let url = posted.one("url").trim().to_string();
    let got = match upload {
        Some((_, name, bytes)) => skills::files_from_zip(bytes, None).map(|f| (f, format!("上传的压缩包 {name}"))),
        None if url.is_empty() => Err("请填技能的链接，或者选一个压缩包上传。".into()),
        None => skills::download(&url).await.map(|f| (f, url.clone())),
    };
    content::blocking(move || {
        let installed = got.and_then(|(files, source)| skills::install(&app.conn(), files, &source, &user.email));
        match installed {
            Ok(_) => Ok(Out::Go("/admin/settings?saved=skill#s-skills".into())),
            Err(e) => refused(&app, &user, e),
        }
    })
    .await
}

pub async fn skill_action(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Path((name, action)): Path<(String, String)>, req: Request) -> Response {
    handle(app, user, req, true, move |app, user, posted| {
        let done = match action.as_str() {
            "enable" | "disable" => skills::set_enabled(&app.conn(), &name, action == "enable"),
            "delete" if posted.one("confirm") == "1" => skills::remove(&app.conn(), &name),
            "delete" => {
                let ctx = json!({
                    "heading": format!("删除技能“{name}”？"),
                    "message": "删除后对话就不再用它。要再用，得重新安装。",
                    "action": format!("/admin/settings/skills/{name}/delete"),
                    "back": "/admin/settings#s-skills",
                    "button": "确认删除",
                });
                return Ok(Out::Page(StatusCode::OK, "confirm.html", content::shell(app, user, "settings", ctx)));
            }
            _ => return Err(Out::Fail(StatusCode::NOT_FOUND, "没有这个操作。".into())),
        };
        match done {
            Ok(()) => Ok(Out::Go("/admin/settings#s-skills".into())),
            Err(e) => refused(app, user, e),
        }
    })
    .await
}
