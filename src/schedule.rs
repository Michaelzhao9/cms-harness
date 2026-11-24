//! Publishing at a set time. A schedule remembers who set it, which changes (or all), and the note; when its
//! time comes the server runs the ordinary publish as that person, so the same checks, history and release
//! record apply. Times are UTC, `YYYY-MM-DD HH:MM:SS`.

use crate::auth::{Actor, Scope};
use crate::changes::Target;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS schedules(id INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT NOT NULL, targets TEXT, note TEXT NOT NULL,
  actor TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'waiting' CHECK(status IN ('waiting','done','failed','cancelled')),
  result TEXT, release_id INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')));
CREATE INDEX IF NOT EXISTS schedules_due ON schedules(status, at);";

fn db(e: rusqlite::Error) -> String {
    format!("读写定时发布失败：{e}")
}

/// `at` as UTC `YYYY-MM-DD HH:MM:SS`, given in a zone `offset_minutes` east of UTC (480 for China). It must be
/// in the future and within a year.
pub fn normalize(conn: &Connection, at: &str, offset_minutes: i64) -> Result<String, String> {
    let at = at.trim().replace('T', " ");
    let (utc, future, near): (Option<String>, bool, bool) = conn
        .query_row(
            "SELECT datetime(?1, ?2), datetime(?1, ?2) > datetime('now'), datetime(?1, ?2) < datetime('now', '+366 days')",
            params![at, format!("{} minutes", -offset_minutes)],
            |r| Ok((r.get(0)?, r.get::<_, Option<bool>>(1)?.unwrap_or(false), r.get::<_, Option<bool>>(2)?.unwrap_or(false))),
        )
        .map_err(db)?;
    let utc = utc.ok_or_else(|| format!("时间“{at}”看不懂：请写成 2026-12-01 09:00 这样。"))?;
    if !future {
        return Err("定时发布的时间要在将来。".into());
    }
    if !near {
        return Err("定时发布最远排到一年以后。".into());
    }
    Ok(utc)
}

/// Schedules a publish of `targets` (None: everything pending at that time) at UTC `at`.
pub fn create(conn: &Connection, actor: &Actor, at: &str, targets: Option<Vec<String>>, note: &str) -> Result<i64, String> {
    if !actor.can(Scope::Publish) {
        return Err(format!("“{}”没有发布权限，不能定时发布。", actor.name));
    }
    if let Some(list) = &targets {
        if list.is_empty() {
            return Err("没有选要定时发布的改动。".into());
        }
        for t in list {
            Target::parse(t)?;
        }
    }
    let targets = targets.map(|t| json!(t).to_string());
    conn.execute("INSERT INTO schedules(at, targets, note, actor) VALUES (?1, ?2, ?3, ?4)", params![at, targets, note, actor.name]).map_err(db)?;
    Ok(conn.last_insert_rowid())
}

/// Waiting schedules, soonest first, then the 20 most recent that ran or were cancelled.
pub fn list(conn: &Connection) -> Result<Vec<Value>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, at, targets, note, actor, status, result, release_id FROM schedules
             WHERE status = 'waiting' OR id IN (SELECT id FROM schedules WHERE status != 'waiting' ORDER BY id DESC LIMIT 20)
             ORDER BY status != 'waiting', CASE WHEN status = 'waiting' THEN at END, id DESC",
        )
        .map_err(db)?;
    let rows = stmt
        .query_map([], |r| {
            let targets: Option<String> = r.get(2)?;
            Ok(json!({
                "id": r.get::<_, i64>(0)?, "at": r.get::<_, String>(1)?,
                "targets": targets.and_then(|t| serde_json::from_str::<Value>(&t).ok()),
                "note": r.get::<_, String>(3)?, "actor": r.get::<_, String>(4)?, "status": r.get::<_, String>(5)?,
                "result": r.get::<_, Option<String>>(6)?, "release_id": r.get::<_, Option<i64>>(7)?,
            }))
        })
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// Cancels a schedule that has not run.
pub fn cancel(conn: &Connection, id: i64) -> Result<(), String> {
    match conn.execute("UPDATE schedules SET status = 'cancelled' WHERE id = ?1 AND status = 'waiting'", [id]).map_err(db)? {
        0 => Err(format!("没有等待中的定时发布 {id}：它可能已经发布或取消了。")),
        _ => Ok(()),
    }
}

/// The person a schedule acts as, with the permissions they have now: an admin by email, or a key by name.
fn actor_now(conn: &Connection, name: &str) -> Option<Actor> {
    if let Some(key) = name.strip_prefix("key:") {
        let scopes: String = conn.query_row("SELECT scopes FROM api_keys WHERE name = ?1 AND revoked = 0 ORDER BY id DESC LIMIT 1", [key], |r| r.get(0)).optional().ok()??;
        return Some(Actor { name: name.to_string(), scopes: serde_json::from_str(&scopes).ok()?, admin: false });
    }
    let role: String = conn.query_row("SELECT role FROM admins WHERE email = ?1", [name], |r| r.get(0)).optional().ok()??;
    Some(Actor { name: name.to_string(), scopes: crate::auth::role_scopes(&role), admin: true })
}

/// Runs every waiting schedule whose time has come, oldest first. Returns how many ran.
pub fn run_due(conn: &Connection) -> Result<usize, String> {
    let due: Vec<(i64, Option<String>, String, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, targets, note, actor FROM schedules WHERE status = 'waiting' AND at <= datetime('now') ORDER BY at, id")
            .map_err(db)?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(db)?;
        rows.collect::<Result<_, _>>().map_err(db)?
    };
    for (id, targets, note, actor) in &due {
        let outcome = (|| -> Result<i64, String> {
            let who = actor_now(conn, actor).filter(|a| a.can(Scope::Publish)).ok_or_else(|| format!("“{actor}”已经不在了或没有发布权限了。"))?;
            let list = match targets {
                Some(t) => Some(
                    serde_json::from_str::<Vec<String>>(t)
                        .map_err(|e| e.to_string())?
                        .iter()
                        .map(|t| Target::parse(t))
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                None => None,
            };
            Ok(crate::changes::publish(conn, &who, list.as_deref(), note)?.id)
        })();
        let (status, result, release) = match outcome {
            Ok(rid) => ("done", None, Some(rid)),
            Err(e) => ("failed", Some(e), None),
        };
        conn.execute("UPDATE schedules SET status = ?2, result = ?3, release_id = ?4 WHERE id = ?1", params![id, status, result, release]).map_err(db)?;
    }
    Ok(due.len())
}
