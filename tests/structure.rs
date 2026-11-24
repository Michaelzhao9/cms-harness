mod common;
use common::*;
use serde_json::json;

fn seed(app: &cms_harness::Shared) -> Vec<i64> {
    news(app);
    let ids = ["a", "b", "c"].iter().map(|s| ok(app, &editor(), "create_record", json!({ "collection": "news", "note": "n",
        "data": { "title": format!("标题{s}"), "slug": s, "date": "2026-11-01" } }))["id"].as_i64().unwrap()).collect();
    publish_all(app);
    ids
}

#[test]
fn adding_a_field_keeps_existing_data() {
    let app = app();
    let ids = seed(&app);
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "note": "n",
        "add_fields": [{ "name": "views", "title": "浏览", "type": "number", "default": 0 }] }));
    publish_all(&app);
    let got = ok(&app, &reader(), "get_record", json!({ "id": ids[0] }));
    assert_eq!((got["data"]["title"].as_str(), got["data"]["views"].as_i64()), (Some("标题a"), Some(0)));
}

#[test]
fn tightening_is_rejected_with_counts() {
    let app = app();
    seed(&app);
    let b = builder();
    let e = err(&app, &b, "update_collection", json!({ "name": "news", "note": "n",
        "add_fields": [{ "name": "author", "title": "作者", "type": "text", "required": true }] }));
    assert!(e.contains('3') && e.contains("author"), "{e}");
    let e = err(&app, &b, "update_collection", json!({ "name": "news", "note": "n",
        "change_fields": [{ "name": "date", "unique": true }] }));
    assert!(e.contains("date"), "{e}");
    let e = err(&app, &b, "update_collection", json!({ "name": "news", "note": "n",
        "change_fields": [{ "name": "title", "type": "number" }] }));
    assert!(e.contains('3') && e.contains("标题a"), "{e}"); // count plus a sample value
}

#[test]
fn type_change_converts_live_and_draft_docs() {
    let app = app();
    let ids = seed(&app);
    ok(&app, &editor(), "update_record", json!({ "id": ids[0], "note": "n", "data": { "date": "2026-12-01" } }));
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "note": "n",
        "change_fields": [{ "name": "date", "type": "text", "index": false }] }));
    ok(&app, &builder(), "publish", json!({ "note": "n", "targets": ["collection:news"] }));
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": ids[1] }))["data"]["date"], "2026-11-01");
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": ids[0] }))["data"]["date"], "2026-12-01");
}

#[test]
fn removing_a_field_needs_the_count_and_is_reversible() {
    let app = app();
    let ids = seed(&app);
    let b = builder();
    let e = err(&app, &b, "update_collection", json!({ "name": "news", "note": "n", "remove_fields": ["date"] }));
    assert!(e.contains("confirm") && e.contains('3'), "{e}");
    assert!(err(&app, &b, "update_collection", json!({ "name": "news", "note": "n", "remove_fields": ["date"], "confirm": 2 })).contains('3'));
    ok(&app, &b, "update_collection", json!({ "name": "news", "note": "n", "remove_fields": ["date"], "confirm": 3 }));
    ok(&app, &b, "update_collection", json!({ "name": "news", "note": "n", "remove_fields": ["cover"] })); // no data: no confirm
    publish_all(&app);
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": ids[0] }))["data"].get("date"), None);
    ok(&app, &b, "update_collection", json!({ "name": "news", "note": "n",
        "add_fields": [{ "name": "date", "title": "日期", "type": "date" }] }));
    publish_all(&app);
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": ids[0] }))["data"]["date"], "2026-11-01");
}

#[test]
fn deleting_a_collection_is_soft_and_its_name_stays_taken() {
    let app = app();
    seed(&app);
    let b = builder();
    assert!(err(&app, &b, "delete_collection", json!({ "name": "news", "note": "n" })).contains('3'));
    ok(&app, &b, "delete_collection", json!({ "name": "news", "note": "n", "confirm": 3 }));
    assert_eq!(ok(&app, &reader(), "get_collection", json!({ "name": "news" }))["status"], "deleting");
    publish_all(&app);
    assert_eq!(ok(&app, &reader(), "list_collections", json!({})), json!([]));
    let rows: i64 = app.conn().query_row("SELECT count(*) FROM records WHERE collection='news' AND published IS NOT NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 3); // data is kept

    let e = err(&app, &b, "create_collection", json!({ "name": "news", "title": "新的", "fields": [], "note": "n" }));
    assert!(e.contains("restore_version"), "{e}");
    let v = ok(&app, &reader(), "list_versions", json!({ "target": "collection:news" }));
    ok(&app, &b, "restore_version", json!({ "target": "collection:news", "version_id": v[1]["id"], "note": "恢复" }));
    publish_all(&app);
    assert_eq!(ok(&app, &reader(), "query_records", json!({ "collection": "news" }))["total"], 3);
}
