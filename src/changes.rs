//! Draft state: every object has a published copy and a draft copy; these functions move the draft.

use crate::auth::Actor;
use crate::schema::CollectionDef;
use rusqlite::{params, types::Value as Sql, Connection, OptionalExtension};
use serde_json::Value;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Collection(String),
    Record(i64),
    View(String),
    Notes,
}

impl Target {
    pub fn parse(s: &str) -> Result<Target, String> {
        let bad = || format!("对象标识“{s}”不合法：请使用 collection:<名字>、record:<编号>、view:<名字> 或 notes。");
        if s == "notes" {
            return Ok(Target::Notes);
        }
        match s.split_once(':').ok_or_else(bad)? {
            ("collection", n) if crate::schema::valid_name(n) => Ok(Target::Collection(n.into())),
            ("view", n) if crate::schema::valid_name(n) => Ok(Target::View(n.into())),
            ("record", n) => n.parse().map(Target::Record).map_err(|_| bad()),
            _ => Err(bad()),
        }
    }

    /// (table, key column, key value, has draft_deleted column)
    fn row(&self) -> (&'static str, &'static str, Sql, bool) {
        match self {
            Target::Collection(n) => ("collections", "name", Sql::Text(n.clone()), true),
            Target::Record(id) => ("records", "id", Sql::Integer(*id), true),
            Target::View(n) => ("views", "name", Sql::Text(n.clone()), true),
            Target::Notes => ("notes", "id", Sql::Integer(1), false),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Collection(n) => write!(f, "collection:{n}"),
            Target::Record(id) => write!(f, "record:{id}"),
            Target::View(n) => write!(f, "view:{n}"),
            Target::Notes => write!(f, "notes"),
        }
    }
}

pub fn status(published: bool, draft: bool, draft_deleted: bool) -> &'static str {
    match (published, draft, draft_deleted) {
        (false, true, _) => "draft",
        (false, false, _) => "deleted",
        (true, _, true) => "deleting",
        (true, true, false) => "changed",
        (true, false, false) => "published",
    }
}

fn db(target: &Target, e: rusqlite::Error) -> String {
    format!("读写“{target}”失败：{e}")
}

fn missing(target: &Target) -> String {
    format!("“{target}”不存在，请检查名字或编号。")
}

/// Records carry `updated_at`; every write to one refreshes it.
fn touch(target: &Target) -> &'static str {
    if matches!(target, Target::Record(_)) { ", updated_at = datetime('now')" } else { "" }
}

pub fn save_draft(conn: &Connection, actor: &Actor, target: &Target, doc: &Value, note: &str) -> Result<(), String> {
    let (table, key, val, has_dd) = target.row();
    let doc = doc.to_string();
    let reset = if has_dd { ", draft_deleted = 0" } else { "" };
    let reset = format!("{reset}{}", touch(target));
    let n = conn
        .execute(
            &format!("UPDATE {table} SET draft = ?1, draft_actor = ?2, draft_note = ?3{reset} WHERE {key} = ?4"),
            params![doc, actor.name, note, val],
        )
        .map_err(|e| db(target, e))?;
    if n > 0 {
        return Ok(());
    }
    if matches!(target, Target::Record(_)) {
        return Err(missing(target));
    }
    conn.execute(
        &format!("INSERT INTO {table}({key}, draft, draft_actor, draft_note) VALUES(?1, ?2, ?3, ?4)"),
        params![val, doc, actor.name, note],
    )
    .map_err(|e| db(target, e))?;
    Ok(())
}

/// (has a published copy, has ever been published). A record deleted after being live has neither a published
/// copy nor a draft, but it has history: its row is a tombstone that keeps the id restorable.
fn liveness(conn: &Connection, target: &Target) -> Result<(bool, bool), String> {
    let (table, key, val, _) = target.row();
    let live: bool = conn
        .query_row(&format!("SELECT published IS NOT NULL FROM {table} WHERE {key} = ?1"), [val], |r| r.get(0))
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => missing(target),
            e => db(target, e),
        })?;
    let history: bool = conn
        .query_row("SELECT EXISTS(SELECT 1 FROM versions WHERE target = ?1)", [target.to_string()], |r| r.get(0))
        .map_err(|e| db(target, e))?;
    Ok((live, live || history))
}

/// Clears the draft and any pending deletion, leaving the published copy (or the tombstone) as it is.
fn clear_draft(conn: &Connection, target: &Target) -> Result<(), String> {
    let (table, key, val, has_dd) = target.row();
    let reset = if has_dd { ", draft_deleted = 0" } else { "" };
    let reset = format!("{reset}{}", touch(target));
    conn.execute(&format!("UPDATE {table} SET draft = NULL, draft_actor = NULL, draft_note = NULL{reset} WHERE {key} = ?1"), [val])
        .map_err(|e| db(target, e))?;
    Ok(())
}

/// Delete the row of a never-published object (and, for a collection, its records).
fn remove_row(conn: &Connection, target: &Target) -> Result<(), String> {
    let (table, key, val, _) = target.row();
    if let Target::Collection(n) = target {
        conn.execute("DELETE FROM records WHERE collection = ?1", [n]).map_err(|e| db(target, e))?;
    }
    conn.execute(&format!("DELETE FROM {table} WHERE {key} = ?1"), [val]).map_err(|e| db(target, e))?;
    Ok(())
}

