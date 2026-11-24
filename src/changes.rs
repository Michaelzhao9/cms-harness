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
    /// A site file, by path.
    File(String),
    /// A site option that goes live on publish, like `member_login`.
    Setting(String),
}

/// Site options that are drafted and published like everything else.
pub const SETTINGS: &[(&str, &str)] = &[("member_login", "会员登录"), ("import_map", "网站的 import map"), ("script_origins", "网站允许的脚本来源")];

impl Target {
    pub fn parse(s: &str) -> Result<Target, String> {
        let bad = || format!("对象标识“{s}”不合法：请使用 collection:<名字>、record:<编号>、view:<名字>、file:<路径>、setting:<名字> 或 notes。");
        if s == "notes" {
            return Ok(Target::Notes);
        }
        match s.split_once(':').ok_or_else(bad)? {
            ("collection", n) if crate::schema::valid_name(n) => Ok(Target::Collection(n.into())),
            ("view", n) if crate::schema::valid_name(n) => Ok(Target::View(n.into())),
            ("record", n) => n.parse().map(Target::Record).map_err(|_| bad()),
            ("file", p) if crate::site::check_path(p).is_ok() => Ok(Target::File(p.into())),
            ("setting", k) if SETTINGS.iter().any(|(n, _)| *n == k) => Ok(Target::Setting(k.into())),
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
            Target::File(p) => ("site_files", "path", Sql::Text(p.clone()), true),
            Target::Setting(k) => ("options", "key", Sql::Text(k.clone()), true),
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
            Target::File(p) => write!(f, "file:{p}"),
            Target::Setting(k) => write!(f, "setting:{k}"),
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
    let tx = conn.unchecked_transaction().map_err(|e| db(target, e))?;
    mark_deleted_in(&tx, actor, target, note)?;
    tx.commit().map_err(|e| db(target, e))
}

/// `mark_deleted` inside a transaction the caller holds.
pub fn mark_deleted_in(tx: &Connection, actor: &Actor, target: &Target, note: &str) -> Result<(), String> {
    if matches!(target, Target::Notes) {
        return Err("说明不能删除，请直接修改它的内容。".into());
    }
    let (live, ever) = liveness(tx, target)?;
    if !ever {
        remove_row(tx, target)?;
    } else if !live {
        // A tombstone whose restored draft is being dropped again: back to the tombstone.
        clear_draft(tx, target)?;
    } else {
        if let Target::Collection(name) = target {
            // The deletion replaces any pending definition: record drafts go back to the live definition first,
            // so restoring the collection later starts from a consistent state.
            back_to_live(tx, name, "删除集合")?;
        }
        let (table, key, val, _) = target.row();
        // For a collection the deletion replaces its pending definition, which was just undone above.
        let drop_def = if matches!(target, Target::Collection(_)) { ", draft = NULL" } else { "" };
        tx.execute(
            &format!("UPDATE {table} SET draft_deleted = 1{drop_def}, draft_actor = ?1, draft_note = ?2{} WHERE {key} = ?3", touch(target)),
            params![actor.name, note, val],
        )
        .map_err(|e| db(target, e))?;
    }
    Ok(())
}

pub fn discard(conn: &Connection, target: &Target) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| db(target, e))?;
    if !liveness(&tx, target)?.1 && !matches!(target, Target::Notes) {
        if let Target::Collection(name) = target {
            // Discarding a collection that was never published would wipe its records without the count that
            // delete_collection asks for.
            let count = record_count(&tx, name)?;
            if count > 0 {
                return Err(format!(
                    "集合“{name}”从未发布，放弃它会连同 {count} 条记录一起清除、无法恢复：确认要这样做，请改用 delete_collection 并加上 confirm: {count}。"
                ));
            }
        }
        remove_row(&tx, target)?;
    } else {
        if let Target::Collection(name) = target {
            back_to_live(&tx, name, "放弃集合的草稿")?;
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
    if name == crate::members::MEMBERS {
        return Ok(crate::members::members_def());
    }
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

fn drop_indexes(conn: &Connection, collection: &str, keep: &[String]) -> Result<(), String> {
    let prefix = format!("hc:{collection}:");
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND substr(name, 1, ?1) = ?2").map_err(|e| e.to_string())?;
    let old: Vec<String> = stmt.query_map(params![prefix.len() as i64, prefix], |r| r.get(0)).map_err(|e| e.to_string())?
        .collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    for name in old.into_iter().filter(|n| !keep.contains(n)) {
        conn.execute(&format!("DROP INDEX \"{name}\""), []).map_err(|e| format!("删除索引“{name}”失败：{e}"))?;
    }
    Ok(())
}

/// Makes the field indexes of `def` match its `index` / `unique` fields: one partial expression index per field,
/// on exactly the expression `query` filters by.
pub fn sync_indexes(conn: &Connection, def: &CollectionDef) -> Result<(), String> {
    let mut keep = vec![];
    for f in def.fields.iter().filter(|f| f.index || f.unique) {
        let name = format!("hc:{}:{}", def.name, f.name);
        conn.execute(
            &format!("CREATE INDEX IF NOT EXISTS \"{name}\" ON records(json_extract(published,'$.{}')) WHERE collection = '{}'", f.name, def.name),
            [],
        ).map_err(|e| format!("创建索引“{name}”失败：{e}"))?;
        keep.push(name);
    }
    drop_indexes(conn, &def.name, &keep)
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

/// Rewrites a collection's record drafts from its draft definition back to its live one, so the draft
/// definition can be dropped. Refuses, naming the records, when one cannot go back.
fn back_to_live(conn: &Connection, name: &str, doing: &str) -> Result<(), String> {
    let row = coll_row_required(conn, name)?;
    let (Some(live), Some(draft)) = (&row.published, &row.draft) else { return Ok(()) };
    let stuck = rewrite_drafts(conn, name, &parse_def(name, draft)?, &parse_def(name, live)?)?;
    if stuck.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = stuck.iter().take(5).map(|id| format!("record:{id}")).collect();
    Err(format!(
        "无法{doing}：有 {} 条记录的草稿是按尚未发布的定义写的，退不回线上的定义（例如 {}）。请先把这些记录改成线上定义也接受的值（例如把新定义去掉的必填字段加回来、填好），或者先把集合的定义和这些记录一起发布；实在不要这些改动，再用 discard_draft 放弃这些记录的草稿。",
        stuck.len(),
        ids.join("、")
    ))
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
        .prepare("SELECT id, published, CASE WHEN draft_deleted THEN NULL ELSE draft END FROM records WHERE collection = ?1 AND (published IS NOT NULL OR draft IS NOT NULL) ORDER BY id")
        .map_err(fail)?;
    // The leftover draft of a record that is being deleted is never published, so it is not checked.
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

/// Every object with a draft or a pending deletion, in release order: collections, views, site files, notes, records.
fn pending_targets(conn: &Connection) -> Result<Vec<Target>, String> {
    let mut out: Vec<Target> = vec![];
    let waiting = "draft IS NOT NULL OR draft_deleted = 1";
    out.extend(names(conn, &format!("SELECT name FROM collections WHERE {waiting} ORDER BY name"))?.into_iter().map(Target::Collection));
    out.extend(names(conn, &format!("SELECT name FROM views WHERE {waiting} ORDER BY name"))?.into_iter().map(Target::View));
    out.extend(names(conn, &format!("SELECT path FROM site_files WHERE {waiting} ORDER BY path"))?.into_iter().map(Target::File));
    out.extend(names(conn, &format!("SELECT key FROM options WHERE {waiting} ORDER BY key"))?.into_iter().map(Target::Setting));
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

/// Pending targets as `list_pending` lists them: the preview numbers its change marks by this order.
pub fn pending_order(conn: &Connection) -> Result<Vec<String>, String> {
    Ok(pending_targets(conn)?.iter().map(ToString::to_string).collect())
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
    /// For a text site file: line counts of the live copy and the draft (0 when there is none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<[usize; 2]>,
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
            Target::File(p) => p.clone(),
            Target::Setting(k) => setting_title(k).to_string(),
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
            lines: matches!(&target, Target::File(p) if !crate::site::is_binary(p)).then(|| {
                let count = |v: &Value| v.as_str().map_or(0, |s| s.lines().count());
                [count(&old), if s.draft_deleted { 0 } else { count(&new) }]
            }),
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

pub fn may_publish(actor: &Actor, target: &Target) -> bool {
    actor.can(crate::auth::Scope::Publish) && actor.can(own_scope(target))
}

/// Order inside one release: structure first, then content, deletions last.
fn rank(target: &Target, deleting: bool) -> u8 {
    match (target, deleting) {
        (Target::Collection(_), false) => 0,
        (Target::View(_) | Target::File(_) | Target::Setting(_), false) => 1,
        (Target::Notes, _) => 2,
        (Target::Record(_), false) => 3,
        (Target::Record(_), true) => 4,
        (Target::View(_) | Target::File(_) | Target::Setting(_), true) => 5,
        (Target::Collection(_), true) => 6,
    }
}

/// Rewrites every record of a collection (both copies) from the old definition to the new one.
/// Returns the records whose live copy changed, with the copy they had before.
fn migrate_records(conn: &Connection, name: &str, old: &CollectionDef, new: &CollectionDef, doomed: &[i64], problems: &mut Vec<String>) -> Result<Vec<(i64, String)>, String> {
    let fail = |e: rusqlite::Error| format!("改写集合“{name}”的记录失败：{e}");
    let mut stmt = conn
        .prepare("SELECT id, published, draft, draft_deleted FROM records WHERE collection = ?1 AND (published IS NOT NULL OR draft IS NOT NULL)")
        .map_err(fail)?;
    let rows: Vec<(i64, Option<String>, Option<String>, bool)> =
        stmt.query_map([name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
    let mut rewritten = vec![];
    for (id, published, _draft, _deleting) in rows {
        // Drafts already follow the definition being published (it was in force when they were written), so
        // converting them again would apply a conversion twice. Only live copies move here.
        for (column, json) in [("published", published)] {
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
                // The live copy of a record this release deletes must not hold up the release.
                Err(_) if doomed.contains(&id) => {}
                Err(e) => problems.push(format!("record:{id}：{e}")),
            }
        }
    }
    Ok(rewritten)
}

/// Checks every live record of the touched collections against its live definition.
// ponytail: re-validates every live record of each touched collection on each publish (about 0.14 s per 3,000
// records in release builds, under the global lock); make it incremental if collections reach ~20k records.
fn verify(conn: &Connection, touched: &[String], released: &[(i64, String)], problems: &mut Vec<String>) -> Result<(), String> {
    // A record draft follows its collection's pending definition; if that definition is not in the release,
    // say so, since the record itself may not have been edited by anyone.
    let pending_def = |name: &str| -> Result<bool, String> { Ok(coll_row(conn, name)?.is_some_and(|r| r.draft.is_some())) };
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
                let hint = if pending_def(name)? {
                    format!("（这条记录的草稿是按 collection:{name} 尚未发布的定义写的，请把 collection:{name} 一起发布）")
                } else {
                    String::new()
                };
                problems.push(format!("record:{id}：{e}{hint}"));
            }
            if let Err(e) = check_unique(conn, &def, Some(id), &doc, true) {
                problems.push(format!("record:{id}：{e}"));
            }
        }
    }
    Ok(())
}

/// Rebuilds the field indexes of every live collection. Run at startup, so live reads never depend on an
/// index that is missing (for example in a database written by an older version).
pub fn sync_all_indexes(conn: &Connection) -> Result<(), String> {
    for (_, def) in live_defs(conn)? {
        sync_indexes(conn, &def)?;
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

/// After a release, every live view in scope must point at a live collection and fit its live definition.
/// In scope: views released here, and views over a collection touched, published or deleted here.
pub fn check_views(conn: &Connection, released: &[String], collections: &[String], problems: &mut Vec<String>) -> Result<(), String> {
    let fail = |e: rusqlite::Error| format!("检查界面失败：{e}");
    let mut stmt = conn.prepare("SELECT name, published FROM views WHERE published IS NOT NULL ORDER BY name").map_err(fail)?;
    let rows: Vec<(String, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
    for (name, json) in rows {
        let view: crate::schema::ViewDef = serde_json::from_str(&json).map_err(|e| format!("界面“{name}”的定义已损坏：{e}"))?;
        if !released.contains(&name) && !collections.contains(&view.collection) {
            continue;
        }
        let def = if view.collection == crate::members::MEMBERS { Some(crate::members::members_def()) } else { live_def_opt(conn, &view.collection)? };
        match def {
            None => problems.push(format!("view:{name} 引用的 collection:{} 不在线上：请把集合一起发布，或先删除这个界面。", view.collection)),
            Some(def) => {
                if let Err(e) = crate::schema::check_view(&view, &def) {
                    problems.push(format!("view:{name}：{e}"));
                }
            }
        }
    }
    Ok(())
}

/// What a record draft becomes when it goes live. The draft follows the definition in force for drafts; when that is
/// a pending definition outside this release, the draft is brought back to the live definition first. Either way
/// the result is validated and cleaned against the live definition, so what goes live is exactly what the rules allow.
fn live_copy_of(conn: &Connection, id: i64, collection: &str, draft: Option<&str>, problems: &mut Vec<String>) -> Result<Option<String>, String> {
    let Some(draft) = draft else { return Ok(None) };
    let Some(def) = live_def_opt(conn, collection)? else {
        // Not live: verification reports it; keep the draft as it is.
        return Ok(Some(draft.to_string()));
    };
    let doc: Value = serde_json::from_str(draft).map_err(|e| format!("记录 {id} 的内容已损坏：{e}"))?;
    let pending = coll_row(conn, collection)?.and_then(|r| r.draft);
    let hint = |e: String| match &pending {
        Some(_) => format!("record:{id}：{e}（这条记录的草稿是按 collection:{collection} 尚未发布的定义写的，请把 collection:{collection} 一起发布）"),
        None => format!("record:{id}：{e}"),
    };
    let fitted = match &pending {
        Some(p) => crate::schema::migrate(&parse_def(collection, p)?, &def, &doc),
        None => crate::schema::normalize(&def, &doc, &serde_json::json!({})),
    };
    match fitted {
        Ok(v) => Ok(Some(v.to_string())),
        Err(e) => {
            problems.push(hint(e));
            Ok(Some(draft.to_string()))
        }
    }
}

/// Publishes pending changes in one transaction: all of the actor's, or only the listed ones.
/// Nothing goes live unless the whole release passes verification.
pub fn publish(conn: &Connection, actor: &Actor, only: Option<&[Target]>, note: &str) -> Result<Release, String> {
    run_publish(conn, actor, only, note, true)
}

/// Runs `publish` with all its checks inside a transaction that is always rolled back: what would go live,
/// or the problems that would stop it. Nothing is written.
pub fn dry_run_publish(conn: &Connection, actor: &Actor, only: Option<&[Target]>) -> Result<Release, String> {
    run_publish(conn, actor, only, "试运行", false)
}

fn run_publish(conn: &Connection, actor: &Actor, only: Option<&[Target]>, note: &str, commit: bool) -> Result<Release, String> {
    let all = pending_targets(conn)?;
    if all.is_empty() {
        return Err("没有待发布的改动：先修改内容或结构，再来发布。".into());
    }
    // Candidates: the listed targets, or everything the actor may publish.
    let mut no_right: Vec<String> = vec![];
    let mut chosen: Vec<Target> = match only {
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
            }
            all.iter().filter(|t| list.contains(t)).cloned().collect()
        }
        None => {
            let (mine, rest): (Vec<Target>, Vec<Target>) = all.into_iter().partition(|t| may_publish(actor, t));
            no_right = rest.iter().map(Target::to_string).collect();
            mine
        }
    };
    // A record draft cannot go live into a collection that will not be live once this release is applied:
    // one that is deleted and not being restored here, or one whose deletion is part of this release.
    // Record deletions are never held back.
    let mut stranded: Vec<(Target, String)> = vec![];
    for t in &chosen {
        let Target::Record(id) = t else { continue };
        let row: Option<(String, bool, Option<String>)> = conn
            .query_row("SELECT collection, draft_deleted, draft FROM records WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()
            .map_err(|e| db(t, e))?;
        let Some((collection, deleting, draft)) = row else { continue };
        let Some(c) = coll_row(conn, &collection)? else { continue };
        let in_release = chosen.contains(&Target::Collection(collection.clone()));
        let gone_after = if in_release { c.draft_deleted } else { c.deleted };
        let unborn = !in_release && c.published.is_none();
        // On "publish everything", a draft written for a pending definition that is not part of this release
        // waits for it instead of blocking everyone else's changes.
        let lagging = only.is_none()
            && !in_release
            && c.draft.is_some()
            && match (&c.published, &draft) {
                (Some(live), Some(d)) => {
                    let doc: Value = serde_json::from_str(d).unwrap_or(Value::Null);
                    let pending = parse_def(&collection, c.draft.as_deref().unwrap_or_default())?;
                    crate::schema::migrate(&pending, &parse_def(&collection, live)?, &doc).is_err()
                }
                _ => false,
            };
        if !deleting && (gone_after || unborn || lagging) {
            stranded.push((t.clone(), collection));
        }
    }
    if only.is_some() {
        if let Some((t, c)) = stranded.first() {
            let unborn = coll_row(conn, c)?.is_some_and(|r| r.published.is_none() && !r.deleted);
            return Err(if unborn {
                format!("“{t}”所属的集合还没有上线：请把 collection:{c} 一起发布。")
            } else {
                format!("“{t}”所属的 collection:{c} 已删除，或正在这次发布里被删除，它的草稿上不了线：请用 discard_draft 放弃它，或先把集合恢复。")
            });
        }
    }
    chosen.retain(|t| !stranded.iter().any(|(s, _)| s == t));
    let held: Vec<String> = stranded.iter().map(|(t, _)| t.to_string()).collect();
    if chosen.is_empty() {
        let mut why = vec![];
        if !no_right.is_empty() {
            why.push(format!("“{}”没有权限发布 {}（发布内容需要 write 和 publish，发布结构需要 structure 和 publish）", actor.name, no_right.join("、")));
        }
        if !held.is_empty() {
            why.push(format!("{} 要等所属集合的定义上线后才能发布（集合已删除、还没发布，或它们按尚未发布的新定义写成）", held.join("、")));
        }
        return Err(format!("没有可以发布的改动：{}。", why.join("；")));
    }
    let skipped: Vec<String> = no_right.into_iter().chain(held).collect();

    let fail = |e: rusqlite::Error| format!("发布失败：{e}");
    let tx = conn.unchecked_transaction().map_err(fail)?;
    let mut items = vec![];
    for target in chosen {
        let s = slot(&tx, &target)?.ok_or_else(|| missing(&target))?;
        items.push((target, s));
    }
    items.sort_by_key(|(t, s)| rank(t, s.draft_deleted));
    let doomed: Vec<i64> = items
        .iter()
        .filter_map(|(t, s)| match t {
            Target::Record(id) if s.draft_deleted => Some(*id),
            _ => None,
        })
        .collect();

    let mut problems: Vec<String> = vec![];
    let mut touched: Vec<String> = vec![];
    let mut released: Vec<(i64, String)> = vec![];
    // collections whose definition went live or was deleted in this release
    let mut defs: Vec<(String, bool)> = vec![];
    let mut views: Vec<String> = vec![];
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
                    let changed = migrate_records(&tx, name, &parse_def(name, old)?, &new, &doomed, &mut problems)?;
                    rewritten.extend(changed.into_iter().map(|(id, before)| (id, before, name.clone())));
                }
                tx.execute(&format!("UPDATE collections SET published = draft, deleted = 0, {clear} WHERE name = ?1"), [name]).map_err(fail)?;
                touched.push(name.clone());
                defs.push((name.clone(), true));
            }
            Target::Collection(name) => {
                defs.push((name.clone(), false));
                tx.execute(&format!("UPDATE collections SET deleted = 1, {clear} WHERE name = ?1"), [name]).map_err(fail)?;
            }
            Target::Record(id) => {
                let collection: String = tx.query_row("SELECT collection FROM records WHERE id = ?1", [id], |r| r.get(0)).map_err(fail)?;
                let live = if s.draft_deleted { None } else { Some(live_copy_of(&tx, *id, &collection, s.draft.as_deref(), &mut problems)?) };
                tx.execute(&format!("UPDATE records SET published = ?2, {clear}, updated_at = datetime('now') WHERE id = ?1"), params![id, live])
                    .map_err(fail)?;
                if !s.draft_deleted {
                    released.push((*id, collection.clone()));
                }
                touched.push(collection);
            }
            Target::View(_) | Target::Notes | Target::File(_) | Target::Setting(_) => {
                if let Target::View(n) = target {
                    views.push(n.clone());
                }
                let copy = if s.draft_deleted { "published = NULL" } else { "published = draft" };
                tx.execute(&format!("UPDATE {table} SET {copy}, {clear} WHERE {key} = ?1"), [val]).map_err(fail)?;
            }
        }
    }
    for (name, live) in &defs {
        match live {
            true => sync_indexes(&tx, &live_def(&tx, name)?)?,
            false => drop_indexes(&tx, name, &[])?,
        }
    }
    touched.sort();
    touched.dedup();
    verify(&tx, &touched, &released, &mut problems)?;
    check_relations(&tx, &touched, &mut problems)?;
    let scope: Vec<String> = touched.iter().cloned().chain(defs.iter().map(|(n, _)| n.clone())).collect();
    check_views(&tx, &views, &scope, &mut problems)?;
    crate::site::check_live(&tx, &mut problems)?;
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
    if commit {
        tx.commit().map_err(fail)?;
    }
    // Without a commit, dropping the transaction rolls everything back.
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
    let doc = match target {
        Target::Record(id) => {
            // An old record must fit the definition drafts follow now, and its HTML is cleaned again.
            let collection: String =
                conn.query_row("SELECT collection FROM records WHERE id = ?1", [id], |r| r.get(0)).map_err(|e| db(target, e))?;
            let def = current_def(conn, &collection)?.ok_or_else(|| format!("记录 {id} 所属的集合“{collection}”没有定义，无法恢复。"))?;
            crate::schema::fit(&def, &doc).map_err(|e| format!("版本 {version_id} 不符合集合“{collection}”现在的定义，没有恢复：{e}"))?
        }
        Target::File(path) => {
            crate::site::check_file(path, doc.as_str().unwrap_or_default())?;
            check_file_count(conn, std::slice::from_ref(path))?;
            doc
        }
        _ => doc,
    };
    save_draft(conn, actor, target, &doc, note)
}

/// A record's published copy brought to the definition drafts follow, for use as the base of a new draft.
pub fn to_current(conn: &Connection, collection: &str, doc: &Value) -> Result<Value, String> {
    let Some(row) = coll_row(conn, collection)? else { return Ok(doc.clone()) };
    match (&row.published, &row.draft) {
        (Some(live), Some(draft)) => crate::schema::migrate(&parse_def(collection, live)?, &parse_def(collection, draft)?, doc),
        _ => Ok(doc.clone()),
    }
}

// ---- site files ----

/// Refuses when writing these paths would leave more than `site::MAX_FILES` files once everything pending is published.
pub fn check_file_count(conn: &Connection, paths: &[String]) -> Result<(), String> {
    let kept = names(conn, "SELECT path FROM site_files WHERE draft_deleted = 0 AND (draft IS NOT NULL OR published IS NOT NULL)")?;
    let mut all: std::collections::HashSet<&str> = kept.iter().map(String::as_str).collect();
    all.extend(paths.iter().map(String::as_str));
    let max = crate::site::MAX_FILES;
    if all.len() > max {
        return Err(format!("站点最多 {max} 个文件，这次写入后会有 {} 个：请先删掉不用的文件。", all.len()));
    }
    Ok(())
}

/// Puts `content` into the draft of a site file, already checked. Content equal to the live copy drops any draft
/// or pending deletion instead, so nothing waits to be published. Returns false when nothing changed.
pub fn stage_file(conn: &Connection, actor: &Actor, path: &str, content: &str, note: &str) -> Result<bool, String> {
    let target = Target::File(path.to_string());
    let doc = Value::String(content.to_string());
    let json = doc.to_string();
    let s = slot(conn, &target)?;
    let (published, draft, deleting) = s.map_or((None, None, false), |s| (s.published, s.draft, s.draft_deleted));
    if published.as_deref() == Some(json.as_str()) {
        let pending = draft.is_some() || deleting;
        if pending {
            clear_draft(conn, &target)?;
        }
        return Ok(pending);
    }
    if !deleting && draft.as_deref() == Some(json.as_str()) {
        return Ok(false);
    }
    save_draft(conn, actor, &target, &doc, note)?;
    Ok(true)
}

pub fn setting_title(key: &str) -> &'static str {
    SETTINGS.iter().find(|(n, _)| *n == key).map_or("设置", |(_, t)| *t)
}

/// A site option: the live value, or with `effective` the draft if there is one (a pending deletion reads as unset).
pub fn setting(conn: &Connection, key: &str, effective: bool) -> Value {
    let row: Option<(Option<String>, Option<String>, bool)> = conn
        .query_row("SELECT published, draft, draft_deleted FROM options WHERE key = ?1", [key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()
        .ok()
        .flatten();
    let Some((published, draft, deleting)) = row else { return Value::Null };
    let json = if !effective { published } else if deleting { None } else { draft.or(published) };
    parse_doc(&json)
}
