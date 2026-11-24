//! Member accounts: the website's own visitors who sign up and log in. Separate from admins in every way:
//! their own table, sessions, cookie and rate limits. A member session never grants anything in the admin.

use crate::auth::{self, Actor};
use crate::changes::{self, Target};
use crate::schema::{CollectionDef, FieldDef, FieldType};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

pub const COOKIE: &str = "hc_member";
pub const MIN_PASSWORD: usize = 10;
const MAX_LOGIN_FAILURES: i64 = 5;
const MAX_SIGNUPS_PER_HOUR: i64 = 10;
pub const BAD_LOGIN: &str = "邮箱或密码不正确。";
pub const LOCKED: &str = "尝试次数太多，请 15 分钟后再试。";
/// The admin screen over members, and the name the built-in pseudo-collection goes by.
pub const MEMBERS: &str = "members";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub id: i64,
    pub email: String,
    pub name: String,
}

fn db(e: rusqlite::Error) -> String {
    format!("读取数据库失败：{e}")
}

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn random_hex() -> String {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    hex::encode(bytes)
}

/// Whether visitors can sign up and log in: live means published, `effective` also counts a pending draft (the
/// admin shows 用户管理 as soon as the chat turns login on).
pub fn login_enabled(conn: &Connection, effective: bool) -> bool {
    changes::setting(conn, "member_login", effective) == json!(true)
}

/// Turns member login on or off as a draft; it goes live with the next release.
pub fn set_login(conn: &Connection, actor: &Actor, enabled: bool, note: &str) -> Result<Value, String> {
    let target = Target::Setting("member_login".into());
    changes::save_draft(conn, actor, &target, &json!(enabled), note)?;
    Ok(json!({ "enabled": enabled, "status": changes::target_status(conn, &target)? }))
}

/// The shape the admin's 用户管理 screen and `set_view` see for members. Not a real collection: rows come from
/// the `members` table and the password hash is never part of it.
pub fn members_def() -> CollectionDef {
    let field = |name: &str, title: &str, kind: FieldType, options: &[&str]| FieldDef {
        name: name.into(),
        title: title.into(),
        kind,
        required: false,
        unique: false,
        index: true,
        multiple: false,
        default: None,
        options: options.iter().map(|s| s.to_string()).collect(),
        target: None,
    };
    let mut def: CollectionDef = serde_json::from_value(json!({ "name": MEMBERS, "title": "用户", "fields": [] })).expect("valid empty def");
    def.fields = vec![
        field("email", "邮箱", FieldType::Text, &[]),
        field("name", "名字", FieldType::Text, &[]),
        field("status", "状态", FieldType::Select, &["active", "disabled"]),
        field("created_at", "注册时间", FieldType::Date, &[]),
        field("last_login", "最近登录", FieldType::Date, &[]),
    ];
    def
}

fn check_email(email: &str) -> Result<String, String> {
    let e = auth::normalize_email(email);
    let (local, domain) = e.split_once('@').ok_or("邮箱格式不对。")?;
    if local.is_empty() || !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') || e.len() > 254 || e.contains(char::is_whitespace) {
        return Err("邮箱格式不对。".into());
    }
    Ok(e)
}

fn attempts(conn: &Connection, key: &str, window: &str) -> Result<i64, String> {
    conn.query_row(&format!("SELECT count(*) FROM member_attempts WHERE key = ?1 AND at > datetime('now', '{window}')"), [key], |r| r.get(0)).map_err(db)
}

fn note_attempt(conn: &Connection, keys: &[&str]) -> Result<(), String> {
    for k in keys {
        conn.execute("INSERT INTO member_attempts(key) VALUES (?1)", [k]).map_err(db)?;
    }
    Ok(())
}

