//! Pages for records. Every collection a visitor may read gets a page per record, as soon as the site has a detail
//! template for it (`<collection>/_item.html`, else the site-wide `_item.html`). Where a record lives comes from
//! its collection's `url` pattern, `/{collection}/{slug}` unless it says otherwise: `{slug}`, `{id}`, a field
//! (`{year}`) or a relation's field (`{category.slug}`).
//!
//! Each record gets a slug when it is created, from its title (Chinese stays Chinese), and keeps it when the
//! title changes, so links do not move. A changed slug is a draft like everything else; once it goes live, the
//! old one is remembered and redirects. Links in rich text are kept as `/_r/<id>` (who, not where), so they follow
//! a record wherever it moves.

use crate::schema::{CollectionDef, FieldType};
use crate::site::Mode;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

/// Tables and triggers; the slug columns are added to `records` by `store::open`.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS url_history(collection TEXT NOT NULL, slug TEXT NOT NULL, record_id INTEGER NOT NULL,
  PRIMARY KEY(collection, slug));
CREATE INDEX IF NOT EXISTS records_slug ON records(collection, slug);
CREATE TRIGGER IF NOT EXISTS records_slug_history AFTER UPDATE OF slug ON records
  WHEN old.slug IS NOT NULL AND new.slug IS NOT old.slug AND old.published IS NOT NULL BEGIN
  INSERT OR REPLACE INTO url_history(collection, slug, record_id) VALUES (old.collection, old.slug, old.id);
END;";

/// A path segment from text: lowercase, letters and digits of any script kept, everything else a single `-`.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(60).collect();
    out.trim_end_matches('-').to_string()
}

/// A slug someone typed: one path segment, letters, digits, `-`, `_` and `.`.
pub fn check_slug(s: &str) -> Result<(), String> {
    let ok = !s.is_empty()
        && s.chars().count() <= 80
        && s.chars().all(|c| c.is_alphanumeric() || "-_.".contains(c))
        && !s.starts_with('.')
        && !s.starts_with('_');
    if ok {
        Ok(())
    } else {
        Err(format!("网址名“{s}”不合法：只能用字母、数字和 - _ .，不能以 . 或 _ 开头，最长 80 个字符。"))
    }
}

/// The text a record's slug is made from: its first single text field.
fn title_of(def: &CollectionDef, doc: &Value) -> String {
    def.fields
        .iter()
        .filter(|f| f.kind == FieldType::Text && !f.multiple)
        .find_map(|f| doc.get(&f.name).and_then(Value::as_str).filter(|s| !s.trim().is_empty()))
        .unwrap_or_default()
        .to_string()
}

fn taken(conn: &Connection, collection: &str, id: i64, slug: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM records WHERE collection = ?1 AND id != ?2 AND (slug = ?3 OR draft_slug = ?3))
         OR EXISTS(SELECT 1 FROM url_history WHERE collection = ?1 AND slug = ?3 AND record_id != ?2)",
        params![collection, id, slug],
        |r| r.get(0),
    )
    .map_err(|e| format!("检查网址名失败：{e}"))
}

/// Gives a new record its slug, from its title (or its number when it has none), made unique in its collection.
pub fn assign(conn: &Connection, def: &CollectionDef, id: i64, doc: &Value) -> Result<String, String> {
    let base = Some(slugify(&title_of(def, doc))).filter(|s| !s.is_empty()).unwrap_or_else(|| id.to_string());
    let mut slug = base.clone();
    let mut n = 2;
    while taken(conn, &def.name, id, &slug)? {
        slug = format!("{base}-{n}");
        n += 1;
    }
    conn.execute("UPDATE records SET slug = ?2 WHERE id = ?1", params![id, slug]).map_err(|e| format!("保存网址名失败：{e}"))?;
    Ok(slug)
}

