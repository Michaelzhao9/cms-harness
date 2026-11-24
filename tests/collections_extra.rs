mod common;
use common::*;
use cms_harness::changes::{status, Target};
use serde_json::json;

fn create(app: &cms_harness::Shared, note: &str) -> Result<serde_json::Value, String> {
    cms_harness::tools::call(app, &builder(), "create_collection",
        json!({ "name": "x", "title": "X", "fields": [], "note": note }))
}

#[test]
fn argument_errors_name_the_parameter() {
    let app = app();
    let b = builder();
    assert!(err(&app, &b, "create_collection", json!({ "name": "x", "title": "X", "fields": [], "note": "n", "single": "yes" })).contains("single"));
    assert!(err(&app, &b, "create_collection", json!({ "name": "x", "title": "X", "note": "n",
        "fields": [{ "name": "a", "title": 5, "type": "text" }] })).contains("fields"));
    assert!(err(&app, &b, "create_collection", json!({ "name": "x", "title": "X", "fields": [] })).contains("note"));
    assert!(err(&app, &b, "create_collection", json!([1, 2])).contains("对象"));
    assert!(err(&app, &b, "create_collection", json!({ "name": "x", "title": "X", "fields": [], "note": "n", "extra": 1 })).contains("extra"));
}

#[test]
fn every_listed_tool_is_implemented() {
    let app = app();
    for spec in cms_harness::tools::specs() {
        if let Err(e) = cms_harness::tools::call(&app, &builder(), spec.name, json!({})) {
            assert!(!e.contains("尚未实现"), "{} has no implementation: {e}", spec.name);
        }
    }
}

#[test]
fn deleting_a_never_published_collection_removes_its_records() {
    let app = app();
    news(&app);
    app.conn().execute("INSERT INTO records(collection, draft) VALUES('news', '{}')", []).unwrap();
    // It holds a record, so the call must name the count.
    assert!(err(&app, &builder(), "delete_collection", json!({ "name": "news", "note": "删" })).contains("confirm"));
    let r = ok(&app, &builder(), "delete_collection", json!({ "name": "news", "note": "删", "confirm": 1 }));
    assert_eq!(r["status"], "deleted");
    let n: i64 = app.conn().query_row("SELECT count(*) FROM records", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    assert_eq!(ok(&app, &reader(), "list_collections", json!({})), json!([]));
}

#[test]
fn remove_fields() {
    let app = app();
    news(&app);
    let b = builder();
    ok(&app, &b, "update_collection", json!({ "name": "news", "note": "n", "remove_fields": ["cover"] }));
    let def = ok(&app, &b, "get_collection", json!({ "name": "news" }))["definition"].clone();
    assert_eq!(def["fields"].as_array().unwrap().len(), 5);
    assert!(err(&app, &b, "update_collection", json!({ "name": "news", "note": "n", "remove_fields": ["nope"] })).contains("nope"));
}

#[test]
fn note_rules() {
    let app = app();
    assert!(create(&app, "").is_err());
    assert!(create(&app, "   ").is_err());
    assert!(create(&app, &"a".repeat(201)).is_err());
    assert!(create(&app, &"a".repeat(200)).is_ok());
}

#[test]
fn null_args_and_scope_order() {
    let app = app();
    assert_eq!(ok(&app, &reader(), "list_collections", json!(null)), json!([]));
    assert!(err(&app, &reader(), "create_collection", json!({ "bogus": 1 })).contains("structure"));
}

#[test]
fn self_relation_is_accepted() {
    let app = app();
    ok(&app, &builder(), "create_collection", json!({ "name": "page", "title": "页", "note": "n",
        "fields": [{ "name": "parent", "title": "上级", "type": "relation", "target": "page" }] }));
}

#[test]
fn status_and_target() {
    assert_eq!(status(false, true, false), "draft");
    assert_eq!(status(true, true, false), "changed");
    assert_eq!(status(true, false, false), "published");
    assert_eq!(status(true, false, true), "deleting");
    assert_eq!(status(false, false, false), "deleted");
    for t in [Target::Collection("news".into()), Target::Record(7), Target::View("v".into()), Target::Notes] {
        assert_eq!(Target::parse(&t.to_string()).unwrap(), t);
    }
    for bad in ["record:abc", "collection:Bad", "nope:1", ""] {
        assert!(Target::parse(bad).is_err(), "{bad}");
    }
}
