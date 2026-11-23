mod common;
use common::*;
use serde_json::json;

fn coll(app: &harness_cms::Shared, name: &str, fields: serde_json::Value) {
    ok(app, &builder(), "create_collection", json!({ "name": name, "title": name, "note": "n", "fields": fields }));
}

fn rec(app: &harness_cms::Shared, collection: &str, data: serde_json::Value) -> i64 {
    ok(app, &editor(), "create_record", json!({ "collection": collection, "note": "n", "data": data }))["id"].as_i64().unwrap()
}

#[test]
fn every_id_of_a_multi_valued_relation_is_checked() {
    let app = app();
    coll(&app, "tags", json!([{ "name": "name", "title": "名称", "type": "text" }]));
    coll(&app, "posts", json!([{ "name": "tags", "title": "标签", "type": "relation", "target": "tags", "multiple": true }]));
    let (a, b) = (rec(&app, "tags", json!({ "name": "a" })), rec(&app, "tags", json!({ "name": "b" })));
    let post = rec(&app, "posts", json!({ "tags": [a, b] }));
    // Publish the structure, one tag and the post: the other tag is still a draft.
    let e = err(&app, &builder(), "publish", json!({ "note": "n",
        "targets": ["collection:tags", "collection:posts", format!("record:{a}"), format!("record:{post}")] }));
    assert!(e.contains(&format!("record:{b}")) && !e.contains(&format!("指向 record:{a}，")), "{e}");
    publish_all(&app);
}

#[test]
fn a_collection_cannot_go_live_pointing_at_an_unpublished_collection() {
    let app = app();
    coll(&app, "people", json!([{ "name": "name", "title": "姓名", "type": "text" }]));
    coll(&app, "posts", json!([{ "name": "author", "title": "作者", "type": "relation", "target": "people" }]));
    let e = err(&app, &builder(), "publish", json!({ "note": "n", "targets": ["collection:posts"] }));
    assert!(e.contains("collection:posts") && e.contains("people"), "{e}");
    assert_eq!(ok(&app, &reader(), "get_collection", json!({ "name": "posts" }))["status"], "draft");
}

#[test]
fn a_record_may_point_at_another_record_of_its_own_collection() {
    let app = app();
    coll(&app, "pages", json!([
        { "name": "title", "title": "标题", "type": "text" },
        { "name": "parent", "title": "上级", "type": "relation", "target": "pages" }
    ]));
    let home = rec(&app, "pages", json!({ "title": "首页" }));
    let about = rec(&app, "pages", json!({ "title": "关于", "parent": home }));
    publish_all(&app);
    // The parent cannot be deleted while the child still points at it.
    ok(&app, &editor(), "delete_record", json!({ "id": home, "note": "n" }));
    assert!(err(&app, &builder(), "publish", json!({ "note": "n" })).contains(&format!("record:{about}")));
}

#[test]
fn unrelated_collections_are_not_rescanned_into_false_alarms() {
    let app = app();
    coll(&app, "people", json!([{ "name": "name", "title": "姓名", "type": "text" }]));
    coll(&app, "posts", json!([{ "name": "author", "title": "作者", "type": "relation", "target": "people" }]));
    news(&app);
    let p = rec(&app, "people", json!({ "name": "张" }));
    rec(&app, "posts", json!({ "author": p }));
    publish_all(&app);
    // A later release that only touches news goes through.
    ok(&app, &editor(), "create_record", json!({ "collection": "news", "note": "n", "data": { "title": "A", "slug": "a" } }));
    assert_eq!(publish_all(&app)["published"].as_array().unwrap().len(), 1);
}