/// Changes a record's slug: at once while the record has never been live, else as a draft until it is published.
pub fn rename(conn: &Connection, collection: &str, id: i64, slug: &str) -> Result<(), String> {
    check_slug(slug)?;
    if taken(conn, collection, id, slug)? {
        return Err(format!("网址名“{slug}”已经被这个集合里的别的记录用了，请换一个。"));
    }
    let live: bool = conn.query_row("SELECT published IS NOT NULL FROM records WHERE id = ?1", [id], |r| r.get(0)).map_err(|e| e.to_string())?;
    let sql = if live { "UPDATE records SET draft_slug = ?2 WHERE id = ?1" } else { "UPDATE records SET slug = ?2, draft_slug = NULL WHERE id = ?1" };
    conn.execute(sql, params![id, slug]).map_err(|e| format!("保存网址名失败：{e}"))?;
    Ok(())
}

/// Fills in slugs for records made before slugs existed.
pub fn backfill(conn: &Connection) -> Result<(), String> {
    let rows: Vec<(i64, String, Option<String>, Option<String>)> = {
        let mut stmt = conn.prepare("SELECT id, collection, published, draft FROM records WHERE slug IS NULL").map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };
    for (id, collection, published, draft) in rows {
        let Ok(def) = crate::changes::effective_def(conn, &collection) else { continue };
        let doc = published.or(draft).and_then(|d| serde_json::from_str(&d).ok()).unwrap_or(Value::Null);
        assign(conn, &def, id, &doc)?;
    }
    Ok(())
}

/// A collection's URL pattern.
pub fn pattern(def: &CollectionDef) -> String {
    def.url.clone().unwrap_or_else(|| format!("/{}/{{slug}}", def.name))
}

/// A pattern a collection may declare: starts with `/`, plain segments and placeholders, and exactly one of
/// `{slug}` or `{id}` so it names one record.
pub fn check_pattern(def: &CollectionDef, p: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("网址规则“{p}”不合法：{why}"));
    if !p.starts_with('/') || p.len() > 120 || p.contains("//") {
        return bad("要以 / 开头，比如 /news/{slug}。");
    }
    let mut names = 0;
    for seg in p.trim_matches('/').split('/') {
        if let Some(inner) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            let (field, sub) = inner.split_once('.').map_or((inner, None), |(a, b)| (a, Some(b)));
            match (field, sub) {
                ("slug" | "id", None) => names += 1,
                (f, None) if def.field(f).is_some_and(|d| !d.multiple && d.kind != FieldType::Richtext) => {}
                (f, Some("slug" | "id")) if def.field(f).is_some_and(|d| d.kind == FieldType::Relation && !d.multiple) => {}
                _ => return bad(&format!("{{{inner}}} 不是这个集合的单值字段、或关联字段的 .slug。")),
            }
        } else if seg.is_empty() || !seg.chars().all(|c| c.is_alphanumeric() || "-_.".contains(c)) || seg.starts_with('_') {
            return bad("每一段只能用字母、数字和 - _ .，不能以 _ 开头。");
        }
    }
    if names != 1 {
        return bad("要有且只有一个 {slug} 或 {id}，用来找到是哪一条记录。");
    }
    if ["admin", "api", "uploads", "_hc", "_r", "account", "mcp"].contains(&p.trim_start_matches('/').split('/').next().unwrap_or_default()) {
        return bad("admin、api、uploads、account、mcp 开头的网址留给系统。");
    }
    Ok(())
}

/// The detail template for a collection, if the site has one in this mode.
pub fn template(conn: &Connection, mode: Mode, collection: &str) -> Option<String> {
    [format!("{collection}/_item.html"), "_item.html".to_string()]
        .into_iter()
        .find(|p| crate::site::file(conn, mode, p).ok().flatten().is_some())
}

fn slug_of(conn: &Connection, id: i64, mode: Mode) -> Option<String> {
    let sql = match mode {
        Mode::Live => "SELECT slug FROM records WHERE id = ?1",
        Mode::Preview => "SELECT coalesce(draft_slug, slug) FROM records WHERE id = ?1",
    };
    conn.query_row(sql, [id], |r| r.get(0)).optional().ok().flatten().flatten()
}

fn doc_of(conn: &Connection, id: i64, mode: Mode) -> Option<Value> {
    let sql = match mode {
        Mode::Live => "SELECT published FROM records WHERE id = ?1",
        Mode::Preview => "SELECT CASE WHEN draft_deleted THEN NULL ELSE coalesce(draft, published) END FROM records WHERE id = ?1",
    };
    let doc: Option<String> = conn.query_row(sql, [id], |r| r.get(0)).optional().ok().flatten().flatten();
    doc.and_then(|d| serde_json::from_str(&d).ok())
}