pub fn mark_deleted(conn: &Connection, actor: &Actor, target: &Target, note: &str) -> Result<(), String> {
    if matches!(target, Target::Notes) {
        return Err("说明不能删除，请直接修改它的内容。".into());
    }
    let tx = conn.unchecked_transaction().map_err(|e| db(target, e))?;
    let (live, ever) = liveness(&tx, target)?;
    if !ever {
        remove_row(&tx, target)?;
    } else if !live {
        // A tombstone whose restored draft is being dropped again: back to the tombstone.
        clear_draft(&tx, target)?;
    } else {
        let (table, key, val, _) = target.row();
        tx.execute(
            &format!("UPDATE {table} SET draft_deleted = 1, draft_actor = ?1, draft_note = ?2{} WHERE {key} = ?3", touch(target)),
            params![actor.name, note, val],
        )
        .map_err(|e| db(target, e))?;
    }
    tx.commit().map_err(|e| db(target, e))
}

pub fn discard(conn: &Connection, target: &Target) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| db(target, e))?;
    if !liveness(&tx, target)?.1 && !matches!(target, Target::Notes) {
        remove_row(&tx, target)?;
    } else {
        if let Target::Collection(name) = target {
            // Record drafts follow the draft definition; dropping it means they must fit the live one again.
            let row = coll_row_required(&tx, name)?;
            if let (Some(live), Some(draft)) = (&row.published, &row.draft) {
                let stuck = rewrite_drafts(&tx, name, &parse_def(name, draft)?, &parse_def(name, live)?)?;
                if !stuck.is_empty() {
                    let ids: Vec<String> = stuck.iter().take(5).map(|id| format!("record:{id}")).collect();
                    return Err(format!(
                        "有 {} 条记录的草稿是按这份草稿定义写的，退不回线上的定义（例如 {}）：请先修改或放弃这些记录的草稿，再放弃集合的草稿。",
                        stuck.len(),
                        ids.join("、")
                    ));
                }
            }
        }
        clear_draft(&tx, target)?;
    }
    tx.commit().map_err(|e| db(target, e))
}

struct CollRow {
    published: Option<String>,
    draft: Option<String>,
    draft_deleted: bool,
    deleted: bool,
}

fn coll_row(conn: &Connection, name: &str) -> Result<Option<CollRow>, String> {
    conn.query_row("SELECT published, draft, draft_deleted, deleted FROM collections WHERE name = ?1", [name], |r| {
        Ok(CollRow { published: r.get(0)?, draft: r.get(1)?, draft_deleted: r.get(2)?, deleted: r.get(3)? })
    })
    .optional()
    .map_err(|e| format!("读取集合“{name}”失败：{e}"))
}

fn coll_row_required(conn: &Connection, name: &str) -> Result<CollRow, String> {
    coll_row(conn, name)?.ok_or_else(|| format!("集合“{name}”不存在，请检查名字。"))
}

fn parse_def(name: &str, json: &str) -> Result<CollectionDef, String> {
    serde_json::from_str(json).map_err(|e| format!("集合“{name}”的定义已损坏：{e}"))
}

/// Draft definition if there is one, else the published one; errors if the collection is gone or pending deletion.
pub fn effective_def(conn: &Connection, name: &str) -> Result<CollectionDef, String> {
    let r = coll_row_required(conn, name)?;
    if r.deleted {
        return Err(format!("集合“{name}”已被删除。"));
    }
    if r.draft_deleted {
        return Err(format!("集合“{name}”正等待删除：请先放弃草稿再修改。"));
    }
    parse_def(name, r.draft.or(r.published).as_deref().ok_or_else(|| format!("集合“{name}”没有定义。"))?)
}

/// The published, non-deleted definition, or `None` when the collection is not live.
fn live_def_opt(conn: &Connection, name: &str) -> Result<Option<CollectionDef>, String> {
    match coll_row(conn, name)? {
        Some(r) if !r.deleted => r.published.map(|p| parse_def(name, &p)).transpose(),
        _ => Ok(None),
    }
}

/// The published, non-deleted definition.
pub fn live_def(conn: &Connection, name: &str) -> Result<CollectionDef, String> {
    live_def_opt(conn, name)?.ok_or_else(|| format!("集合“{name}”尚未发布或已删除，线上没有它。"))
}

/// True when the collection is deleted or waiting to be deleted: its records cannot be published.
fn collection_gone(conn: &Connection, name: &str) -> Result<bool, String> {
    Ok(coll_row(conn, name)?.is_none_or(|r| r.deleted || r.draft_deleted))
}

fn row_status(r: &CollRow) -> &'static str {
    // A deleted collection whose definition was restored into the draft is on its way back.
    if r.deleted && r.draft.is_none() { "deleted" } else { status(r.published.is_some(), r.draft.is_some(), r.draft_deleted) }
}

/// Status of a collection; a row that is gone (a never-published collection was removed) reads as "deleted".
pub fn collection_status(conn: &Connection, name: &str) -> Result<&'static str, String> {
    Ok(coll_row(conn, name)?.map_or("deleted", |r| row_status(&r)))
}

/// Definition (draft if any, else published) and status of a collection; `None` if it is missing or deleted.
pub fn collection_view(conn: &Connection, name: &str) -> Result<Option<(CollectionDef, &'static str)>, String> {
    let Some(r) = coll_row(conn, name)?.filter(|r| !r.deleted || r.draft.is_some()) else { return Ok(None) };
    let st = row_status(&r);
    let json = r.draft.or(r.published).ok_or_else(|| format!("集合“{name}”没有定义。"))?;
    Ok(Some((parse_def(name, &json)?, st)))
}