fn open_session(conn: &Connection, member_id: i64) -> Result<String, String> {
    conn.execute("DELETE FROM member_sessions WHERE expires_at <= datetime('now')", []).map_err(db)?;
    let token = random_hex();
    conn.execute("INSERT INTO member_sessions(token_hash, member_id, expires_at) VALUES (?1, ?2, datetime('now', '+30 days'))", params![sha256_hex(&token), member_id])
        .map_err(db)?;
    conn.execute("UPDATE members SET last_login = datetime('now') WHERE id = ?1", [member_id]).map_err(db)?;
    Ok(token)
}

/// Creates a member and logs them in; returns the session token. `client` is the caller's address key.
pub fn signup(conn: &Connection, email: &str, password: &str, name: &str, client: &str) -> Result<String, String> {
    if password.chars().count() < MIN_PASSWORD {
        return Err(format!("密码至少 {MIN_PASSWORD} 个字符。"));
    }
    signup_hashed(conn, email, &auth::hash_password(password), name, client)
}

/// `signup` with the password already hashed, so the slow hashing happens without the database lock.
pub fn signup_hashed(conn: &Connection, email: &str, password_hash: &str, name: &str, client: &str) -> Result<String, String> {
    conn.execute("DELETE FROM member_attempts WHERE at <= datetime('now', '-1 hour')", []).map_err(db)?;
    let client_key = format!("signup:{client}");
    if attempts(conn, &client_key, "-1 hour")? >= MAX_SIGNUPS_PER_HOUR {
        return Err("注册太频繁，请过一会儿再试。".into());
    }
    let email = check_email(email)?;
    let name: String = name.trim().chars().take(80).collect();
    note_attempt(conn, &[&client_key])?;
    // ponytail: says the email is taken; a CMS sign-up form usually does, at the cost of revealing who registered.
    let taken: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM members WHERE email = ?1)", [&email], |r| r.get(0)).map_err(db)?;
    if taken {
        return Err("这个邮箱已经注册过了，请直接登录。".into());
    }
    conn.execute("INSERT INTO members(email, password_hash, name) VALUES (?1, ?2, ?3)", params![email, password_hash, name]).map_err(db)?;
    open_session(conn, conn.last_insert_rowid())
}

/// The stored hash a login checks against: (member id, hash, status) when the email is known. Fails when the email
/// or the client is locked out. Call `login_finish` with the result of verifying, outside the database lock.
pub fn login_begin(conn: &Connection, email: &str, client: &str) -> Result<Option<(i64, String, String)>, String> {
    conn.execute("DELETE FROM member_attempts WHERE at <= datetime('now', '-1 hour')", []).map_err(db)?;
    let email = auth::normalize_email(email);
    if attempts(conn, &format!("login:email:{email}"), "-15 minutes")? >= MAX_LOGIN_FAILURES || attempts(conn, &format!("login:ip:{client}"), "-15 minutes")? >= MAX_LOGIN_FAILURES {
        return Err(LOCKED.into());
    }
    conn.query_row("SELECT id, password_hash, status FROM members WHERE email = ?1", [&email], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()
        .map_err(db)
}

/// The hash to verify against when the email is unknown, so both failures take the same time.
pub fn dummy_hash() -> &'static str {
    static DUMMY: LazyLock<String> = LazyLock::new(|| auth::hash_password("cms-harness member dummy"));
    DUMMY.as_str()
}

/// Records the outcome of a login and opens the session when it passed. 5 failures per email or per client in
/// 15 minutes lock that key; unknown email and wrong password get the same message.
pub fn login_finish(conn: &Connection, email: &str, client: &str, found: Option<(i64, String, String)>, ok: bool) -> Result<String, String> {
    let email = auth::normalize_email(email);
    let (email_key, client_key) = (format!("login:email:{email}"), format!("login:ip:{client}"));
    match found {
        Some((id, _, status)) if ok && status == "active" => {
            conn.execute("DELETE FROM member_attempts WHERE key = ?1", [&email_key]).map_err(db)?;
            open_session(conn, id)
        }
        Some((_, _, status)) if ok && status != "active" => Err("这个账号已被停用。".into()),
        _ => {
            note_attempt(conn, &[&email_key, &client_key])?;
            Err(BAD_LOGIN.into())
        }
    }
}