fn segment(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(slugify(s)).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Where a record lives in this mode, or None when its collection has no detail template (or a placeholder has no
/// value). `doc` is the record as the caller already has it.
pub fn url_for(conn: &Connection, mode: Mode, def: &CollectionDef, id: i64, doc: &Value) -> Option<String> {
    url_for_in(conn, mode, def, id, doc, None)
}

/// A record's slug in a language: its own translated one, else the default language's.
fn slug_in(conn: &Connection, id: i64, mode: Mode, doc: &Value, lang: Option<&crate::langs::Current>) -> Option<String> {
    lang.filter(|l| !l.is_default())
        .and_then(|l| doc[crate::langs::KEY][&l.code]["_slug"].as_str().map(String::from))
        .or_else(|| slug_of(conn, id, mode))
}

/// `url_for` in a language: the language's prefix and slug; None when the record is not in that language.
pub fn url_for_in(conn: &Connection, mode: Mode, def: &CollectionDef, id: i64, doc: &Value, lang: Option<&crate::langs::Current>) -> Option<String> {
    template(conn, mode, &def.name)?;
    if let Some(l) = lang.filter(|l| !l.is_default()) {
        if !crate::langs::visible(def, doc, &l.code, &l.default) {
            return None;
        }
    }
    let mut out = lang.map(|l| l.prefix.clone()).unwrap_or_default();
    for seg in pattern(def).trim_matches('/').split('/') {
        out.push('/');
        let Some(inner) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
            out.push_str(seg);
            continue;
        };
        let value = match inner.split_once('.') {
            None if inner == "slug" => slug_in(conn, id, mode, doc, lang)?,
            None if inner == "id" => id.to_string(),
            None => segment(doc.get(inner)?)?,
            Some((rel, "slug")) => {
                let rid = doc.get(rel)?.as_i64()?;
                let rdoc = doc_of(conn, rid, mode).unwrap_or(Value::Null);
                slug_in(conn, rid, mode, &rdoc, lang)?
            }
            Some((rel, _)) => doc.get(rel)?.as_i64()?.to_string(),
        };
        out.push_str(&value);
    }
    if out.is_empty() {
        out.push('/');
    }
    Some(out)
}

/// A record found for a path: which, and the path it should be at (a redirect when the request used another).
pub struct Found {
    pub collection: String,
    pub id: i64,
    pub canonical: String,
    pub template: String,
}

/// Collections and their definitions as `mode` sees them, that have a detail template.
fn routable(conn: &Connection, mode: Mode) -> Vec<(CollectionDef, String)> {
    let names: Vec<String> = conn
        .prepare("SELECT name FROM collections ORDER BY name")
        .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
        .unwrap_or_default();
    names
        .into_iter()
        .filter_map(|n| {
            let def = crate::site::any_def(conn, mode, &n).ok().flatten()?;
            let t = template(conn, mode, &n)?;
            Some((def, t))
        })
        .collect()
}

/// The record a request path names, for a viewer who may read it. `segments` are the decoded path segments.
pub fn resolve(conn: &Connection, mode: Mode, segments: &[String], viewer: crate::access::Viewer) -> Option<Found> {
    resolve_in(conn, mode, segments, viewer, None)
}

/// `resolve` in a language: `segments` without the language prefix, slugs matched in that language.
pub fn resolve_in(conn: &Connection, mode: Mode, segments: &[String], viewer: crate::access::Viewer, lang: Option<&crate::langs::Current>) -> Option<Found> {
    for (def, template) in routable(conn, mode) {
        let pat: Vec<String> = pattern(&def).trim_matches('/').split('/').map(String::from).collect();
        if pat.len() != segments.len() {
            continue;
        }
        let mut key: Option<(&str, &str)> = None;
        let literal_ok = pat.iter().zip(segments).all(|(p, s)| match p.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
            Some(inner @ ("slug" | "id")) => {
                key = Some((inner, s.as_str()));
                true
            }
            Some(_) => true,
            None => p == s,
        });
        let Some((kind, value)) = key.filter(|_| literal_ok) else { continue };
        let Some(id) = find(conn, mode, &def.name, kind, value, lang) else { continue };
        if let Some(found) = found(conn, mode, &def, id, viewer, template, lang) {
            return Some(found);
        }
    }
    None
}

