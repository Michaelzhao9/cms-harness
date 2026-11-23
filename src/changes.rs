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

pub fn save_draft(conn: &Connection, actor: &Actor, target: &Target, doc: &Value, note: &str) -> Result<(), String> {
    let (table, key, val, has_dd) = target.row();
    let doc = doc.to_string();
    let reset = if has_dd { ", draft_deleted = 0" } else { "" };
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

fn published_is_null(conn: &Connection, target: &Target) -> Result<bool, String> {
    let (table, key, val, _) = target.row();
    conn.query_row(&format!("SELECT published IS NULL FROM {table} WHERE {key} = ?1"), [val], |r| r.get(0))
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => missing(target),
            e => db(target, e),
        })
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
    if published_is_null(&tx, target)? {
        remove_row(&tx, target)?;
    } else {
        let (table, key, val, _) = target.row();
        tx.execute(
            &format!("UPDATE {table} SET draft_deleted = 1, draft_actor = ?1, draft_note = ?2 WHERE {key} = ?3"),
            params![actor.name, note, val],
        )
        .map_err(|e| db(target, e))?;
    }
    tx.commit().map_err(|e| db(target, e))
}

pub fn discard(conn: &Connection, target: &Target) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| db(target, e))?;
    if published_is_null(&tx, target)? && !matches!(target, Target::Notes) {
        remove_row(&tx, target)?;
    } else {
        let (table, key, val, has_dd) = target.row();
        let reset = if has_dd { ", draft_deleted = 0" } else { "" };
        tx.execute(
            &format!("UPDATE {table} SET draft = NULL, draft_actor = NULL, draft_note = NULL{reset} WHERE {key} = ?1"),
            [val],
        )
        .map_err(|e| db(target, e))?;
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

/// The published, non-deleted definition.
pub fn live_def(conn: &Connection, name: &str) -> Result<CollectionDef, String> {
    let r = coll_row_required(conn, name)?;
    match (r.deleted, r.published) {
        (false, Some(p)) => parse_def(name, &p),
        _ => Err(format!("集合“{name}”尚未发布或已删除，线上没有它。")),
    }
}

fn row_status(r: &CollRow) -> &'static str {
    if r.deleted { "deleted" } else { status(r.published.is_some(), r.draft.is_some(), r.draft_deleted) }
}

/// Status of a collection; a row that is gone (a never-published collection was removed) reads as "deleted".
pub fn collection_status(conn: &Connection, name: &str) -> Result<&'static str, String> {
    Ok(coll_row(conn, name)?.map_or("deleted", |r| row_status(&r)))
}

/// Definition (draft if any, else published) and status of a collection; `None` if it is missing or deleted.
pub fn collection_view(conn: &Connection, name: &str) -> Result<Option<(CollectionDef, &'static str)>, String> {
    let Some(r) = coll_row(conn, name)?.filter(|r| !r.deleted) else { return Ok(None) };
    let st = row_status(&r);
    let json = r.draft.or(r.published).ok_or_else(|| format!("集合“{name}”没有定义。"))?;
    Ok(Some((parse_def(name, &json)?, st)))
}