/// Inserts a new record whose draft is `doc` (already normalized) and returns its id.
pub fn insert_record(conn: &Connection, actor: &Actor, collection: &str, doc: &Value, note: &str) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO records(collection, draft, draft_actor, draft_note) VALUES(?1, ?2, ?3, ?4)",
        params![collection, doc.to_string(), actor.name, note],
    )
    .map_err(|e| format!("新建集合“{collection}”的记录失败：{e}"))?;
    Ok(conn.last_insert_rowid())
}

/// Every `unique` field of `doc` must differ from the same field of all other records of the collection
/// (tombstones skipped). `live_only` compares against published values only, else against drafts too.
pub fn check_unique(conn: &Connection, def: &CollectionDef, id: Option<i64>, doc: &Value, live_only: bool) -> Result<(), String> {
    if !crate::schema::valid_name(&def.name) {
        return Err(format!("集合名“{}”不合法，无法检查唯一性。", def.name));
    }
    for f in def.fields.iter().filter(|f| f.unique) {
        if !crate::schema::valid_name(&f.name) {
            return Err(format!("字段名“{}”不合法，无法检查唯一性。", f.name));
        }
        let Some(p) = doc.get(&f.name).and_then(crate::query::to_sql) else { continue };
        let draft = if live_only { String::new() } else { format!(" OR json_extract(draft,'$.{0}') = ?2", f.name) };
        let sql = format!(
            "SELECT id FROM records WHERE collection = ?1 AND id IS NOT ?3 AND (published IS NOT NULL OR draft IS NOT NULL) \
             AND (json_extract(published,'$.{0}') = ?2{draft}) LIMIT 1",
            f.name
        );
        let other: Option<i64> = conn
            .query_row(&sql, params![def.name, p, id], |r| r.get(0))
            .optional()
            .map_err(|e| format!("检查字段“{}”是否唯一失败：{e}", f.name))?;
        if let Some(other) = other {
            let shown = doc[&f.name].to_string();
            let shown = if shown.chars().count() > 50 { format!("{}…", shown.chars().take(50).collect::<String>()) } else { shown };
            return Err(format!("字段“{}”必须唯一：值 {shown} 已被记录 {other} 使用，请换一个值。", f.name));
        }
    }
    Ok(())
}

// ---- structural change rules ----
//
// Invariant: a record's published copy follows the collection's live definition, and its draft copy follows the
// definition in force for drafts (the draft definition when there is one, else the live one). Saving or dropping a
// draft definition therefore rewrites the record drafts in the same transaction.

/// The definition drafts are written against: the draft definition if any, else the published one.
fn current_def(conn: &Connection, name: &str) -> Result<Option<CollectionDef>, String> {
    let Some(row) = coll_row(conn, name)? else { return Ok(None) };
    row.draft.or(row.published).map(|j| parse_def(name, &j)).transpose()
}