/// `login_begin`, verify, `login_finish` in one go, for callers that are not on a request path.
pub fn login(conn: &Connection, email: &str, password: &str, client: &str) -> Result<String, String> {
    let found = login_begin(conn, email, client)?;
    let ok = auth::verify_password(found.as_ref().map_or(dummy_hash(), |f| f.1.as_str()), password);
    login_finish(conn, email, client, found, ok)
}

pub fn logout(conn: &Connection, token: &str) {
    let _ = conn.execute("DELETE FROM member_sessions WHERE token_hash = ?1", [sha256_hex(token)]);
}

/// The member behind a session token, if the session is live and the member active.
pub fn from_token(conn: &Connection, token: &str) -> Option<Member> {
    conn.query_row(
        "SELECT m.id, m.email, m.name FROM member_sessions s JOIN members m ON m.id = s.member_id
         WHERE s.token_hash = ?1 AND s.expires_at > datetime('now') AND m.status = 'active'",
        [sha256_hex(token)],
        |r| Ok(Member { id: r.get(0)?, email: r.get(1)?, name: r.get(2)? }),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn member_json(m: &Member) -> Value {
    json!({ "id": m.id, "email": m.email, "name": m.name })
}

/// Members for the admin and the `list_members` tool. Never includes password hashes.
pub fn list(conn: &Connection, status: Option<&str>, query: Option<&str>, limit: i64, offset: i64) -> Result<Value, String> {
    let status = status.filter(|s| !s.is_empty());
    if let Some(s) = status {
        if s != "active" && s != "disabled" {
            return Err("status 只能是 active 或 disabled。".into());
        }
    }
    let like = query.map(|q| format!("%{}%", q.trim().replace(['%', '_'], ""))).filter(|q| q != "%%");
    let limit = limit.clamp(1, 200);
    let filter = "(?1 IS NULL OR status = ?1) AND (?2 IS NULL OR email LIKE ?2 OR name LIKE ?2)";
    let total: i64 = conn.query_row(&format!("SELECT count(*) FROM members WHERE {filter}"), params![status, like], |r| r.get(0)).map_err(db)?;
    let mut st = conn
        .prepare(&format!("SELECT id, email, name, status, created_at, last_login FROM members WHERE {filter} ORDER BY id DESC LIMIT ?3 OFFSET ?4"))
        .map_err(db)?;
    let rows = st
        .query_map(params![status, like, limit, offset.max(0)], |r| {
            Ok(json!({ "id": r.get::<_, i64>(0)?, "email": r.get::<_, String>(1)?, "name": r.get::<_, String>(2)?, "status": r.get::<_, String>(3)?,
                "created_at": r.get::<_, String>(4)?, "last_login": r.get::<_, Option<String>>(5)? }))
        })
        .map_err(db)?;
    let items: Vec<Value> = rows.collect::<Result<_, _>>().map_err(db)?;
    Ok(json!({ "items": items, "total": total }))
}

/// Disables or re-enables a member; disabling ends their sessions at once.
pub fn set_status(conn: &Connection, id: i64, status: &str) -> Result<Value, String> {
    if status != "active" && status != "disabled" {
        return Err("status 只能是 active 或 disabled。".into());
    }
    match conn.execute("UPDATE members SET status = ?2 WHERE id = ?1", params![id, status]).map_err(db)? {
        0 => return Err(format!("没有编号为 {id} 的用户。")),
        _ if status == "disabled" => {
            conn.execute("DELETE FROM member_sessions WHERE member_id = ?1", [id]).map_err(db)?;
        }
        _ => {}
    }
    Ok(json!({ "id": id, "status": status }))
}
