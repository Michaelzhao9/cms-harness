mod common;
use harness_cms::store;

#[test]
fn creates_all_tables_and_is_idempotent() {
    let conn = store::open(":memory:").unwrap();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap()
        .query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(names, ["admin_sessions", "admins", "api_keys", "collections", "login_attempts", "notes", "records", "releases", "site_files", "versions", "views"]);
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
