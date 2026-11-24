mod common;
use cms_harness::store;

#[test]
fn creates_all_tables_and_is_idempotent() {
    let conn = store::open(":memory:").unwrap();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap()
        .query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(names, ["admin_sessions", "admins", "api_keys", "collections", "login_attempts", "member_attempts", "member_sessions", "members", "notes", "options", "preview_tokens", "records", "releases", "site_files", "site_settings", "versions", "views"]);
    conn.execute_batch(store::SCHEMA).unwrap(); // second run must not fail
}

#[test]
fn record_ids_are_never_reused() {
    let conn = store::open(":memory:").unwrap();
    conn.execute("INSERT INTO records(collection, draft) VALUES('a', '{}')", []).unwrap();
    conn.execute("DELETE FROM records", []).unwrap();
    conn.execute("INSERT INTO records(collection, draft) VALUES('a', '{}')", []).unwrap();
    assert_eq!(conn.last_insert_rowid(), 2);
}

#[test]
fn legacy_site_settings_move_into_published_options() {
    let path = std::env::temp_dir().join(format!("cms-harness-legacy-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE site_settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO site_settings VALUES ('import_map', '{\"imports\":{\"react\":\"https://esm.sh/react@19\"}}');").unwrap();
    }
    let conn = store::open(path.to_str().unwrap()).unwrap();
    let live = cms_harness::changes::setting(&conn, "import_map", false);
    assert_eq!(live["imports"]["react"], "https://esm.sh/react@19");
    let left: i64 = conn.query_row("SELECT count(*) FROM site_settings", [], |r| r.get(0)).unwrap();
    assert_eq!(left, 0);
    drop(conn);
    let _ = std::fs::remove_file(&path);
}