fn find(conn: &Connection, mode: Mode, collection: &str, kind: &str, value: &str, lang: Option<&crate::langs::Current>) -> Option<i64> {
    let visible = match mode {
        Mode::Live => "published IS NOT NULL",
        Mode::Preview => "(published IS NOT NULL OR draft IS NOT NULL) AND draft_deleted = 0",
    };
    let (doc, base) = match mode {
        Mode::Live => ("published", "slug"),
        Mode::Preview => ("coalesce(draft, published)", "coalesce(draft_slug, slug)"),
    };
    // In another language a translated slug wins; records without one keep the default language's.
    let by = match (kind, lang.filter(|l| !l.is_default())) {
        ("id", _) => "id = ?2".to_string(),
        (_, None) => format!("{base} = ?2"),
        (_, Some(l)) => {
            let path = format!("$._i18n.\"{}\"._slug", l.code);
            format!("(json_extract({doc}, '{path}') = ?2 OR (json_extract({doc}, '{path}') IS NULL AND {base} = ?2))")
        }
    };
    let id: Option<i64> = conn
        .query_row(&format!("SELECT id FROM records WHERE collection = ?1 AND {by} AND {visible}"), params![collection, value], |r| r.get(0))
        .optional()
        .ok()
        .flatten();
    // In another language, the default language's slug still finds the record (and redirects to its own address).
    let id = id.or_else(|| {
        lang.filter(|l| !l.is_default() && kind == "slug")
            .and_then(|_| conn.query_row(&format!("SELECT id FROM records WHERE collection = ?1 AND {base} = ?2 AND {visible}"), params![collection, value], |r| r.get(0)).optional().ok().flatten())
    });
    id.or_else(|| {
        // A slug this record used to have.
        (kind == "slug")
            .then(|| conn.query_row("SELECT record_id FROM url_history WHERE collection = ?1 AND slug = ?2", params![collection, value], |r| r.get(0)).optional().ok().flatten())
            .flatten()
    })
}

fn found(conn: &Connection, mode: Mode, def: &CollectionDef, id: i64, viewer: crate::access::Viewer, template: String, lang: Option<&crate::langs::Current>) -> Option<Found> {
    let (_, owner) = crate::site::visible_def(conn, mode, &def.name, viewer).ok().flatten()?;
    if let Some(me) = owner {
        let theirs: Option<i64> = conn.query_row("SELECT owner FROM records WHERE id = ?1", [id], |r| r.get(0)).ok().flatten();
        if theirs != Some(me) {
            return None;
        }
    }
    let doc = doc_of(conn, id, mode)?;
    let canonical = url_for_in(conn, mode, def, id, &doc, lang)?;
    Some(Found { collection: def.name.clone(), id, canonical, template })
}

/// Where `/_r/<id>` points: the record's page, for a viewer who may read it.
pub fn by_id(conn: &Connection, mode: Mode, id: i64, viewer: crate::access::Viewer) -> Option<Found> {
    by_id_in(conn, mode, id, viewer, None)
}

pub fn by_id_in(conn: &Connection, mode: Mode, id: i64, viewer: crate::access::Viewer, lang: Option<&crate::langs::Current>) -> Option<Found> {
    let collection: String = conn.query_row("SELECT collection FROM records WHERE id = ?1", [id], |r| r.get(0)).ok()?;
    let def = crate::site::any_def(conn, mode, &collection).ok().flatten()?;
    let template = template(conn, mode, &collection)?;
    found(conn, mode, &def, id, viewer, template, lang)
}

