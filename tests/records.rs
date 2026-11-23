mod common;
use common::*;
use serde_json::json;

#[test]
fn create_update_get_query() {
    let app = app();
    news(&app);
    let e = editor();
    let a = ok(&app, &e, "create_record", json!({ "collection": "news", "note": "n",
        "data": { "title": "开业", "slug": "open", "date": "2026-11-01", "tag": "公司" } }))["id"].as_i64().unwrap();
    ok(&app, &e, "create_record", json!({ "collection": "news", "note": "n",
        "data": { "title": "展会", "slug": "expo", "date": "2026-11-20", "tag": "行业" } }));
    ok(&app, &e, "update_record", json!({ "id": a, "note": "n", "data": { "title": "盛大开业" } }));
    let got = ok(&app, &reader(), "get_record", json!({ "id": a }));
    assert_eq!((got["status"].as_str(), got["data"]["title"].as_str(), got["data"]["slug"].as_str()),
               (Some("draft"), Some("盛大开业"), Some("open")));

    let q = ok(&app, &reader(), "query_records", json!({ "collection": "news", "sort": "-date" }));
    assert_eq!(q["total"], 2);
    assert_eq!(q["items"][0]["data"]["slug"], "expo");
    let q = ok(&app, &reader(), "query_records", json!({ "collection": "news", "filter": { "date": { "lt": "2026-11-10" } } }));
    assert_eq!((q["total"].as_i64(), q["items"][0]["id"].as_i64()), (Some(1), Some(a)));
    let q = ok(&app, &reader(), "query_records", json!({ "collection": "news", "limit": 1, "offset": 1, "sort": "date" }));
    assert_eq!((q["total"].as_i64(), q["items"].as_array().unwrap().len()), (Some(2), 1));
}

#[test]
fn uniqueness_sees_drafts() {
    let app = app();
    news(&app);
    let e = editor();
    let a = ok(&app, &e, "create_record", json!({ "collection": "news", "note": "n", "data": { "title": "A", "slug": "same" } }))["id"].clone();
    assert!(err(&app, &e, "create_record", json!({ "collection": "news", "note": "n", "data": { "title": "B", "slug": "same" } })).contains("slug"));
    ok(&app, &e, "update_record", json!({ "id": a, "note": "n", "data": { "slug": "same" } })); // a record may keep its own value
}

#[test]
fn rejects_work_on_things_being_deleted() {
    let app = app();
    news(&app);
    let (b, e) = (builder(), editor());
    assert!(err(&app, &e, "update_record", json!({ "id": 999, "note": "n", "data": {} })).contains("999"));
    assert!(err(&app, &e, "create_record", json!({ "collection": "nope", "note": "n", "data": {} })).contains("nope"));
    assert!(err(&app, &reader(), "query_records", json!({ "collection": "news", "filter": { "body": "x" } })).contains("body"));
    ok(&app, &b, "delete_collection", json!({ "name": "news", "note": "n" })); // never published: gone at once
    assert!(err(&app, &e, "create_record", json!({ "collection": "news", "note": "n", "data": { "title": "x" } })).contains("news"));
}

#[test]
fn single_collections_hold_one_record() {
    let app = app();
    ok(&app, &builder(), "create_collection", json!({ "name": "site", "title": "网站设置", "single": true, "note": "n",
        "fields": [{ "name": "name", "title": "站名", "type": "text" }] }));
    ok(&app, &editor(), "create_record", json!({ "collection": "site", "note": "n", "data": { "name": "甲" } }));
    assert!(err(&app, &editor(), "create_record", json!({ "collection": "site", "note": "n", "data": { "name": "乙" } })).contains("site"));
}

#[test]
fn relations_must_point_at_existing_records() {
    let app = app();
    ok(&app, &builder(), "create_collection", json!({ "name": "people", "title": "人物", "note": "n",
        "fields": [{ "name": "name", "title": "姓名", "type": "text" }] }));
    ok(&app, &builder(), "create_collection", json!({ "name": "posts", "title": "文章", "note": "n",
        "fields": [{ "name": "author", "title": "作者", "type": "relation", "target": "people" }] }));
    let p = ok(&app, &editor(), "create_record", json!({ "collection": "people", "note": "n", "data": { "name": "张" } }))["id"].clone();
    let post = ok(&app, &editor(), "create_record", json!({ "collection": "posts", "note": "n", "data": { "author": p } }))["id"].clone();
    assert!(err(&app, &editor(), "create_record", json!({ "collection": "posts", "note": "n", "data": { "author": 999 } })).contains("999"));
    assert!(err(&app, &editor(), "create_record", json!({ "collection": "posts", "note": "n", "data": { "author": post } })).contains("people"));
}
