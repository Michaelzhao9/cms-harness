use rusqlite::Connection;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS admins(id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, password_hash TEXT NOT NULL,
  role TEXT NOT NULL CHECK(role IN ('builder','editor')), prefs TEXT NOT NULL DEFAULT '{}');
CREATE TABLE IF NOT EXISTS api_keys(id INTEGER PRIMARY KEY, name TEXT NOT NULL, key_hash TEXT NOT NULL UNIQUE, scopes TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')), revoked INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS collections(name TEXT PRIMARY KEY, published TEXT, draft TEXT,
  draft_deleted INTEGER NOT NULL DEFAULT 0, deleted INTEGER NOT NULL DEFAULT 0, draft_actor TEXT, draft_note TEXT);
CREATE TABLE IF NOT EXISTS records(id INTEGER PRIMARY KEY AUTOINCREMENT, collection TEXT NOT NULL, published TEXT, draft TEXT,
  draft_deleted INTEGER NOT NULL DEFAULT 0, draft_actor TEXT, draft_note TEXT, updated_at TEXT NOT NULL DEFAULT (datetime('now')));
CREATE INDEX IF NOT EXISTS records_collection ON records(collection);
CREATE TABLE IF NOT EXISTS views(name TEXT PRIMARY KEY, published TEXT, draft TEXT,
  draft_deleted INTEGER NOT NULL DEFAULT 0, draft_actor TEXT, draft_note TEXT);
CREATE TABLE IF NOT EXISTS site_files(path TEXT PRIMARY KEY, published TEXT, draft TEXT,
  draft_deleted INTEGER NOT NULL DEFAULT 0, draft_actor TEXT, draft_note TEXT);
CREATE TABLE IF NOT EXISTS site_settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS options(key TEXT PRIMARY KEY, published TEXT, draft TEXT,
  draft_deleted INTEGER NOT NULL DEFAULT 0, draft_actor TEXT, draft_note TEXT);
CREATE TABLE IF NOT EXISTS members(id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL UNIQUE, password_hash TEXT NOT NULL,
  name TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','disabled')),
  created_at TEXT NOT NULL DEFAULT (datetime('now')), last_login TEXT);
CREATE TABLE IF NOT EXISTS member_sessions(token_hash TEXT PRIMARY KEY, member_id INTEGER NOT NULL REFERENCES members(id) ON DELETE CASCADE,
  expires_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS member_attempts(key TEXT NOT NULL, at TEXT NOT NULL DEFAULT (datetime('now')));
CREATE INDEX IF NOT EXISTS member_attempts_key ON member_attempts(key, at);
CREATE TABLE IF NOT EXISTS notes(id INTEGER PRIMARY KEY CHECK(id = 1), published TEXT, draft TEXT, draft_actor TEXT, draft_note TEXT);
CREATE TABLE IF NOT EXISTS releases(id INTEGER PRIMARY KEY, actor TEXT NOT NULL, note TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')));
CREATE TABLE IF NOT EXISTS versions(id INTEGER PRIMARY KEY, release_id INTEGER NOT NULL REFERENCES releases(id),
  target TEXT NOT NULL, doc TEXT, prev TEXT, actor TEXT NOT NULL, note TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS versions_target ON versions(target, id);
CREATE TABLE IF NOT EXISTS admin_sessions(token_hash TEXT PRIMARY KEY, admin_id INTEGER NOT NULL REFERENCES admins(id) ON DELETE CASCADE,
  csrf TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT (datetime('now')), expires_at TEXT NOT NULL, prefs TEXT NOT NULL DEFAULT '{}');
CREATE TABLE IF NOT EXISTS preview_tokens(token_hash TEXT PRIMARY KEY,
  session TEXT NOT NULL REFERENCES admin_sessions(token_hash) ON DELETE CASCADE, expires_at TEXT NOT NULL, created_at TEXT);
CREATE TABLE IF NOT EXISTS login_attempts(key TEXT NOT NULL, at TEXT NOT NULL DEFAULT (datetime('now')));
CREATE INDEX IF NOT EXISTS login_attempts_key ON login_attempts(key, at);
";

pub fn open(path: &str) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    // In-memory databases cannot use WAL; ignore that failure.
    let _ = conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()));
    conn.execute_batch(SCHEMA)?;
    if !conn.prepare("SELECT 1 FROM pragma_table_info('preview_tokens') WHERE name = 'created_at'")?.exists([])? {
        conn.execute_batch("ALTER TABLE preview_tokens ADD COLUMN created_at TEXT")?;
    }
    // Records added by visitors or members carry their owner and when they came in.
    if !conn.prepare("SELECT 1 FROM pragma_table_info('records') WHERE name = 'owner'")?.exists([])? {
        conn.execute_batch("ALTER TABLE records ADD COLUMN owner INTEGER; ALTER TABLE records ADD COLUMN submitted_at TEXT;")?;
    }
    // Site options used to change the live site at once; they are drafted options now. Keep what was live.
    conn.execute_batch(
        "INSERT OR IGNORE INTO options(key, published) SELECT key, value FROM site_settings WHERE key IN ('import_map', 'script_origins');
         DELETE FROM site_settings WHERE key IN ('import_map', 'script_origins');",
    )?;
    // Databases from before per-admin preferences lack the column.
    if !conn.prepare("SELECT 1 FROM pragma_table_info('admins') WHERE name = 'prefs'")?.exists([])? {
        conn.execute_batch("ALTER TABLE admins ADD COLUMN prefs TEXT NOT NULL DEFAULT '{}'")?;
    }
    // Record pages: each record's slug (live, and a pending change), and the slugs records used to have.
    if !conn.prepare("SELECT 1 FROM pragma_table_info('records') WHERE name = 'slug'")?.exists([])? {
        conn.execute_batch("ALTER TABLE records ADD COLUMN slug TEXT; ALTER TABLE records ADD COLUMN draft_slug TEXT;")?;
    }
    conn.execute_batch(crate::routes::SCHEMA)?;
    // Full-text search: the functions its triggers call, then the index, filled for records published before it.
    crate::search::register(&conn)?;
    conn.execute_batch(crate::search::SCHEMA)?;
    crate::search::backfill(&conn)?;
    conn.execute_batch(crate::schedule::SCHEMA)?;
    conn.execute_batch(crate::websearch::SCHEMA)?;
    conn.execute_batch(crate::cache::SCHEMA)?;
    conn.execute_batch(crate::shield::SCHEMA)?;
    crate::routes::backfill(&conn).map_err(|e| anyhow::anyhow!(e))?;
    Ok(conn)
}
