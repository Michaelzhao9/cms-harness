//! Rendered live pages, kept until something they used changes. A render records what it read: site files,
//! collections, the address rules. Triggers bump a counter for each of those whenever its published state changes
//! (a publish, a scheduled publish, a visitor's submission, a restore); a cached page is served only while every
//! counter it depends on is unchanged. So a dynamic site answers at the speed of a static one, and a publish
//! recomputes exactly the pages it touched, nothing more.

use rusqlite::{params_from_iter, Connection};
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS epochs(key TEXT PRIMARY KEY, n INTEGER NOT NULL);
CREATE TRIGGER IF NOT EXISTS epochs_records_insert AFTER INSERT ON records WHEN new.published IS NOT NULL BEGIN
  INSERT INTO epochs VALUES ('c:' || new.collection, 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
  INSERT INTO epochs VALUES ('records', 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
END;
CREATE TRIGGER IF NOT EXISTS epochs_records_update AFTER UPDATE OF published, slug ON records
  WHEN new.published IS NOT NULL OR old.published IS NOT NULL BEGIN
  INSERT INTO epochs VALUES ('c:' || new.collection, 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
  INSERT INTO epochs VALUES ('records', 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
END;
CREATE TRIGGER IF NOT EXISTS epochs_records_delete AFTER DELETE ON records BEGIN
  INSERT INTO epochs VALUES ('c:' || old.collection, 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
  INSERT INTO epochs VALUES ('records', 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
END;
CREATE TRIGGER IF NOT EXISTS epochs_collections AFTER UPDATE OF published, deleted ON collections BEGIN
  INSERT INTO epochs VALUES ('c:' || new.name, 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
  INSERT INTO epochs VALUES ('routes', 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
END;
CREATE TRIGGER IF NOT EXISTS epochs_files_insert AFTER INSERT ON site_files WHEN new.published IS NOT NULL BEGIN
  INSERT INTO epochs VALUES ('f:' || new.path, 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
  INSERT INTO epochs VALUES ('routes', 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
END;
CREATE TRIGGER IF NOT EXISTS epochs_files_update AFTER UPDATE OF published ON site_files BEGIN
  INSERT INTO epochs VALUES ('f:' || new.path, 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
  INSERT INTO epochs SELECT 'routes', 1 WHERE new.path LIKE '%_item.html' OR (old.published IS NULL) != (new.published IS NULL)
    ON CONFLICT(key) DO UPDATE SET n = n + 1;
END;
CREATE TRIGGER IF NOT EXISTS epochs_files_delete AFTER DELETE ON site_files BEGIN
  INSERT INTO epochs VALUES ('f:' || old.path, 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
  INSERT INTO epochs VALUES ('routes', 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
END;
CREATE TRIGGER IF NOT EXISTS epochs_options AFTER UPDATE OF published ON options BEGIN
  INSERT INTO epochs VALUES ('settings', 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
END;
CREATE TRIGGER IF NOT EXISTS epochs_options_insert AFTER INSERT ON options WHEN new.published IS NOT NULL BEGIN
  INSERT INTO epochs VALUES ('settings', 1) ON CONFLICT(key) DO UPDATE SET n = n + 1;
END;";

/// What every page depends on, besides what it read: the site's settings and its address rules.
pub const ALWAYS: [&str; 2] = ["settings", "routes"];

/// One cached response.
#[derive(Clone)]
pub struct Entry {
    deps: Vec<(String, i64)>,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// The cache of live pages. ponytail: a plain map cleared whole when it passes `LIMIT` entries; an LRU if big
/// sites churn through it.
#[derive(Default)]
pub struct Cache {
    pages: Mutex<HashMap<String, Entry>>,
}

const LIMIT: usize = 5000;

fn counters(conn: &Connection, keys: &[String]) -> HashMap<String, i64> {
    if keys.is_empty() {
        return HashMap::new();
    }
    let marks = vec!["?"; keys.len()].join(",");
    conn.prepare(&format!("SELECT key, n FROM epochs WHERE key IN ({marks})"))
        .and_then(|mut s| s.query_map(params_from_iter(keys), |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
        .unwrap_or_default()
}

impl Cache {
    /// The cached response for `key`, when nothing it depends on has changed since.
    pub fn get(&self, conn: &Connection, key: &str) -> Option<Entry> {
        let entry = self.pages.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned()?;
        let keys: Vec<String> = entry.deps.iter().map(|(k, _)| k.clone()).collect();
        let now = counters(conn, &keys);
        entry.deps.iter().all(|(k, n)| now.get(k).copied().unwrap_or(0) == *n).then_some(entry)
    }

    /// Keeps a response with the counters of what it read, as they are now.
    pub fn put(&self, conn: &Connection, key: String, deps: BTreeSet<String>, status: u16, headers: Vec<(String, String)>, body: Vec<u8>) {
        let keys: Vec<String> = deps.into_iter().chain(ALWAYS.iter().map(|s| s.to_string())).collect::<BTreeSet<_>>().into_iter().collect();
        let now = counters(conn, &keys);
        let deps = keys.into_iter().map(|k| { let n = now.get(&k).copied().unwrap_or(0); (k, n) }).collect();
        let mut pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        if pages.len() >= LIMIT {
            pages.clear();
        }
        pages.insert(key, Entry { deps, status, headers, body });
    }

    /// Forgets everything (after a restore swaps the database under it).
    pub fn clear(&self) {
        self.pages.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}
