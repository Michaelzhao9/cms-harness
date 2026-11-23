mod common;
use axum::http::StatusCode;
use common::*;
use harness_cms::auth::Scope;
use serde_json::json;

fn seed(app: &harness_cms::Shared, public: bool) {
    news(app);
    ok(app, &builder(), "update_collection", json!({ "name": "news", "note": "n", "public_read": public }));
    for (t, s, d, tag) in [("开业", "open", "2026-11-01", "公司"), ("展会", "expo", "2026-11-20", "行业")] {
        ok(app, &editor(), "create_record", json!({ "collection": "news", "note": "n", "data": { "title": t, "slug": s, "date": d, "tag": tag } }));
    }
    publish_all(app);
    ok(app, &editor(), "create_record", json!({ "collection": "news", "note": "n", "data": { "title": "草稿", "slug": "draft" } }));
}

#[tokio::test]
async fn public_collections_are_readable_without_a_key() {
    let app = app();
    seed(&app, true);
    let (status, headers, body) = get(&app, "/api/v1/content/news?sort=-date", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["access-control-allow-origin"], "*");
    assert_eq!(body["total"], 2); // the draft is not served
    assert_eq!(body["items"][0]["slug"], "expo");
    assert!(body["items"][0]["id"].is_i64());
    let (_, _, body) = get(&app, "/api/v1/content/news?tag=%E5%85%AC%E5%8F%B8", None).await; // tag=公司
    assert_eq!((body["total"].as_i64(), body["items"][0]["slug"].as_str()), (Some(1), Some("open")));
    let (_, _, body) = get(&app, "/api/v1/content/news?date__gte=2026-11-10&limit=1", None).await;
    assert_eq!(body["items"][0]["slug"], "expo");
    let id = body["items"][0]["id"].as_i64().unwrap();
    let (status, _, one) = get(&app, &format!("/api/v1/content/news/{id}"), None).await;
    assert_eq!((status, one["title"].as_str()), (StatusCode::OK, Some("展会")));
}

#[tokio::test]
async fn private_collections_need_a_read_key() {
    let app = app();
    seed(&app, false);
    let (status, headers, body) = get(&app, "/api/v1/content/news", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(headers.get("access-control-allow-origin").is_none());
    assert_eq!(body["ok"], false);
    let k = key(&app, &[Scope::Read]);
    assert_eq!(get(&app, "/api/v1/content/news", Some("hc_wrong")).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(get(&app, "/api/v1/content/news", Some(&k)).await.0, StatusCode::OK);
}

#[tokio::test]
async fn bad_requests_get_clear_answers() {
    let app = app();
    seed(&app, true);
    assert_eq!(get(&app, "/api/v1/content/nope", None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(get(&app, "/api/v1/content/news/999", None).await.0, StatusCode::NOT_FOUND);
    let (status, _, body) = get(&app, "/api/v1/content/news?body=x", None).await; // body is not indexed
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("body"));
    assert_eq!(get(&app, "/api/v1/content/news?limit=1000", None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(get(&app, "/api/v1/content/news?date__gte=2030-01-01", None).await.2, json!({ "items": [], "total": 0 }));
}

#[tokio::test]
async fn single_collections_return_one_object_and_expand_works() {
    let app = app();
    let b = builder();
    ok(&app, &b, "create_collection", json!({ "name": "site", "title": "设置", "single": true, "public_read": true, "note": "n",
        "fields": [{ "name": "name", "title": "站名", "type": "text" }] }));
    ok(&app, &b, "create_collection", json!({ "name": "people", "title": "人物", "public_read": true, "note": "n",
        "fields": [{ "name": "name", "title": "姓名", "type": "text" }] }));
    ok(&app, &b, "create_collection", json!({ "name": "posts", "title": "文章", "public_read": true, "note": "n",
        "fields": [{ "name": "author", "title": "作者", "type": "relation", "target": "people" }] }));
    publish_all(&app);
    assert_eq!(get(&app, "/api/v1/content/site", None).await.0, StatusCode::NOT_FOUND);
    ok(&app, &editor(), "create_record", json!({ "collection": "site", "note": "n", "data": { "name": "甲" } }));
    let p = ok(&app, &editor(), "create_record", json!({ "collection": "people", "note": "n", "data": { "name": "张" } }))["id"].clone();
    ok(&app, &editor(), "create_record", json!({ "collection": "posts", "note": "n", "data": { "author": p } }));
    publish_all(&app);
    assert_eq!(get(&app, "/api/v1/content/site", None).await.2["name"], "甲");
    assert_eq!(get(&app, "/api/v1/content/posts", None).await.2["items"][0]["author"], p);
    assert_eq!(get(&app, "/api/v1/content/posts?expand=author", None).await.2["items"][0]["author"]["name"], "张");
}

#[test]
fn filtered_reads_use_the_field_index() {
    use harness_cms::{changes, query};
    let app = app();
    seed(&app, true);
    let conn = app.conn();
    let def = changes::live_def(&conn, "news").unwrap();
    let (q, _) = query::parse_params(&def, &[("date__gte".into(), "2026-11-10".into())]).unwrap();
    let (sql, params) = query::live_sql(&def, &q);
    let plan: Vec<String> = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap()
        .query_map(rusqlite::params_from_iter(params), |r| r.get(3)).unwrap().map(Result::unwrap).collect();
    assert!(plan.iter().any(|line| line.contains("hc:news:date")), "{plan:?}");
}

#[tokio::test]
async fn expand_rejects_non_relations_and_unknown_operators() {
    let app = app();
    seed(&app, true);
    let (status, _, body) = get(&app, "/api/v1/content/news?expand=title", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("title"));
    assert_eq!(get(&app, "/api/v1/content/news?date__near=1", None).await.0, StatusCode::BAD_REQUEST);
}

#[test]
fn indexes_follow_published_definitions() {
    let app = app();
    seed(&app, true);
    let names = |app: &harness_cms::Shared| -> Vec<String> {
        let conn = app.conn();
        let mut st = conn.prepare("SELECT name FROM sqlite_master WHERE name LIKE 'hc:%' ORDER BY name").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert_eq!(names(&app), ["hc:news:date", "hc:news:slug", "hc:news:tag"]);
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "note": "n", "remove_fields": ["date", "tag"], "confirm": 2 }));
    assert_eq!(names(&app).len(), 3, "a draft definition changes nothing live");
    publish_all(&app);
    assert_eq!(names(&app), ["hc:news:slug"]);
    ok(&app, &builder(), "delete_collection", json!({ "name": "news", "note": "n", "confirm": 3 }));
    publish_all(&app);
    assert!(names(&app).is_empty());
}