/// Own-site links in rich text become `/_r/<id>` when they point at a record's page, so they follow the record.
/// `origin` is the site's own origin; links to it count as own links.
pub fn link_refs(conn: &Connection, origin: &str, html: &str) -> String {
    if !html.contains("href=") {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find("href=\"") {
        let (head, tail) = rest.split_at(at + 6);
        out.push_str(head);
        let Some(end) = tail.find('"') else {
            rest = tail;
            break;
        };
        let href = &tail[..end];
        let path = href.strip_prefix(origin).filter(|p| p.starts_with('/')).unwrap_or(href);
        let replaced = path
            .starts_with('/')
            .then(|| {
                let (p, _) = path.split_once(['?', '#']).unwrap_or((path, ""));
                let segments: Vec<String> = p.trim_matches('/').split('/').map(decode).collect();
                // A link into another language of this site: its prefix is not part of the record's address.
                let langs = crate::langs::load(conn, true);
                let lang = langs.as_ref().and_then(|l| {
                    let first = segments.first()?;
                    l.get(first).filter(|x| !l.is_default(&x.code)).map(|x| crate::langs::Current::new(l, &x.code, false))
                });
                let rest = if lang.is_some() { &segments[1..] } else { &segments[..] };
                resolve_in(conn, Mode::Preview, rest, crate::access::Viewer::Key, lang.as_ref()).map(|f| format!("/_r/{}", f.id))
            })
            .flatten();
        out.push_str(replaced.as_deref().unwrap_or(href));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// `/_r/<id>` links in rendered HTML replaced by where each record is now; ones that cannot be shown stay.
pub fn expand_refs(conn: &Connection, mode: Mode, html: &str, viewer: crate::access::Viewer) -> String {
    expand_refs_in(conn, mode, html, viewer, None)
}

pub fn expand_refs_in(conn: &Connection, mode: Mode, html: &str, viewer: crate::access::Viewer, lang: Option<&crate::langs::Current>) -> String {
    if !html.contains("/_r/") {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find("\"/_r/") {
        let (head, tail) = rest.split_at(at + 1);
        out.push_str(head);
        let digits: String = tail[4..].chars().take_while(char::is_ascii_digit).collect();
        let after = &tail[4 + digits.len()..];
        match digits.parse::<i64>().ok().filter(|_| after.starts_with('"')).and_then(|id| by_id_in(conn, mode, id, viewer, lang)) {
            Some(f) => {
                out.push_str(&f.canonical);
                rest = after;
            }
            None => {
                out.push_str("/_r/");
                rest = &tail[4..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A percent-decoded path segment.
pub fn decode(seg: &str) -> String {
    percent_encoding::percent_decode_str(seg).decode_utf8_lossy().into_owned()
}

/// A path with each segment percent-encoded, for places that need plain ASCII (the sitemap).
pub fn encode(path: &str) -> String {
    const SEG: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');
    path.split('/').map(|s| percent_encoding::utf8_percent_encode(s, SEG).to_string()).collect::<Vec<_>>().join("/")
}

/// Every live record page a visitor may read without signing in: (path, last change).
pub fn public_pages(conn: &Connection) -> Vec<(String, Option<String>)> {
    let mut out = vec![];
    for (def, _) in routable(conn, Mode::Live) {
        if !def.access.is_public() {
            continue;
        }
        let rows: Vec<(i64, String, Option<String>)> = conn
            .prepare("SELECT id, published, updated_at FROM records WHERE collection = ?1 AND published IS NOT NULL ORDER BY id")
            .and_then(|mut s| s.query_map([&def.name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect())
            .unwrap_or_default();
        for (id, doc, at) in rows {
            let doc = serde_json::from_str(&doc).unwrap_or(Value::Null);
            if let Some(url) = url_for(conn, Mode::Live, &def, id, &doc) {
                out.push((url, at.map(|d| d.chars().take(10).collect())));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("鼓手大赛 2026！"), "鼓手大赛-2026");
        assert_eq!(slugify("  Hello, World -- Again "), "hello-world-again");
        assert_eq!(slugify("!!!"), "");
        assert!(check_slug("a-b_c.1").is_ok());
        assert!(check_slug("a/b").is_err() && check_slug("_x").is_err() && check_slug("").is_err());
        assert_eq!(encode("/news/鼓手"), "/news/%E9%BC%93%E6%89%8B");
    }
}
