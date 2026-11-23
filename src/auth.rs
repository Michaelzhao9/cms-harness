use argon2::password_hash::{rand_core::OsRng as SaltRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::{rngs::OsRng, Rng, RngCore};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Write,
    Structure,
    Publish,
}

impl Scope {
    pub fn parse(s: &str) -> Option<Scope> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }
}

#[derive(Debug, Clone)]
pub struct Actor {
    pub name: String,
    pub scopes: Vec<Scope>,
    pub admin: bool,
}

impl Actor {
    pub fn can(&self, scope: Scope) -> bool {
        scope == Scope::Read || self.scopes.contains(&scope)
    }
}

pub fn role_scopes(role: &str) -> Vec<Scope> {
    match role {
        "builder" => vec![Scope::Read, Scope::Write, Scope::Structure, Scope::Publish],
        "editor" => vec![Scope::Read, Scope::Write, Scope::Publish],
        _ => vec![],
    }
}

pub fn hash_password(password: &str) -> String {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut SaltRng))
        .expect("argon2 hashing with default params cannot fail")
        .to_string()
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

pub fn create_admin(conn: &Connection, email: &str, role: &str, password: Option<&str>) -> Result<String, String> {
    if email.trim().is_empty() {
        return Err("邮箱不能为空，请提供管理员邮箱。".into());
    }
    if role_scopes(role).is_empty() {
        return Err(format!("未知角色「{role}」，角色只能是 builder 或 editor。"));
    }
    let password = match password {
        Some(p) => p.to_string(),
        None => {
            const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
            (0..20).map(|_| CHARS[OsRng.gen_range(0..CHARS.len())] as char).collect()
        }
    };
    conn.execute(
        "INSERT INTO admins(email, password_hash, role) VALUES(?1, ?2, ?3)
         ON CONFLICT(email) DO UPDATE SET password_hash = excluded.password_hash, role = excluded.role",
        params![email, hash_password(&password), role],
    )
    .map_err(|e| format!("保存管理员「{email}」失败：{e}"))?;
    Ok(password)
}

fn key_hash(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

pub fn create_key(conn: &Connection, name: &str, scopes: &[Scope]) -> Result<String, String> {
    let mut bytes = [0u8; 20];
    OsRng.fill_bytes(&mut bytes);
    let key = format!("hc_{}", hex::encode(bytes));
    let scopes = serde_json::to_string(scopes).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO api_keys(name, key_hash, scopes) VALUES(?1, ?2, ?3)",
        params![name, key_hash(&key), scopes],
    )
    .map_err(|e| format!("创建密钥「{name}」失败：{e}"))?;
    Ok(key)
}

pub fn actor_from_key(conn: &Connection, bearer: &str) -> Option<Actor> {
    let (name, scopes): (String, String) = conn
        .query_row(
            "SELECT name, scopes FROM api_keys WHERE key_hash = ?1 AND revoked = 0",
            [key_hash(bearer)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?;
    Some(Actor { name: format!("key:{name}"), scopes: serde_json::from_str(&scopes).ok()?, admin: false })
}

pub fn revoke_key(conn: &Connection, id: i64) -> Result<(), String> {
    match conn.execute("UPDATE api_keys SET revoked = 1 WHERE id = ?1", [id]) {
        Ok(0) => Err(format!("密钥 {id} 不存在，请检查密钥编号。")),
        Ok(_) => Ok(()),
        Err(e) => Err(format!("吊销密钥 {id} 失败：{e}")),
    }
}
