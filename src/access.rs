//! Who may read a collection's live records and who may add to it from the site. Records a visitor or member
//! adds go live at once (a form submission, a comment, an order), stamped with their owner.

use crate::changes;
use crate::members::Member;
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

/// What a reader may see of a collection's live records.
pub enum Scope {
    All,
    /// The records this member added.
    Owner(i64),
    /// The records a read rule lets them see.
    Rule(crate::rules::Compiled),
}

impl Scope {
    /// Narrows a query to what the reader may see.
    pub fn apply(self, q: &mut crate::query::Query) {
        match self {
            Scope::All => {}
            Scope::Owner(id) => q.owner = Some(id),
            Scope::Rule(rule) => q.rule = Some(rule),
        }
    }
}

/// What `viewer` may read of `def`: `None` when nothing. A read rule never says "nothing" up front: it is
/// asked of every record, a visitor included (a rule may open some records to everyone).
pub fn read_scope(conn: &Connection, def: &CollectionDef, viewer: Viewer) -> Result<Option<Scope>, String> {
    if let (Some(rule), false) = (&def.access.read_rule, viewer == Viewer::Key) {
        let member = match viewer {
            Viewer::Member(id) => crate::members::by_id(conn, id).map(|m| crate::members::member_json(&m)),
            _ => None,
        };
        return crate::rules::compile(Some(def), rule, member.as_ref(), None).map(|rule| Some(Scope::Rule(rule)));
    }
    Ok(read_filter(&def.access, viewer).map(|owner| owner.map_or(Scope::All, Scope::Owner)))
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
    let allowed = match (&def.access.create_rule, def.access.create, who) {
        // A condition: asked once the values are known, below.
        (Some(_), _, _) => true,
        (None, Create::Public, _) | (None, Create::Members, Submitter::Member(_)) => true,
        (None, Create::Members, Submitter::Public(_)) => return Err("请先登录再提交。".into()),
        (None, Create::None, _) => false,
    };
    if !allowed || def.single {
        return Err(format!("“{}”不接受提交。", def.title));
    }
    if data.to_string().len() > MAX_SUBMIT_BYTES {
        return Err("提交的内容太长了。".into());
    }
    let (key, actor) = throttle(conn, who)?;
    let owner = match who {
        Submitter::Member(id) => Some(*id),
        Submitter::Public(_) => None,
    };
    let doc = clean(&def, fill_defaults(&def, normalize(&def, &json!({}), data)?)?);
    if let Some(rule) = &def.access.create_rule {
        let member = match who {
            Submitter::Member(id) => crate::members::by_id(conn, *id).map(|m| crate::members::member_json(&m)),
            Submitter::Public(_) => None,
        };
        if !crate::rules::create_allows(conn, &def, rule, member.as_ref(), &doc)? {
            return Err(if member.is_some() { format!("“{}”不接受提交。", def.title) } else { "请先登录再提交。".into() });
        }
    }
    unique(conn, &def, None, &doc)?;
    check_live_relations(conn, &def, &doc)?;
    conn.execute("INSERT INTO member_attempts(key) VALUES (?1)", [&key]).map_err(db)?;
    let tx = conn.unchecked_transaction().map_err(db)?;
    tx.execute(
        "INSERT INTO records(collection, published, owner, submitted_at) VALUES (?1, ?2, ?3, datetime('now'))",
        params![collection, doc.to_string(), owner],
    )
    .map_err(db)?;
    let id = tx.last_insert_rowid();
    crate::routes::assign(&tx, &def, id, &doc)?;
    history(&tx, &actor, id, Some(&doc.to_string()), None, "访客提交")?;
    crate::hooks::fire(&tx, "submit", &crate::hooks::summary(&def, &doc), json!({ "collection": collection, "id": id, "record": doc }));
    tx.commit().map_err(db)?;
    Ok(id)
}

/// The rate-limit key and the history name of `who`; an error when they wrote too much within the hour.
fn throttle(conn: &Connection, who: &Submitter) -> Result<(String, String), String> {
    let (key, limit, actor) = match who {
        Submitter::Member(id) => (format!("submit:m:{id}"), MEMBER_PER_HOUR, format!("member:{id}")),
        // The client address is only a rate-limit key, deleted within the hour; history just says 访客.
        Submitter::Public(client) => (format!("submit:ip:{client}"), PUBLIC_PER_HOUR, "访客".to_string()),
    };
    conn.execute("DELETE FROM member_attempts WHERE at <= datetime('now', '-1 hour')", []).map_err(db)?;
    let recent: i64 = conn.query_row("SELECT count(*) FROM member_attempts WHERE key = ?1", [&key], |r| r.get(0)).map_err(db)?;
    if recent >= limit {
        return Err("提交太频繁，请过一会儿再试。".into());
    }
    Ok((key, actor))
}

