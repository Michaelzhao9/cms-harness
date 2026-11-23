use rusqlite::Connection;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS admins(id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, password_hash TEXT NOT NULL,
  role TEXT NOT NULL CHECK(role IN ('builder','editor')));
CREATE TABLE IF NOT EXISTS api_keys(id INTEGER PRIMARY KEY, name TEXT NOT NULL, key_hash TEXT NOT NULL UNIQUE, scopes TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')), revoked INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS collections(name TEXT PRIMARY KEY, published TEXT, draft TEXT,
  draft_deleted INTEGER NOT NULL DEFAULT 0, deleted INTEGER NOT NULL DEFAULT 0, draft_actor TEXT, draft_note TEXT);
CREATE TABLE IF NOT EXISTS records(id INTEGER PRIMARY KEY AUTOINCREMENT, collection TEXT NOT NULL, published TEXT, draft TEXT,
  draft_deleted INTEGER NOT NULL DEFAULT 0, draft_actor TEXT, draft_note TEXT, updated_at TEXT NOT NULL DEFAULT (datetime('now')));
CREATE INDEX IF NOT EXISTS records_collection ON records(collection);
CREATE TABLE IF NOT EXISTS views(name TEXT PRIMARY KEY, published TEXT, draft TEXT,
  draft_deleted INTEGER NOT NULL DEFAULT 0, draft_actor TEXT, draft_note TEXT);
CREATE TABLE IF NOT EXISTS notes(id INTEGER PRIMARY KEY CHECK(id = 1), published TEXT, draft TEXT, draft_actor TEXT, draft_note TEXT);
CREATE TABLE IF NOT EXISTS releases(id INTEGER PRIMARY KEY, actor TEXT NOT NULL, note TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')));
CREATE TABLE IF NOT EXISTS versions(id INTEGER PRIMARY KEY, release_id INTEGER NOT NULL REFERENCES releases(id),
  target TEXT NOT NULL, doc TEXT, prev TEXT, actor TEXT NOT NULL, note TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS versions_target ON versions(target, id);
";

pub fn open(path: &str) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    // In-memory databases cannot use WAL; ignore that failure.
    let _ = conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()));
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}
