//! Full-text search over published records. SQLite's FTS5 holds one row per published record (rowid = record id),
//! kept up to date by triggers on `records`, so every way a record goes live (publish, a visitor's submission, a
//! restore) updates it. FTS5's tokenizer does not split Chinese, so text is turned into terms here first: Chinese,
//! Japanese and Korean runs become single characters and overlapping pairs, other text stays as words.
//! Who may see a hit is decided when searching, by the same read rules as everything else.

use rusqlite::{functions::FunctionFlags, Connection};
use serde_json::Value;
use std::collections::HashMap;

/// The schema, after `register` has added the functions the triggers call.
pub const SCHEMA: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS search USING fts5(terms, body UNINDEXED, tokenize = 'unicode61');
CREATE TRIGGER IF NOT EXISTS records_search_insert AFTER INSERT ON records WHEN new.published IS NOT NULL BEGIN
  INSERT INTO search(rowid, terms, body) VALUES (new.id, hc_terms(hc_text(new.published)), hc_text(new.published));
END;
CREATE TRIGGER IF NOT EXISTS records_search_update AFTER UPDATE OF published ON records BEGIN
  DELETE FROM search WHERE rowid = old.id;
  INSERT INTO search(rowid, terms, body) SELECT new.id, hc_terms(hc_text(new.published)), hc_text(new.published) WHERE new.published IS NOT NULL;
END;
CREATE TRIGGER IF NOT EXISTS records_search_delete AFTER DELETE ON records BEGIN
  DELETE FROM search WHERE rowid = old.id;
END;";

/// Adds `hc_text(json)` and `hc_terms(text)` to a connection; the triggers need them on every connection that writes.
pub fn register(conn: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    conn.create_scalar_function("hc_text", 1, flags, |ctx| Ok(ctx.get::<Option<String>>(0)?.map(|j| text_of(&j))))?;
    conn.create_scalar_function("hc_terms", 1, flags, |ctx| Ok(ctx.get::<Option<String>>(0)?.map(|t| terms(&t, false))))
}

/// Indexes the published records an older database already has.
pub fn backfill(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO search(rowid, terms, body) SELECT id, hc_terms(hc_text(published)), hc_text(published) FROM records
         WHERE published IS NOT NULL AND id NOT IN (SELECT rowid FROM search)",
        [],
    )?;
    Ok(())
}

/// The readable text of a record: its string values, rich text without tags.
pub fn text_of(json: &str) -> String {
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) if !s.starts_with("/uploads/") => out.push(strip_tags(s)),
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(o) => o.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = vec![];
    if let Ok(v) = serde_json::from_str::<Value>(json) {
        walk(&v, &mut out);
    }
    out.retain(|s| !s.trim().is_empty());
    out.join(" \n ")
}

fn strip_tags(s: &str) -> String {
    if !s.contains('<') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{ac00}'..='\u{d7af}')
}

/// Space-separated terms. For the index (`query = false`) a Chinese run gives every character and every pair; for a
/// query, a run of two or more characters gives only its pairs, so "陶瓷杯" must match "陶瓷" and "瓷杯" together.
pub fn terms(text: &str, query: bool) -> String {
    let mut out: Vec<String> = vec![];
    let mut word = String::new();
    let mut run: Vec<char> = vec![];
    let flush_run = |run: &mut Vec<char>, out: &mut Vec<String>| {
        if run.len() == 1 || !query {
            out.extend(run.iter().map(|c| c.to_string()));
        }
        out.extend(run.windows(2).map(|w| w.iter().collect::<String>()));
        run.clear();
    };
    for c in text.chars().flat_map(char::to_lowercase) {
        if is_cjk(c) {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            run.push(c);
        } else if c.is_alphanumeric() {
            flush_run(&mut run, &mut out);
            word.push(c);
        } else {
            flush_run(&mut run, &mut out);
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
        }
    }
    flush_run(&mut run, &mut out);
    if !word.is_empty() {
        out.push(word);
    }
    out.join(" ")
}

