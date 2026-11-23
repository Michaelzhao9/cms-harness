mod common;
use common::*;
use serde_json::json;

#[test]
fn create_read_update_discard() {
    let app = app();
    news(&app);
    let got = ok(&app, &reader(), "get_collection", json!({ "name": "news" }));
    assert_eq!(got["status"], "draft");
    assert_eq!(got["definition"]["fields"].as_array().unwrap().len(), 6);
    assert_eq!(got["record_count"], 0);

    ok(&app, &builder(), "update_collection", json!({ "name": "news", "note": "加作者",
        "add_fields": [{ "name": "author", "title": "作者", "type": "text" }],
        "change_fields": [{ "name": "title", "title": "新闻标题" }] }));
    let def = ok(&app, &reader(), "get_collection", json!({ "name": "news" }))["definition"].clone();
    assert_eq!(def["fields"][0]["title"], "新闻标题");
    assert_eq!(def["fields"][6]["name"], "author");

    assert_eq!(ok(&app, &reader(), "list_collections", json!({})).as_array().unwrap().len(), 1);
    // The discard_draft tool arrives in Task 7; until then call the function it will wrap.
    harness_cms::changes::discard(&app.conn(), &harness_cms::changes::Target::Collection("news".into())).unwrap();
    assert_eq!(ok(&app, &reader(), "list_collections", json!({})), json!([])); // never published: the row is gone
}

#[test]
fn rules_are_enforced() {
    let app = app();
    news(&app);
    let b = builder();
    assert!(err(&app, &editor(), "create_collection", json!({ "name": "x", "title": "X", "fields": [], "note": "n" })).contains("structure"));
    assert!(err(&app, &b, "create_collection", json!({ "name": "news", "title": "重复", "fields": [], "note": "n" })).contains("news"));
    assert!(err(&app, &b, "create_collection", json!({ "name": "Bad", "title": "X", "fields": [], "note": "n" })).contains("Bad"));
    assert!(err(&app, &b, "create_collection", json!({ "name": "x", "title": "X", "fields": [], "note": "n", "extra": 1 })).contains("extra"));
    assert!(err(&app, &b, "update_collection", json!({ "name": "news", "note": "n",
        "add_fields": [{ "name": "title", "title": "重复", "type": "text" }] })).contains("title"));
    assert!(err(&app, &b, "update_collection", json!({ "name": "news", "note": "n",
        "change_fields": [{ "name": "nope", "title": "X" }] })).contains("nope"));
    assert!(err(&app, &b, "create_collection", json!({ "name": "x", "title": "X", "note": "n",
        "fields": [{ "name": "a", "title": "A", "type": "relation", "target": "missing" }] })).contains("missing"));
    assert!(err(&app, &b, "no_such_tool", json!({})).contains("no_such_tool"));
}

#[test]
fn every_tool_has_an_object_schema() {
    for spec in harness_cms::tools::specs() {
        assert_eq!(spec.schema["type"], "object", "{}", spec.name);
        assert_eq!(spec.schema["additionalProperties"], false, "{}", spec.name);
    }
}