/// Rewrites every record draft of a collection from one definition to another. Returns the ids that cannot
/// follow; drafts of records that are being deleted are left alone.
fn rewrite_drafts(conn: &Connection, name: &str, from: &CollectionDef, to: &CollectionDef) -> Result<Vec<i64>, String> {
    let fail = |e: rusqlite::Error| format!("改写集合“{name}”的记录草稿失败：{e}");
    let mut stmt = conn.prepare("SELECT id, draft, draft_deleted FROM records WHERE collection = ?1 AND draft IS NOT NULL ORDER BY id").map_err(fail)?;
    let rows: Vec<(i64, String, bool)> =
        stmt.query_map([name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
    let mut stuck = vec![];
    for (id, json, deleting) in rows {
        let doc: Value = serde_json::from_str(&json).map_err(|e| format!("记录 {id} 的内容已损坏：{e}"))?;
        match crate::schema::migrate(from, to, &doc) {
            Ok(next) if next != doc => {
                conn.execute("UPDATE records SET draft = ?1, updated_at = datetime('now') WHERE id = ?2", params![next.to_string(), id]).map_err(fail)?;
            }
            Ok(_) => {}
            Err(_) if deleting => {}
            Err(_) => stuck.push(id),
        }
    }
    Ok(stuck)
}

/// Saves a draft definition for a collection. For an existing collection it first refuses anything the stored
/// records cannot follow, then brings every record draft into the new shape, all in one transaction.
pub fn save_definition(conn: &Connection, actor: &Actor, new: &CollectionDef, note: &str) -> Result<(), String> {
    let target = Target::Collection(new.name.clone());
    check_structure(conn, &new.name, new)?;
    let tx = conn.unchecked_transaction().map_err(|e| db(&target, e))?;
    if let Some(current) = current_def(&tx, &new.name)? {
        let stuck = rewrite_drafts(&tx, &new.name, &current, new)?;
        if let Some(id) = stuck.first() {
            return Err(format!("记录 {id} 的草稿跟不上新的定义，改动没有保存：请先修改这条记录。"));
        }
    }
    save_draft(&tx, actor, &target, &serde_json::json!(new), note)?;
    tx.commit().map_err(|e| db(&target, e))
}

/// Rejects a new definition that existing records cannot follow: values that do not convert, required fields
/// left empty, unique fields holding duplicates. Looks at both copies of every record of the collection.
pub fn check_structure(conn: &Connection, name: &str, new: &CollectionDef) -> Result<(), String> {
    let Some(row) = coll_row(conn, name)? else { return Ok(()) };
    let live = row.published.as_deref().map(|j| parse_def(name, j)).transpose()?;
    let current = match row.draft.as_deref() {
        Some(j) => Some(parse_def(name, j)?),
        None => live.clone(),
    };
    let fail = |e: rusqlite::Error| format!("检查集合“{name}”的记录失败：{e}");
    let mut stmt = conn
        .prepare("SELECT id, published, draft FROM records WHERE collection = ?1 AND (published IS NOT NULL OR draft IS NOT NULL) ORDER BY id")
        .map_err(fail)?;
    let rows: Vec<(i64, Option<String>, Option<String>)> =
        stmt.query_map([name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;

    let mut failing: Vec<i64> = vec![];
    let mut samples: Vec<String> = vec![];
    let mut seen: std::collections::HashMap<(String, String), i64> = std::collections::HashMap::new();
    let mut note = |id: i64, why: String, failing: &mut Vec<i64>| {
        if !failing.contains(&id) {
            failing.push(id);
        }
        let why = why.trim_end_matches('。').to_string();
        if !why.is_empty() && samples.len() < 3 && !samples.contains(&why) {
            samples.push(why);
        }
    };
    for (id, published, draft) in rows {
        // Published copies follow the live definition; drafts follow the current draft definition.
        for (json, old) in [(published, live.as_ref().or(current.as_ref())), (draft, current.as_ref())] {
            let (Some(json), Some(old)) = (json, old) else { continue };
            let doc: Value = serde_json::from_str(&json).map_err(|e| format!("记录 {id} 的内容已损坏：{e}"))?;
            let next = match crate::schema::migrate(old, new, &doc) {
                Ok(next) => next,
                Err(e) => {
                    note(id, format!("记录 {id}：{e}"), &mut failing);
                    continue;
                }
            };
            for f in new.fields.iter().filter(|f| f.unique) {
                let Some(v) = next.get(&f.name).filter(|v| !v.is_null()) else { continue };
                match seen.get(&(f.name.clone(), v.to_string())) {
                    Some(other) if *other != id => {
                        let full = v.to_string();
                        let shown = if full.chars().count() > 50 { format!("{}…", full.chars().take(50).collect::<String>()) } else { full };
                        note(id, format!("字段“{}”的值 {shown} 在记录 {other} 和记录 {id} 里重复", f.name), &mut failing);
                        note(*other, String::new(), &mut failing);
                    }
                    Some(_) => {}
                    None => {
                        seen.insert((f.name.clone(), v.to_string()), id);
                    }
                }
            }
        }
    }
    if failing.is_empty() {
        return Ok(());
    }
    let hint = if samples.iter().any(|s| s.contains("必填")) { "；如果是新增的必填字段，可以给它一个默认值，或者先不设为必填" } else { "" };
    Err(format!(
        "有 {} 条记录不符合新的定义，改动没有保存。例如：{}。请先修改这些记录，或调整这次改动{hint}。",
        failing.len(),
        samples.join("；")
    ))
}

/// How many records hold a value (in either copy) for any of these fields.
pub fn affected_by_removal(conn: &Connection, name: &str, fields: &[String]) -> Result<i64, String> {
    if fields.is_empty() {
        return Ok(0);
    }
    let mut holds = vec![];
    for f in fields {
        if !crate::schema::valid_name(f) {
            return Err(format!("字段名“{f}”不合法。"));
        }
        holds.push(format!("json_extract(published,'$.{f}') IS NOT NULL OR json_extract(draft,'$.{f}') IS NOT NULL"));
    }
    conn.query_row(
        &format!(
            "SELECT count(*) FROM records WHERE collection = ?1 AND (published IS NOT NULL OR draft IS NOT NULL) AND ({})",
            holds.join(" OR ")
        ),
        [name],
        |r| r.get(0),
    )
    .map_err(|e| format!("统计集合“{name}”受影响的记录失败：{e}"))
}

// ---- pending list, publish, history ----

/// One object's two copies and who last drafted it.
struct Slot {
    published: Option<String>,
    draft: Option<String>,
    draft_deleted: bool,
    actor: Option<String>,
    note: Option<String>,
}

fn slot(conn: &Connection, target: &Target) -> Result<Option<Slot>, String> {
    let (table, key, val, has_dd) = target.row();
    let dd = if has_dd { "draft_deleted" } else { "0" };
    conn.query_row(
        &format!("SELECT published, draft, {dd}, draft_actor, draft_note FROM {table} WHERE {key} = ?1"),
        [val],
        |r| Ok(Slot { published: r.get(0)?, draft: r.get(1)?, draft_deleted: r.get(2)?, actor: r.get(3)?, note: r.get(4)? }),
    )
    .optional()
    .map_err(|e| db(target, e))
}

/// Status of any object; a missing row reads as "deleted".
pub fn target_status(conn: &Connection, target: &Target) -> Result<&'static str, String> {
    if let Target::Collection(name) = target {
        return collection_status(conn, name);
    }
    Ok(slot(conn, target)?.map_or("deleted", |s| status(s.published.is_some(), s.draft.is_some(), s.draft_deleted)))
}

fn names(conn: &Connection, sql: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| format!("读取待发布清单失败：{e}"))?;
    let rows = stmt.query_map([], |r| r.get(0)).map_err(|e| format!("读取待发布清单失败：{e}"))?;
    rows.collect::<Result<_, _>>().map_err(|e| format!("读取待发布清单失败：{e}"))
}

/// Every object with a draft or a pending deletion, in release order: collections, views, notes, records.
fn pending_targets(conn: &Connection) -> Result<Vec<Target>, String> {
    let mut out: Vec<Target> = vec![];
    let waiting = "draft IS NOT NULL OR draft_deleted = 1";
    out.extend(names(conn, &format!("SELECT name FROM collections WHERE {waiting} ORDER BY name"))?.into_iter().map(Target::Collection));
    out.extend(names(conn, &format!("SELECT name FROM views WHERE {waiting} ORDER BY name"))?.into_iter().map(Target::View));
    if !names(conn, "SELECT 'notes' FROM notes WHERE draft IS NOT NULL")?.is_empty() {
        out.push(Target::Notes);
    }
    let mut stmt = conn
        .prepare(&format!("SELECT id FROM records WHERE {waiting} ORDER BY id"))
        .map_err(|e| format!("读取待发布清单失败：{e}"))?;
    let ids = stmt.query_map([], |r| r.get(0)).map_err(|e| format!("读取待发布清单失败：{e}"))?;
    for id in ids {
        out.push(Target::Record(id.map_err(|e| format!("读取待发布清单失败：{e}"))?));
    }
    Ok(out)
}

fn parse_doc(json: &Option<String>) -> Value {
    json.as_deref().and_then(|j| serde_json::from_str(j).ok()).unwrap_or(Value::Null)
}

/// Top-level keys whose values differ between the two documents.
fn changed_keys(a: &Value, b: &Value) -> Vec<String> {
    let (Some(a), Some(b)) = (a.as_object(), b.as_object()) else { return vec![] };
    let mut keys: Vec<String> = a.keys().chain(b.keys()).filter(|k| a.get(*k) != b.get(*k)).cloned().collect();
    keys.sort();
    keys.dedup();
    keys
}

/// How many records a collection holds, tombstones not counted.
pub fn record_count(conn: &Connection, name: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT count(*) FROM records WHERE collection = ?1 AND (published IS NOT NULL OR draft IS NOT NULL)",
        [name],
        |r| r.get(0),
    )
    .map_err(|e| format!("统计集合“{name}”的记录数失败：{e}"))
}

#[derive(Debug, serde::Serialize)]
pub struct Pending {
    pub target: String,
    /// created | changed | deleted
    pub kind: &'static str,
    pub title: String,
    /// Top-level keys that differ from the published copy; empty for created and deleted.
    pub fields: Vec<String>,
    pub actor: String,
    pub note: String,
    /// For a collection: how many records it holds. Otherwise 1.
    pub affected: i64,
}

pub fn pending(conn: &Connection) -> Result<Vec<Pending>, String> {
    let mut out = vec![];
    for target in pending_targets(conn)? {
        let Some(s) = slot(conn, &target)? else { continue };
        let (old, new) = (parse_doc(&s.published), parse_doc(&s.draft));
        let shown = if new.is_null() { &old } else { &new };
        let title = match &target {
            Target::Notes => "站点说明".to_string(),
            Target::Record(id) => ["title", "name"]
                .iter()
                .find_map(|k| shown.get(*k).and_then(Value::as_str))
                .map_or_else(|| format!("记录 {id}"), String::from),
            Target::Collection(n) | Target::View(n) => shown.get("title").and_then(Value::as_str).unwrap_or(n).to_string(),
        };
        let kind = if s.draft_deleted { "deleted" } else if s.published.is_none() { "created" } else { "changed" };
        out.push(Pending {
            target: target.to_string(),
            kind,
            title,
            fields: if kind == "changed" { changed_keys(&old, &new) } else { vec![] },
            actor: s.actor.unwrap_or_default(),
            note: s.note.unwrap_or_default(),
            affected: if let Target::Collection(n) = &target { record_count(conn, n)? } else { 1 },
        });
    }
    Ok(out)
}

#[derive(Debug, serde::Serialize)]
pub struct Release {
    pub id: i64,
    pub published: Vec<String>,
    /// Pending objects left alone because the actor may not publish them.
    pub skipped: Vec<String>,
}

/// The scope that covers changes to this object: content needs write, everything else is structure.
pub fn own_scope(target: &Target) -> crate::auth::Scope {
    use crate::auth::Scope;
    if matches!(target, Target::Record(_)) { Scope::Write } else { Scope::Structure }
}

fn may_publish(actor: &Actor, target: &Target) -> bool {
    actor.can(crate::auth::Scope::Publish) && actor.can(own_scope(target))
}

/// Order inside one release: structure first, then content, deletions last.
fn rank(target: &Target, deleting: bool) -> u8 {
    match (target, deleting) {
        (Target::Collection(_), false) => 0,
        (Target::View(_), false) => 1,
        (Target::Notes, _) => 2,
        (Target::Record(_), false) => 3,
        (Target::Record(_), true) => 4,
        (Target::View(_), true) => 5,
        (Target::Collection(_), true) => 6,
    }
}

/// Rewrites every record of a collection (both copies) from the old definition to the new one.
/// Returns the records whose live copy changed, with the copy they had before.
fn migrate_records(conn: &Connection, name: &str, old: &CollectionDef, new: &CollectionDef, problems: &mut Vec<String>) -> Result<Vec<(i64, String)>, String> {
    let fail = |e: rusqlite::Error| format!("改写集合“{name}”的记录失败：{e}");
    let mut stmt = conn
        .prepare("SELECT id, published, draft, draft_deleted FROM records WHERE collection = ?1 AND (published IS NOT NULL OR draft IS NOT NULL)")
        .map_err(fail)?;
    let rows: Vec<(i64, Option<String>, Option<String>, bool)> =
        stmt.query_map([name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
    let mut rewritten = vec![];
    for (id, published, draft, deleting) in rows {
        for (column, json) in [("published", published), ("draft", draft)] {
            let Some(json) = json else { continue };
            let doc: Value = serde_json::from_str(&json).map_err(|e| format!("记录 {id} 的内容已损坏：{e}"))?;
            match crate::schema::migrate(old, new, &doc) {
                Ok(next) if next != doc => {
                    conn.execute(&format!("UPDATE records SET {column} = ?1, updated_at = datetime('now') WHERE id = ?2"), params![next.to_string(), id])
                        .map_err(fail)?;
                    if column == "published" {
                        rewritten.push((id, json));
                    }
                }
                Ok(_) => {}
                // The abandoned draft of a record that is being deleted must not hold up the release.
                Err(_) if column == "draft" && deleting => {}
                Err(e) => problems.push(format!("record:{id}：{e}")),
            }
        }
    }
    Ok(rewritten)
}

/// Checks every live record of the touched collections against its live definition.
// ponytail: re-validates every live record of each touched collection on each publish; make it incremental
// if collections reach ~100k records.
fn verify(conn: &Connection, touched: &[String], released: &[(i64, String)], problems: &mut Vec<String>) -> Result<(), String> {
    for (id, collection) in released {
        if live_def_opt(conn, collection)?.is_some() {
            continue;
        }
        problems.push(if collection_gone(conn, collection)? {
            format!("record:{id} 所属的 collection:{collection} 已删除，这条记录的草稿上不了线：请用 discard_draft 放弃它，或先把集合恢复。")
        } else {
            format!("record:{id} 所属的集合还没有上线：请把 collection:{collection} 一起发布。")
        });
    }
    for name in touched {
        let Some(def) = live_def_opt(conn, name)? else { continue };
        let fail = |e: rusqlite::Error| format!("检查集合“{name}”的记录失败：{e}");
        let mut stmt = conn.prepare("SELECT id, published FROM records WHERE collection = ?1 AND published IS NOT NULL").map_err(fail)?;
        let rows: Vec<(i64, String)> =
            stmt.query_map([name], |r| Ok((r.get(0)?, r.get(1)?))).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
        for (id, json) in rows {
            let doc: Value = serde_json::from_str(&json).map_err(|e| format!("记录 {id} 的内容已损坏：{e}"))?;
            if let Err(e) = crate::schema::normalize(&def, &doc, &serde_json::json!({})) {
                problems.push(format!("record:{id}：{e}"));
            }
            if let Err(e) = check_unique(conn, &def, Some(id), &doc, true) {
                problems.push(format!("record:{id}：{e}"));
            }
        }
    }
    Ok(())
}

/// (name, definition) of every collection that is live: published and not deleted.
fn live_defs(conn: &Connection) -> Result<Vec<(String, CollectionDef)>, String> {
    let fail = |e: rusqlite::Error| format!("读取线上的集合失败：{e}");
    let mut stmt = conn.prepare("SELECT name, published FROM collections WHERE deleted = 0 AND published IS NOT NULL ORDER BY name").map_err(fail)?;
    let rows: Vec<(String, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
    rows.into_iter().map(|(name, json)| Ok((name.clone(), parse_def(&name, &json)?))).collect()
}

/// After a release, a live record may only point at live records, and a live collection only at live collections.
/// Checks the touched collections and every live collection that points at one of them.
// ponytail: one lookup per relation value and a linear scan of live definitions; batch the lookups if a
// collection reaches ~100k related records.
pub fn check_relations(conn: &Connection, touched: &[String], problems: &mut Vec<String>) -> Result<(), String> {
    use crate::schema::FieldType;
    let defs = live_defs(conn)?;
    let is_live = |name: &str| defs.iter().any(|(n, _)| n == name);
    for (name, def) in &defs {
        let relations: Vec<_> = def.fields.iter().filter(|f| f.kind == FieldType::Relation).collect();
        for f in &relations {
            let target = f.target.as_deref().unwrap_or_default();
            if !is_live(target) {
                problems.push(format!("collection:{name} 的关联字段“{}”指向 collection:{target}，它不在线上：请把它一起发布，或先去掉这个字段。", f.name));
            }
        }
        let in_scope = touched.contains(name) || relations.iter().any(|f| touched.iter().any(|t| Some(t.as_str()) == f.target.as_deref()));
        if relations.is_empty() || !in_scope {
            continue;
        }
        let fail = |e: rusqlite::Error| format!("检查集合“{name}”的关联失败：{e}");
        let mut stmt = conn.prepare("SELECT id, published FROM records WHERE collection = ?1 AND published IS NOT NULL").map_err(fail)?;
        let rows: Vec<(i64, String)> = stmt.query_map([name], |r| Ok((r.get(0)?, r.get(1)?))).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
        for (id, json) in rows {
            let doc: Value = serde_json::from_str(&json).map_err(|e| format!("记录 {id} 的内容已损坏：{e}"))?;
            for f in &relations {
                let target = f.target.as_deref().unwrap_or_default();
                let ids: Vec<i64> = match doc.get(&f.name) {
                    Some(Value::Array(a)) => a.iter().filter_map(Value::as_i64).collect(),
                    Some(v) => v.as_i64().into_iter().collect(),
                    None => vec![],
                };
                for rid in ids {
                    let live: Option<i64> = conn
                        .query_row("SELECT id FROM records WHERE id = ?1 AND collection = ?2 AND published IS NOT NULL", params![rid, target], |r| r.get(0))
                        .optional()
                        .map_err(fail)?;
                    if live.is_none() {
                        problems.push(format!(
                            "record:{id} 的字段“{}”指向 record:{rid}，而它不是集合“{target}”里一条线上的记录（还没发布、正在被删除，或属于别的集合）：请把两者一起发布，或先改掉这个引用。",
                            f.name
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Publishes pending changes in one transaction: all of the actor's, or only the listed ones.
/// Nothing goes live unless the whole release passes verification.
pub fn publish(conn: &Connection, actor: &Actor, only: Option<&[Target]>, note: &str) -> Result<Release, String> {
    let all = pending_targets(conn)?;
    if all.is_empty() {
        return Err("没有待发布的改动：先修改内容或结构，再来发布。".into());
    }
    // A record draft inside a collection that is deleted, or about to be, cannot go live.
    let stranded = |t: &Target| -> Result<Option<String>, String> {
        let Target::Record(id) = t else { return Ok(None) };
        let collection: Option<String> =
            conn.query_row("SELECT collection FROM records WHERE id = ?1", [id], |r| r.get(0)).optional().map_err(|e| db(t, e))?;
        Ok(match collection {
            Some(c) if collection_gone(conn, &c)? => Some(c),
            _ => None,
        })
    };
    let mut skipped = vec![];
    let chosen: Vec<Target> = match only {
        Some([]) => return Err("targets 是空的：请列出要发布的对象，或去掉 targets 来发布全部。".into()),
        Some(list) => {
            for t in list {
                if !all.contains(t) {
                    return Err(format!("“{t}”没有待发布的改动：请先用 list_pending 查看有哪些。"));
                }
                if !may_publish(actor, t) {
                    let own = serde_json::to_value(own_scope(t)).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
                    return Err(format!("“{}”没有权限发布“{t}”：需要 {own} 和 publish 权限。", actor.name));
                }
                if let Some(c) = stranded(t)? {
                    return Err(format!("“{t}”所属的 collection:{c} 已删除或正等待删除，它的草稿上不了线：请用 discard_draft 放弃它，或先把集合恢复。"));
                }
            }
            all.iter().filter(|t| list.contains(t)).cloned().collect()
        }
        None => {
            let mut mine = vec![];
            for t in all {
                if may_publish(actor, &t) && stranded(&t)?.is_none() {
                    mine.push(t);
                } else {
                    skipped.push(t.to_string());
                }
            }
            mine
        }
    };
    if chosen.is_empty() {
        return Err(format!(
            "没有可以发布的改动，这些都被跳过了：{}。原因是“{}”没有对应权限（发布内容需要 write 和 publish，发布结构需要 structure 和 publish），或者它们所属的集合已删除。",
            skipped.join("、"),
            actor.name
        ));
    }

    let fail = |e: rusqlite::Error| format!("发布失败：{e}");
    let tx = conn.unchecked_transaction().map_err(fail)?;
    let mut items = vec![];
    for target in chosen {
        let s = slot(&tx, &target)?.ok_or_else(|| missing(&target))?;
        items.push((target, s));
    }
    items.sort_by_key(|(t, s)| rank(t, s.draft_deleted));

    let mut problems: Vec<String> = vec![];
    let mut touched: Vec<String> = vec![];
    let mut released: Vec<(i64, String)> = vec![];
    // (record id, live copy before the rewrite, collection whose definition change caused it)
    let mut rewritten: Vec<(i64, String, String)> = vec![];
    for (target, s) in &items {
        let (table, key, val, has_dd) = target.row();
        let clear = if has_dd { ", draft_deleted = 0" } else { "" };
        let clear = format!("draft = NULL, draft_actor = NULL, draft_note = NULL{clear}");
        match target {
            Target::Collection(name) if !s.draft_deleted => {
                let new = parse_def(name, s.draft.as_deref().unwrap_or_default())?;
                if let Some(old) = &s.published {
                    let changed = migrate_records(&tx, name, &parse_def(name, old)?, &new, &mut problems)?;
                    rewritten.extend(changed.into_iter().map(|(id, before)| (id, before, name.clone())));
                }
                tx.execute(&format!("UPDATE collections SET published = draft, deleted = 0, {clear} WHERE name = ?1"), [name]).map_err(fail)?;
                touched.push(name.clone());
            }
            Target::Collection(name) => {
                tx.execute(&format!("UPDATE collections SET deleted = 1, {clear} WHERE name = ?1"), [name]).map_err(fail)?;
            }
            Target::Record(id) => {
                let collection: String = tx.query_row("SELECT collection FROM records WHERE id = ?1", [id], |r| r.get(0)).map_err(fail)?;
                let copy = if s.draft_deleted { "published = NULL" } else { "published = draft" };
                tx.execute(&format!("UPDATE records SET {copy}, {clear}, updated_at = datetime('now') WHERE id = ?1"), [id]).map_err(fail)?;
                if !s.draft_deleted {
                    released.push((*id, collection.clone()));
                }
                touched.push(collection);
            }
            Target::View(_) | Target::Notes => {
                let copy = if s.draft_deleted { "published = NULL" } else { "published = draft" };
                tx.execute(&format!("UPDATE {table} SET {copy}, {clear} WHERE {key} = ?1"), [val]).map_err(fail)?;
            }
        }
    }
    touched.sort();
    touched.dedup();
    verify(&tx, &touched, &released, &mut problems)?;
    check_relations(&tx, &touched, &mut problems)?;
    if !problems.is_empty() {
        let mut seen = std::collections::HashSet::new();
        problems.retain(|p| seen.insert(p.clone()));
        let more = if problems.len() > 10 { format!("（另有 {} 个问题未列出）", problems.len() - 10) } else { String::new() };
        problems.truncate(10);
        // Dropping the transaction rolls everything back.
        return Err(format!("发布被拦下，什么都没有上线。{}{more}", problems.join("；")));
    }

    tx.execute("INSERT INTO releases(actor, note) VALUES(?1, ?2)", params![actor.name, note]).map_err(fail)?;
    let release = tx.last_insert_rowid();
    let version = "INSERT INTO versions(release_id, target, doc, prev, actor, note) VALUES(?1, ?2, ?3, ?4, ?5, ?6)";
    // History stores what actually went live: read it back, since a definition change in the same release
    // may have rewritten a document after its draft was read.
    let live_copy = |target: &Target| -> Result<Option<String>, String> {
        let (table, key, val, _) = target.row();
        tx.query_row(&format!("SELECT published FROM {table} WHERE {key} = ?1"), [val], |r| r.get(0)).map_err(fail)
    };
    let mut published = vec![];
    for (target, s) in items {
        let doc = if s.draft_deleted { None } else { live_copy(&target)? };
        tx.execute(version, params![release, target.to_string(), doc, s.published, s.actor.unwrap_or_else(|| actor.name.clone()), s.note.unwrap_or_else(|| note.to_string())])
            .map_err(fail)?;
        published.push(target.to_string());
    }
    // Records rewritten by a definition change, but not themselves part of the release, get a version too.
    for (id, before, collection) in rewritten {
        let target = Target::Record(id);
        if published.contains(&target.to_string()) {
            continue;
        }
        let why = format!("随集合“{collection}”的结构改动自动转换");
        tx.execute(version, params![release, target.to_string(), live_copy(&target)?, before, actor.name, why]).map_err(fail)?;
    }
    tx.commit().map_err(fail)?;
    Ok(Release { id: release, published, skipped })
}

#[derive(Debug, serde::Serialize)]
pub struct Version {
    pub id: i64,
    pub release_id: i64,
    pub actor: String,
    pub note: String,
    pub created_at: String,
    /// True when this version is the object's deletion; it has no content to restore.
    pub deleted: bool,
}

/// An object's publish history, newest first, at most 30.
pub fn versions(conn: &Connection, target: &Target) -> Result<Vec<Version>, String> {
    let fail = |e: rusqlite::Error| db(target, e);
    let mut stmt = conn
        .prepare(
            "SELECT v.id, v.release_id, v.actor, v.note, r.created_at, v.doc IS NULL FROM versions v \
             JOIN releases r ON r.id = v.release_id WHERE v.target = ?1 ORDER BY v.id DESC LIMIT 30",
        )
        .map_err(fail)?;
    let rows = stmt
        .query_map([target.to_string()], |r| {
            Ok(Version { id: r.get(0)?, release_id: r.get(1)?, actor: r.get(2)?, note: r.get(3)?, created_at: r.get(4)?, deleted: r.get(5)? })
        })
        .map_err(fail)?;
    rows.collect::<Result<_, _>>().map_err(fail)
}

/// Puts a past version's content back into the draft; it goes live only when published.
pub fn restore(conn: &Connection, actor: &Actor, target: &Target, version_id: i64, note: &str) -> Result<(), String> {
    let doc: Option<Option<String>> = conn
        .query_row("SELECT doc FROM versions WHERE id = ?1 AND target = ?2", params![version_id, target.to_string()], |r| r.get(0))
        .optional()
        .map_err(|e| db(target, e))?;
    let Some(doc) = doc else {
        return Err(format!("“{target}”没有编号为 {version_id} 的历史版本：请先用 list_versions 查看。"));
    };
    let Some(doc) = doc else {
        return Err(format!("版本 {version_id} 是一次删除，没有内容可以恢复：请选更早的版本。"));
    };
    let doc: Value = serde_json::from_str(&doc).map_err(|e| format!("版本 {version_id} 的内容已损坏：{e}"))?;
    if let Target::Collection(name) = target {
        // An old definition is a structure change like any other: it must suit the records that exist now.
        let def = parse_def(name, &doc.to_string())?;
        crate::schema::check_def(&def)?;
        return save_definition(conn, actor, &def, note);
    }
    save_draft(conn, actor, target, &doc, note)
}
