//! Admin sessions, login rate limiting and CSRF tokens.

use crate::auth::{self, Actor};
use axum::http::{header, HeaderMap, StatusCode};
use rand::{rngs::OsRng, RngCore};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

pub const COOKIE: &str = "hc_admin";
const MAX_AGE: i64 = 30 * 24 * 3600;
const MAX_FAILURES: i64 = 5;

pub const BAD_LOGIN: &str = "邮箱或密码不正确，请检查后重试。";
pub const LOCKED: &str = "登录失败次数过多，请等 15 分钟后再试。";

#[derive(Debug, Clone)]
pub struct AdminUser {
    pub id: i64,
    pub email: String,
    pub role: String,
    pub csrf: String,
    pub token_hash: String,
    /// The admin's own preferences (`mark`, `chat`) with this session's (`nav`) laid over them.
    pub prefs: Value,
}

impl AdminUser {
    pub fn actor(&self) -> Actor {
        Actor { name: self.email.clone(), scopes: auth::role_scopes(&self.role), admin: true }
    }
}

fn random_hex() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Compares in time that depends only on the lengths.
fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn failures(conn: &Connection, key: &str) -> rusqlite::Result<i64> {
    conn.query_row("SELECT count(*) FROM login_attempts WHERE key = ?1 AND at > datetime('now', '-15 minutes')", [key], |r| r.get(0))
}

/// The admin a login checks against: (id, hash) when the email is known. Fails with `LOCKED` when the email or the
/// client is rate limited. The caller verifies outside the database lock, then calls `login_finish`.
pub fn login_begin(conn: &Connection, email: &str, client_key: &str) -> Result<Option<(i64, String)>, String> {
    let db = |e: rusqlite::Error| format!("读取数据库失败：{e}");
    let email = auth::normalize_email(email);
    conn.execute("DELETE FROM login_attempts WHERE at <= datetime('now', '-15 minutes')", []).map_err(db)?;
    if failures(conn, &format!("email:{email}")).map_err(db)? >= MAX_FAILURES || failures(conn, &format!("ip:{client_key}")).map_err(db)? >= MAX_FAILURES {
        return Err(LOCKED.into());
    }
    conn.query_row("SELECT id, password_hash FROM admins WHERE lower(email) = ?1 ORDER BY id LIMIT 1", [&email], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(db)
}

/// Verified when the email is unknown, so both failures take the same time.
pub fn dummy_hash() -> &'static str {
    static DUMMY: LazyLock<String> = LazyLock::new(|| auth::hash_password("cms-harness dummy password"));
    DUMMY.as_str()
}

/// Records the outcome and opens a session when the password matched. Returns the cookie token; only its hash is
/// stored. The error is `BAD_LOGIN` for both unknown emails and wrong passwords.
pub fn login_finish(conn: &Connection, email: &str, client_key: &str, found: Option<(i64, String)>, ok: bool) -> Result<String, String> {
    let db = |e: rusqlite::Error| format!("读取数据库失败：{e}");
    let email = auth::normalize_email(email);
    let (email_key, client_key) = (format!("email:{email}"), format!("ip:{client_key}"));
    let Some((admin_id, _)) = found.filter(|_| ok) else {
        conn.execute("INSERT INTO login_attempts(key) VALUES(?1), (?2)", params![email_key, client_key]).map_err(db)?;
        return Err(BAD_LOGIN.into());
    };
    conn.execute("DELETE FROM login_attempts WHERE key = ?1", [&email_key]).map_err(db)?;
    conn.execute("DELETE FROM admin_sessions WHERE expires_at <= datetime('now')", []).map_err(db)?;
    let token = random_hex();
    conn.execute(
        "INSERT INTO admin_sessions(token_hash, admin_id, csrf, expires_at) VALUES(?1, ?2, ?3, datetime('now', '+30 days'))",
        params![sha256_hex(&token), admin_id, random_hex()],
    )
    .map_err(db)?;
    Ok(token)
}

/// `login_begin`, verify, `login_finish` in one go (tests and the CLI; request handlers split it).
pub fn login(conn: &Connection, email: &str, password: &str, client_key: &str) -> Result<String, String> {
    let found = login_begin(conn, email, client_key)?;
    let ok = auth::verify_password(found.as_ref().map_or(dummy_hash(), |f| f.1.as_str()), password);
    login_finish(conn, email, client_key, found, ok)
}

pub fn logout(conn: &Connection, token_hash: &str) {
    let _ = conn.execute("DELETE FROM admin_sessions WHERE token_hash = ?1", [token_hash]);
}

/// The value of the cookie `name`, from any `Cookie` header.
/// Every value sent for the cookie `name`, in order. A site on a parent domain can plant a same-named cookie with a
/// longer path that the browser sends first, so callers that authenticate try them all.
pub fn cookie_values(headers: &HeaderMap, name: &str) -> Vec<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().strip_prefix(name)?.strip_prefix('=').map(str::to_string))
        .filter(|t| !t.is_empty())
        .collect()
}

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(name)?.strip_prefix('=').map(str::to_string))
        .filter(|t| !t.is_empty())
}

/// The logged-in admin, or `None`. An expired session is deleted when seen.
pub fn from_cookie(conn: &Connection, headers: &HeaderMap) -> Option<AdminUser> {
    cookie_values(headers, COOKIE).iter().find_map(|t| from_token(conn, t))
}

