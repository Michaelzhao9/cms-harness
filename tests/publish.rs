mod common;
use common::*;
use serde_json::json;

fn one(app: &harness_cms::Shared, title: &str, slug: &str) -> i64 {
    ok(app, &editor(), "create_record", json!({ "collection": "news", "note": "n", "data": { "title": title, "slug": slug } }))["id"].as_i64().unwrap()
}

#[test]
fn nothing_pending_is_a_clear_message() {
    let app = app();
    assert!(err(&app, &builder(), "publish", json!({ "note": "n" })).contains("没有待发布"));
    assert_eq!(ok(&app, &reader(), "list_pending", json!({})), json!([]));
}

#[test]
fn structure_then_content_in_one_release() {
    let app = app();
    news(&app);
    let id = one(&app, "开业", "open");
    let pending = ok(&app, &reader(), "list_pending", json!({}));
    assert_eq!(pending.as_array().unwrap().len(), 2);
    assert_eq!(pending[0]["target"], "collection:news");
    let rel = ok(&app, &builder(), "publish", json!({ "note": "首发" }));
    assert_eq!(rel["published"].as_array().unwrap().len(), 2);
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": id }))["status"], "published");
    assert_eq!(ok(&app, &reader(), "get_collection", json!({ "name": "news" }))["status"], "published");
    assert_eq!(ok(&app, &reader(), "list_pending", json!({})), json!([]));
}

#[test]
fn content_cannot_go_live_before_its_structure() {
    let app = app();
    news(&app);
    let id = one(&app, "开业", "open");
    let e = err(&app, &builder(), "publish", json!({ "note": "n", "targets": [format!("record:{id}")] }));
    assert!(e.contains("collection:news"), "{e}");
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": id }))["status"], "draft"); // nothing was published
}

#[test]
fn editors_publish_content_only() {
    let app = app();
    news(&app);
    publish_all(&app);
    let id = one(&app, "开业", "open");
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "note": "n",
        "add_fields": [{ "name": "author", "title": "作者", "type": "text" }] }));
    let rel = ok(&app, &editor(), "publish", json!({ "note": "n" }));
    assert_eq!(rel["published"], json!([format!("record:{id}")]));
    assert_eq!(rel["skipped"], json!(["collection:news"]));
    assert!(err(&app, &editor(), "publish", json!({ "note": "n", "targets": ["collection:news"] })).contains("structure"));
    assert!(err(&app, &reader(), "publish", json!({ "note": "n" })).contains("publish"));
}

#[test]
fn history_restore_and_delete() {
    let app = app();
    news(&app);
    let id = one(&app, "第一版", "v");
    publish_all(&app);
    ok(&app, &editor(), "update_record", json!({ "id": id, "note": "改标题", "data": { "title": "第二版" } }));
    publish_all(&app);
    let target = format!("record:{id}");
    let versions = ok(&app, &reader(), "list_versions", json!({ "target": target }));
    assert_eq!(versions.as_array().unwrap().len(), 2);
    assert_eq!(versions[0]["note"], "改标题");

    ok(&app, &editor(), "restore_version", json!({ "target": target, "version_id": versions[1]["id"], "note": "回滚" }));
    let got = ok(&app, &reader(), "get_record", json!({ "id": id }));
    assert_eq!((got["status"].as_str(), got["data"]["title"].as_str()), (Some("changed"), Some("第一版")));
    ok(&app, &editor(), "discard_draft", json!({ "target": target }));
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": id }))["data"]["title"], "第二版");

    ok(&app, &editor(), "delete_record", json!({ "id": id, "note": "删" }));
    assert!(err(&app, &editor(), "update_record", json!({ "id": id, "note": "n", "data": { "title": "x" } })).contains("discard_draft"));
    publish_all(&app);
    assert!(err(&app, &reader(), "get_record", json!({ "id": id })).contains(&id.to_string()));
    let versions = ok(&app, &reader(), "list_versions", json!({ "target": target }));
    assert_eq!(versions[0]["deleted"], true);
    ok(&app, &editor(), "restore_version", json!({ "target": target, "version_id": versions[1]["id"], "note": "救回" }));
    publish_all(&app);
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": id }))["data"]["title"], "第二版"); // same id is back
}

#[test]
fn uniqueness_is_rechecked_at_publish() {
    let app = app();
    news(&app);
    let a = one(&app, "A", "a");
    publish_all(&app);
    // Bypass the draft-time check the way a race between two writers would.
    app.conn().execute("INSERT INTO records(collection, draft) VALUES('news', '{\"title\":\"B\",\"slug\":\"a\"}')", []).unwrap();
    let e = err(&app, &builder(), "publish", json!({ "note": "n" }));
    assert!(e.contains("slug"), "{e}");
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": a }))["status"], "published");
}
