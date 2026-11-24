//! Who may read a collection's live records and who may add to it from the site. Records a visitor or member
//! adds go live at once (a form submission, a comment, an order), stamped with their owner.

use crate::changes;
use crate::schema::{fill_defaults, normalize, Access, CollectionDef, Create, FieldType, Read};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Viewer {
    /// A valid API key: sees everything that is live.
    Key,
    /// A logged-in member, by id.
    Member(i64),
    Anon,
}

/// What `viewer` may read: None when nothing; Some(None) for every live record; Some(Some(id)) for the records
/// member `id` owns.
pub fn read_filter(access: &Access, viewer: Viewer) -> Option<Option<i64>> {
    match (access.read, viewer) {
        (_, Viewer::Key) | (Read::Public, _) | (Read::Members, Viewer::Member(_)) => Some(None),
        (Read::Owner, Viewer::Member(id)) => Some(Some(id)),
        _ => None,
    }
}

/// The caller of a content request: a valid key wins, then a member session (only while member login is live).
pub fn viewer(conn: &Connection, key: Option<&str>, member_token: Option<&str>) -> Viewer {
    if key.is_some_and(|k| crate::auth::actor_from_key(conn, k).is_some()) {
        return Viewer::Key;
    }
    match member_token {
        Some(t) if crate::members::login_enabled(conn, false) => crate::members::from_token(conn, t).map_or(Viewer::Anon, |m| Viewer::Member(m.id)),
        _ => Viewer::Anon,
    }
}

/// Who adds a record from the site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Submitter {
    Member(i64),
    /// An anonymous visitor, by client address key (for rate limits and history).
    Public(String),
}

const PUBLIC_PER_HOUR: i64 = 20;
const MEMBER_PER_HOUR: i64 = 60;
pub const MAX_SUBMIT_BYTES: usize = 64 * 1024;

fn db(e: rusqlite::Error) -> String {
    format!("读取数据库失败：{e}")
}

/// Adds a record to a live collection whose `create` rule lets `who` do it. It is live at once: no draft, a
/// version row with actor `member:<id>` or `public:<client>` and note 访客提交. Returns the new record's id.
pub fn submit(conn: &Connection, collection: &str, data: &Value, who: &Submitter) -> Result<i64, String> {
    let def = changes::live_def(conn, collection).map_err(|_| format!("没有可以提交的“{collection}”。"))?;
    let allowed = match (def.access.create, who) {
        (Create::Public, _) | (Create::Members, Submitter::Member(_)) => true,
        (Create::Members, Submitter::Public(_)) => return Err("请先登录再提交。".into()),
        (Create::None, _) => false,
    };
    if !allowed || def.single {
        return Err(format!("“{}”不接受提交。", def.title));
    }
    if data.to_string().len() > MAX_SUBMIT_BYTES {
        return Err("提交的内容太长了。".into());
    }
    let (key, limit, actor, owner) = match who {
        Submitter::Member(id) => (format!("submit:m:{id}"), MEMBER_PER_HOUR, format!("member:{id}"), Some(*id)),
        // The client address is only a rate-limit key, deleted within the hour; history just says 访客.
        Submitter::Public(client) => (format!("submit:ip:{client}"), PUBLIC_PER_HOUR, "访客".to_string(), None),
    };
    conn.execute("DELETE FROM member_attempts WHERE at <= datetime('now', '-1 hour')", []).map_err(db)?;
    let recent: i64 = conn.query_row("SELECT count(*) FROM member_attempts WHERE key = ?1", [&key], |r| r.get(0)).map_err(db)?;
    if recent >= limit {
        return Err("提交太频繁，请过一会儿再试。".into());
    }
    let doc = fill_defaults(&def, normalize(&def, &json!({}), data)?)?;
    changes::check_unique(conn, &def, None, &doc, true)?;
    check_live_relations(conn, &def, &doc)?;
    conn.execute("INSERT INTO member_attempts(key) VALUES (?1)", [&key]).map_err(db)?;
    let tx = conn.unchecked_transaction().map_err(db)?;
    tx.execute(
        "INSERT INTO records(collection, published, owner, submitted_at) VALUES (?1, ?2, ?3, datetime('now'))",
        params![collection, doc.to_string(), owner],
    )
    .map_err(db)?;
    let id = tx.last_insert_rowid();
    tx.execute("INSERT INTO releases(actor, note) VALUES (?1, '访客提交')", [&actor]).map_err(db)?;
    let release = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO versions(release_id, target, doc, prev, actor, note) VALUES (?1, ?2, ?3, NULL, ?4, '访客提交')",
        params![release, format!("record:{id}"), doc.to_string(), actor],
    )
    .map_err(db)?;
    tx.commit().map_err(db)?;
    Ok(id)
}

/// Relation values must point at live records of the target collection.
fn check_live_relations(conn: &Connection, def: &CollectionDef, doc: &Value) -> Result<(), String> {
    for f in def.fields.iter().filter(|f| f.kind == FieldType::Relation) {
        let ids = match doc.get(&f.name) {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Null) | None => continue,
            Some(v) => vec![v.clone()],
        };
        let target = f.target.as_deref().unwrap_or_default();
        for id in ids.iter().filter_map(Value::as_i64) {
            let live: Option<i64> = conn
                .query_row("SELECT id FROM records WHERE id = ?1 AND collection = ?2 AND published IS NOT NULL", params![id, target], |r| r.get(0))
                .optional()
                .map_err(db)?;
            if live.is_none() {
                return Err(format!("“{}”选的内容不存在。", f.title));
            }
        }
    }
    Ok(())
}

/// Visitor and member submissions to a collection since `since` (an SQLite datetime), for the nav's "N 条新".
pub fn new_submissions(conn: &Connection, collection: &str, since: Option<&str>) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM records WHERE collection = ?1 AND submitted_at IS NOT NULL AND published IS NOT NULL AND submitted_at > coalesce(?2, '')",
        params![collection, since],
        |r| r.get(0),
    )
    .unwrap_or(0)
}
