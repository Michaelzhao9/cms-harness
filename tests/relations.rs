mod common;
use common::*;
use serde_json::json;

fn setup(app: &cms_harness::Shared) -> (i64, i64) {
    let b = builder();
    ok(app, &b, "create_collection", json!({ "name": "people", "title": "人物", "note": "n",
        "fields": [{ "name": "name", "title": "姓名", "type": "text" }] }));
    ok(app, &b, "create_collection", json!({ "name": "posts", "title": "文章", "note": "n",
        "fields": [{ "name": "author", "title": "作者", "type": "relation", "target": "people" }] }));
    let p = ok(app, &editor(), "create_record", json!({ "collection": "people", "note": "n", "data": { "name": "张" } }))["id"].as_i64().unwrap();
    let post = ok(app, &editor(), "create_record", json!({ "collection": "posts", "note": "n", "data": { "author": p } }))["id"].as_i64().unwrap();
    (p, post)
}

#[test]
fn cannot_publish_a_link_to_an_unpublished_record() {
    let app = app();
    let (p, post) = setup(&app);
    ok(&app, &builder(), "publish", json!({ "note": "n", "targets": ["collection:people", "collection:posts"] }));
    let e = err(&app, &builder(), "publish", json!({ "note": "n", "targets": [format!("record:{post}")] }));
    assert!(e.contains(&format!("record:{p}")), "{e}");
    ok(&app, &builder(), "publish", json!({ "note": "n" })); // together is fine
}

#[test]
fn cannot_delete_a_record_that_is_still_referenced() {
    let app = app();
    let (p, post) = setup(&app);
    publish_all(&app);
    ok(&app, &editor(), "delete_record", json!({ "id": p, "note": "n" }));
    let e = err(&app, &builder(), "publish", json!({ "note": "n" }));
    assert!(e.contains(&format!("record:{post}")), "{e}");
    ok(&app, &editor(), "update_record", json!({ "id": post, "note": "n", "data": { "author": null } }));
    ok(&app, &builder(), "publish", json!({ "note": "n" })); // unlink and delete in one release
}

#[test]
fn cannot_delete_a_collection_that_is_a_relation_target() {
    let app = app();
    setup(&app);
    publish_all(&app);
    ok(&app, &builder(), "delete_collection", json!({ "name": "people", "note": "n", "confirm": 1 }));
    let e = err(&app, &builder(), "publish", json!({ "note": "n" }));
    assert!(e.contains("collection:posts") && e.contains("author"), "{e}");
}