fn from_token(conn: &Connection, token: &str) -> Option<AdminUser> {
    let token_hash = sha256_hex(token);
    let (user, live) = conn
        .query_row(
            "SELECT a.id, a.email, a.role, s.csrf, json_patch(a.prefs, s.prefs), s.expires_at > datetime('now')
             FROM admin_sessions s JOIN admins a ON a.id = s.admin_id WHERE s.token_hash = ?1",
            [&token_hash],
            |r| {
                let prefs: String = r.get(4)?;
                let user = AdminUser {
                    id: r.get(0)?,
                    email: r.get(1)?,
                    role: r.get(2)?,
                    csrf: r.get(3)?,
                    token_hash: token_hash.clone(),
                    prefs: serde_json::from_str(&prefs).unwrap_or_else(|_| Value::Object(Default::default())),
                };
                Ok((user, r.get::<_, bool>(5)?))
            },
        )
        .ok()?;
    if !live {
        logout(conn, &token_hash);
        return None;
    }
    Some(user)
}

// ---- the site preview's tokens ----
//
// The admin frames the preview in a sandbox without `allow-same-origin`: everything the frame asks for comes from
// an opaque origin and carries no cookie. So the preview lives under `/_hc/preview/<token>/…` on the admin's
// address, and the token in the path is the whole permission: reading drafts, nothing else. Each workbench page
// gets a fresh one. It dies with its session (logout, password change) and after an hour without use.

/// A new preview token for the session with this token hash.
pub fn issue_preview(conn: &Connection, token_hash: &str) -> Result<String, String> {
    let db = |e: rusqlite::Error| format!("保存预览链接失败：{e}");
    conn.execute("DELETE FROM preview_tokens WHERE expires_at <= datetime('now')", []).map_err(db)?;
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    conn.execute(
        "INSERT INTO preview_tokens(token_hash, session, expires_at, created_at) VALUES(?1, ?2, datetime('now', '+1 hour'), datetime('now'))",
        params![sha256_hex(&token), token_hash],
    )
        .map_err(db)?;
    Ok(token)
}

/// True when the token is live (and its session too); each use keeps it alive for another hour, at most 8 hours
/// after it was issued: a draft script that reads the token out of its URL cannot keep it alive for weeks.
pub fn use_preview(conn: &Connection, token: &str) -> bool {
    let ok = token.len() == 32 && token.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    ok && conn
        .execute(
            "UPDATE preview_tokens SET expires_at = min(datetime('now', '+1 hour'), datetime(coalesce(created_at, expires_at), '+8 hours'))
             WHERE token_hash = ?1 AND expires_at > datetime('now') AND coalesce(created_at, expires_at) > datetime('now', '-8 hours')
             AND session IN (SELECT token_hash FROM admin_sessions WHERE expires_at > datetime('now'))",
            [sha256_hex(token)],
        )
        .is_ok_and(|n| n == 1)
}

/// Stores one string preference on the admin account, kept across sessions.
pub fn set_admin_pref(conn: &Connection, admin_id: i64, key: &str, value: &str) -> Result<(), String> {
    conn.execute("UPDATE admins SET prefs = json_set(prefs, ?1, ?2) WHERE id = ?3", params![format!("$.{key}"), value, admin_id])
        .map(drop)
        .map_err(|e| format!("保存偏好失败：{e}"))
}

/// Stores one string preference in the session's `prefs`.
pub fn set_pref(conn: &Connection, token_hash: &str, key: &str, value: &str) -> Result<(), String> {
    conn.execute("UPDATE admin_sessions SET prefs = json_set(prefs, ?1, ?2) WHERE token_hash = ?3", params![format!("$.{key}"), value, token_hash])
        .map(drop)
        .map_err(|e| format!("保存偏好失败：{e}"))
}

pub fn check_csrf(user: &AdminUser, form_token: &str) -> Result<(), StatusCode> {
    if constant_eq(user.csrf.as_bytes(), form_token.as_bytes()) {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

/// Sets a new password and ends every other session of this admin.
pub fn change_password(conn: &Connection, admin_id: i64, old: &str, new: &str, keep_token_hash: &str) -> Result<(), String> {
    let db = |e: rusqlite::Error| format!("读取数据库失败：{e}");
    let hash: String = conn
        .query_row("SELECT password_hash FROM admins WHERE id = ?1", [admin_id], |r| r.get(0))
        .optional()
        .map_err(db)?
        .ok_or("这个管理员账号已不存在，请重新登录。")?;
    if !auth::verify_password(&hash, old) {
        return Err("原密码不正确，请重新输入。".into());
    }
    if new.chars().count() < 8 {
        return Err("新密码至少要 8 个字符，请换一个更长的密码。".into());
    }
    conn.execute("UPDATE admins SET password_hash = ?1 WHERE id = ?2", params![auth::hash_password(new), admin_id]).map_err(db)?;
    conn.execute("DELETE FROM admin_sessions WHERE admin_id = ?1 AND token_hash != ?2", params![admin_id, keep_token_hash]).map_err(db)?;
    Ok(())
}

/// The `Set-Cookie` value for a new session.
pub fn session_cookie(token: &str, secure: bool) -> String {
    format!("{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={MAX_AGE}{}", if secure { "; Secure" } else { "" })
}

/// The `Set-Cookie` value that removes the session cookie.
pub fn clear_cookie(secure: bool) -> String {
    format!("{COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{}", if secure { "; Secure" } else { "" })
}