/// The FTS5 query for what a visitor typed: every term must appear; the last word also matches as a prefix.
fn match_expr(q: &str) -> Option<String> {
    let t = terms(q, true);
    let parts: Vec<&str> = t.split(' ').filter(|s| !s.is_empty()).take(32).collect();
    let last = parts.len().checked_sub(1)?;
    let ends_in_word = q.trim_end().chars().last().is_some_and(|c| c.is_alphanumeric() && !is_cjk(c));
    Some(
        parts
            .iter()
            .enumerate()
            .map(|(i, p)| if i == last && ends_in_word { format!("\"{p}\"*") } else { format!("\"{p}\"") })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// A hit: the record, its collection, and a short piece of text around the first match.
#[derive(Debug, Clone)]
pub struct Hit {
    pub id: i64,
    pub collection: String,
    pub snippet: String,
}

/// The published records matching `q` that `viewer` may read, best first. `only` limits it to one collection.
pub fn search(conn: &Connection, q: &str, only: Option<&str>, viewer: crate::access::Viewer, limit: usize) -> Result<Vec<Hit>, String> {
    let Some(expr) = match_expr(q) else { return Ok(vec![]) };
    let fail = |e: rusqlite::Error| format!("搜索失败：{e}");
    // ponytail: reads up to 500 hits and filters by access in Rust; push the rules into SQL if sites get that big.
    let mut stmt = conn
        .prepare(
            "SELECT r.id, r.collection, r.owner, s.body FROM search s JOIN records r ON r.id = s.rowid
             WHERE search MATCH ?1 AND (?2 IS NULL OR r.collection = ?2) ORDER BY bm25(search) LIMIT 500",
        )
        .map_err(fail)?;
    let rows = stmt
        .query_map(rusqlite::params![expr, only], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, String>(3)?)))
        .map_err(fail)?;
    // Each collection's rule once: None when this viewer may not read it, Some(owner filter) otherwise.
    let mut rules: HashMap<String, Option<Option<i64>>> = HashMap::new();
    let mut out = vec![];
    for row in rows {
        let (id, collection, owner, body) = row.map_err(fail)?;
        let rule = match rules.get(&collection) {
            Some(r) => *r,
            None => {
                let r = crate::site::visible_def(conn, crate::site::Mode::Live, &collection, viewer)?.map(|(_, owner)| owner);
                rules.insert(collection.clone(), r);
                r
            }
        };
        match rule {
            Some(None) => {}
            Some(Some(me)) if owner == Some(me) => {}
            _ => continue,
        }
        out.push(Hit { id, snippet: snippet(&body, q), collection });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// About 100 characters of `body` around the first place a word of `q` appears.
pub fn snippet(body: &str, q: &str) -> String {
    let lower: Vec<char> = body.chars().flat_map(char::to_lowercase).collect();
    let chars: Vec<char> = body.chars().collect();
    // Lowercasing can change the length of a few characters; fall back to the start then.
    let same = lower.len() == chars.len();
    let needles: Vec<Vec<char>> = q
        .split_whitespace()
        .map(|w| w.chars().flat_map(char::to_lowercase).take(2).collect::<Vec<_>>())
        .filter(|w| !w.is_empty())
        .collect();
    let at = if same { needles.iter().filter_map(|n| lower.windows(n.len()).position(|w| w == n.as_slice())).min().unwrap_or(0) } else { 0 };
    let start = at.saturating_sub(30);
    let end = (start + 100).min(chars.len());
    let mut s: String = chars[start..end].iter().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        s.insert(0, '…');
    }
    if end < chars.len() {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_and_words() {
        assert_eq!(terms("陶瓷杯 Big-Cup", false), "陶 瓷 杯 陶瓷 瓷杯 big cup");
        assert_eq!(terms("陶瓷杯", true), "陶瓷 瓷杯");
        assert_eq!(terms("杯", true), "杯");
        assert_eq!(match_expr("陶瓷 cu"), Some("\"陶瓷\" \"cu\"*".into()));
        assert_eq!(match_expr("  "), None);
        assert_eq!(text_of(r#"{"t":"<p>你好&amp;<b>世界</b></p>","img":"/uploads/a.png","n":3}"#).trim(), "你好& 世界");
        assert_eq!(snippet("一二三四五 陶瓷 六七", "陶瓷"), "一二三四五 陶瓷 六七");
    }
}