/// Rich text from the site is cleaned, and keeps no images.
pub(crate) fn clean(def: &CollectionDef, mut doc: Value) -> Value {
    for f in def.fields.iter().filter(|f| f.kind == FieldType::Richtext) {
        if let Some(html) = doc.get(&f.name).and_then(Value::as_str) {
            let clean = ammonia::Builder::default().rm_tags(["img"]).clean(html).to_string();
            doc[&f.name] = json!(clean);
        }
    }
    doc
}

/// `check_unique` with a generic message: the real one names the other record's id and value, which a visitor
/// must not learn.
pub(crate) fn unique(conn: &Connection, def: &CollectionDef, id: Option<i64>, doc: &Value) -> Result<(), String> {
    changes::check_unique(conn, def, id, doc, true).map_err(|e| {
        let field = def.fields.iter().find(|f| e.contains(&format!("“{}”", f.name))).map_or("填写的内容", |f| f.title.as_str());
        format!("“{field}”这个值已经有人用过了。")
    })
}

/// One line of history for a record a visitor or member wrote: what it is now (none: removed) and what it was.
pub(crate) fn history(conn: &Connection, actor: &str, id: i64, doc: Option<&str>, prev: Option<&str>, note: &str) -> Result<(), String> {
    conn.execute("INSERT INTO releases(actor, note) VALUES (?1, ?2)", params![actor, note]).map_err(db)?;
    let release = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO versions(release_id, target, doc, prev, actor, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![release, format!("record:{id}"), doc, prev, actor, note],
    )
    .map_err(db)?;
    Ok(())
}

/// Why a change from the site was turned away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Not logged in.
    Login,
    /// Not allowed, or no such record: the two get one answer, so nobody learns which records exist.
    Forbidden,
    /// The values sent do not fit the collection.
    Invalid,
    /// Something else still needs this record.
    Conflict,
    /// Too many writes within the hour.
    Busy,
    Broken,
}

pub type Refused = (Refusal, String);

fn broken(e: String) -> Refused {
    (Refusal::Broken, e)
}

