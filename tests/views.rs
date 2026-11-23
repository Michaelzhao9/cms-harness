mod common;
use common::*;
use serde_json::json;

fn view() -> serde_json::Value {
    json!({ "name": "news_by_date", "title": "新闻（按日期）", "collection": "news", "note": "n",
            "columns": ["title", "date", "tag"], "filters": ["tag"], "sort": "-date", "actions": ["create", "edit"] })
}

#[test]
fn views_follow_draft_publish_and_history() {
    let app = app();
    news(&app);
    ok(&app, &builder(), "set_view", view());
    assert_eq!(ok(&app, &reader(), "list_views", json!({}))[0]["status"], "draft");
    publish_all(&app);
    assert_eq!(ok(&app, &reader(), "list_views", json!({}))[0]["status"], "published");
    let mut changed = view();
    changed["columns"] = json!(["title"]);
    ok(&app, &builder(), "set_view", changed);
    assert_eq!(ok(&app, &reader(), "list_pending", json!({}))[0]["target"], "view:news_by_date");
    ok(&app, &builder(), "discard_draft", json!({ "target": "view:news_by_date" }));
    assert_eq!(ok(&app, &reader(), "list_views", json!({}))[0]["definition"]["columns"].as_array().unwrap().len(), 3);
    ok(&app, &builder(), "delete_view", json!({ "name": "news_by_date", "note": "n" }));
    publish_all(&app);
    assert_eq!(ok(&app, &reader(), "list_views", json!({})), json!([]));
}

#[test]
fn views_are_checked_against_the_collection() {
    let app = app();
    news(&app);
    let b = builder();
    for (key, value, needle) in [("collection", json!("nope"), "nope"), ("columns", json!(["missing"]), "missing"),
                                 ("columns", json!([]), "columns"), ("filters", json!(["body"]), "body"),
                                 ("sort", json!("-body"), "body"), ("name", json!("Bad Name"), "Bad Name"),
                                 ("columns", json!(["title", "title"]), "title"), ("filters", json!(["tag", "tag"]), "tag")] {
        let mut v = view();
        v[key] = value;
        assert!(err(&app, &b, "set_view", v).contains(needle), "{key}");
    }
    assert!(err(&app, &editor(), "set_view", view()).contains("structure"));
    let mut v = view();
    v["actions"] = json!([]);
    ok(&app, &b, "set_view", v);
}

#[test]
fn a_published_view_blocks_removing_its_fields() {
    let app = app();
    news(&app);
    ok(&app, &builder(), "set_view", view());
    publish_all(&app);
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "note": "n", "remove_fields": ["tag"] }));
    let e = err(&app, &builder(), "publish", json!({ "note": "n" }));
    assert!(e.contains("view:news_by_date") && e.contains("tag"), "{e}");
}

#[test]
fn a_published_view_blocks_deleting_its_collection() {
    let app = app();
    news(&app);
    ok(&app, &builder(), "set_view", view());
    publish_all(&app);
    ok(&app, &builder(), "delete_collection", json!({ "name": "news", "note": "n" }));
    let e = err(&app, &builder(), "publish", json!({ "note": "n" }));
    assert!(e.contains("view:news_by_date") && e.contains("collection:news"), "{e}");
}

#[test]
fn site_notes_are_readable_and_drafted() {
    let app = app();
    assert_eq!(ok(&app, &reader(), "get_site_notes", json!({})), json!({ "text": "", "status": "published" }));
    ok(&app, &builder(), "set_site_notes", json!({ "text": "这是一个乐器经销商的官网。", "note": "初稿" }));
    assert_eq!(ok(&app, &reader(), "get_site_notes", json!({}))["status"], "draft");
    publish_all(&app);
    let got = ok(&app, &reader(), "get_site_notes", json!({}));
    assert_eq!((got["status"].as_str(), got["text"].as_str()), (Some("published"), Some("这是一个乐器经销商的官网。")));
    assert!(err(&app, &builder(), "set_site_notes", json!({ "text": "长".repeat(20_001), "note": "n" })).contains("20000"));
    assert!(err(&app, &editor(), "set_site_notes", json!({ "text": "x", "note": "n" })).contains("structure"));
}