/// A live record of `collection` that the collection's rule (`update` or `delete`) lets `who` act on, with the
/// definition, the stored document as text, any pending draft, and who they are in history. `data` is what the
/// request sends, for rules that look at it.
#[allow(clippy::type_complexity)]
fn permitted(
    conn: &Connection, collection: &str, id: i64, who: &Submitter, verb: &str, rule: fn(&Access) -> Option<&str>, data: Option<&Value>,
) -> Result<(CollectionDef, String, Value, Option<String>, Option<Member>, String, String), Refused> {
    let refuse = |title: &str| (Refusal::Forbidden, format!("“{title}”不接受{verb}。"));
    let def = changes::live_def(conn, collection).map_err(|_| refuse(collection))?;
    let Some(rule) = rule(&def.access).map(str::to_string) else { return Err(refuse(&def.title)) };
    let (key, actor) = throttle(conn, who).map_err(|e| (Refusal::Busy, e))?;
    let member = match who {
        Submitter::Member(id) => crate::members::by_id(conn, *id),
        Submitter::Public(_) => None,
    };
    let no = || match member {
        Some(_) => (Refusal::Forbidden, format!("没有权限{verb}这条内容，或者它不存在。")),
        None => (Refusal::Login, format!("请先登录再{verb}。")),
    };
    let row: Option<(String, Option<String>)> = conn
        .query_row("SELECT published, draft FROM records WHERE id = ?1 AND collection = ?2 AND published IS NOT NULL", params![id, collection], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(|e| broken(db(e)))?;
    let Some((text, draft)) = row else { return Err(no()) };
    let old: Value = serde_json::from_str(&text).map_err(|e| broken(format!("记录 {id} 的内容已损坏：{e}")))?;
    // Before the values are looked at closely: someone who may not write learns nothing about what would fit.
    let wanted = data.map(|d| merged(&old, d));
    if !crate::rules::allows(conn, &def, &rule, member.as_ref(), id, wanted.as_ref()).map_err(broken)? {
        return Err(no());
    }
    Ok((def, text, old, draft, member, key, actor))
}

/// `base` with the keys of `over` written on top.
fn merged(base: &Value, over: &Value) -> Value {
    let mut out = base.clone();
    if let (Some(out), Some(over)) = (out.as_object_mut(), over.as_object()) {
        out.extend(over.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    out
}

/// Changes the fields in `data` of a live record, when the collection's `update` rule lets `who`. Live at once,
/// like a submission; a draft the admin has pending gets the same fields, so publishing it does not undo this.
pub fn update(conn: &Connection, collection: &str, id: i64, data: &Value, who: &Submitter) -> Result<(), Refused> {
    if data.to_string().len() > MAX_SUBMIT_BYTES {
        return Err((Refusal::Invalid, "提交的内容太长了。".into()));
    }
    let (def, text, old, draft, member, key, actor) = permitted(conn, collection, id, who, "修改", |a| a.update.as_deref(), Some(data))?;
    let invalid = |e: String| (Refusal::Invalid, e);
    let doc = clean(&def, normalize(&def, &old, data).map_err(invalid)?);
    // Again on what will really be stored: cleaning may have changed a value the rule looks at.
    let rule = def.access.update.as_deref().unwrap_or("none");
    if !crate::rules::allows(conn, &def, rule, member.as_ref(), id, Some(&doc)).map_err(broken)? {
        return Err((Refusal::Forbidden, "没有权限修改这条内容，或者它不存在。".into()));
    }
    unique(conn, &def, Some(id), &doc).map_err(invalid)?;
    check_live_relations(conn, &def, &doc).map_err(invalid)?;
    let pending = match draft {
        Some(d) => {
            let d: Value = serde_json::from_str(&d).map_err(|e| broken(format!("记录 {id} 的草稿已损坏：{e}")))?;
            let sent: Value = data.as_object().map(|o| o.keys().map(|k| (k.clone(), doc[k].clone())).collect::<serde_json::Map<_, _>>().into()).unwrap_or_default();
            Some(merged(&d, &sent).to_string())
        }
        None => None,
    };
    let run = || -> Result<(), String> {
        conn.execute("INSERT INTO member_attempts(key) VALUES (?1)", [&key]).map_err(db)?;
        let tx = conn.unchecked_transaction().map_err(db)?;
        tx.execute("UPDATE records SET published = ?2, draft = coalesce(?3, draft), updated_at = datetime('now') WHERE id = ?1", params![id, doc.to_string(), pending])
            .map_err(db)?;
        history(&tx, &actor, id, Some(&doc.to_string()), Some(&text), "访客修改")?;
        tx.commit().map_err(db)
    };
    run().map_err(broken)
}

/// Removes a live record, when the collection's `delete` rule lets `who` and no other live record points at it.
/// History keeps what it was, so the admin can bring it back.
pub fn delete(conn: &Connection, collection: &str, id: i64, who: &Submitter) -> Result<(), Refused> {
    let (_, text, _, _, _, key, actor) = permitted(conn, collection, id, who, "删除", |a| a.delete.as_deref(), None)?;
    if pointed_at(conn, collection, id).map_err(broken)? {
        return Err((Refusal::Conflict, "还有别的内容用到这一条，不能删除。".into()));
    }
    let run = || -> Result<(), String> {
        conn.execute("INSERT INTO member_attempts(key) VALUES (?1)", [&key]).map_err(db)?;
        let tx = conn.unchecked_transaction().map_err(db)?;
        tx.execute("UPDATE records SET published = NULL, updated_at = datetime('now') WHERE id = ?1", [id]).map_err(db)?;
        history(&tx, &actor, id, None, Some(&text), "访客删除")?;
        tx.commit().map_err(db)
    };
    run().map_err(broken)
}

/// Whether a live record of any collection has a relation to record `id` of `collection`. Publishing checks
/// every live relation, so a removal that left one dangling would block all later publishes.
pub(crate) fn pointed_at(conn: &Connection, collection: &str, id: i64) -> Result<bool, String> {
    for (name, def) in changes::live_defs(conn)? {
        for f in def.fields.iter().filter(|f| f.kind == FieldType::Relation && f.target.as_deref() == Some(collection) && crate::schema::valid_name(&f.name)) {
            let sql = format!(
                "SELECT 1 FROM records, json_each(CASE json_type(published,'$.{0}') WHEN 'array' THEN json_extract(published,'$.{0}')                  ELSE json_array(json_extract(published,'$.{0}')) END) AS v WHERE collection = ?1 AND published IS NOT NULL AND v.value = ?2 LIMIT 1",
                f.name
            );
            if conn.query_row(&sql, params![name, id], |_| Ok(())).optional().map_err(db)?.is_some() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Relation values must point at live records of the target collection.
pub(crate) fn check_live_relations(conn: &Connection, def: &CollectionDef, doc: &Value) -> Result<(), String> {
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
