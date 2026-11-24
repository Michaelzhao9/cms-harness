mod common;

use axum::http::{header, Request, StatusCode};
use common::*;
use cms_harness::Shared;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn call(app: &Shared, req: Request<axum::body::Body>) -> (StatusCode, Value) {
    let resp = cms_harness::api::router(app.clone()).oneshot(req).await.unwrap();
    let s = resp.status();
    let b = resp.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

async fn read(app: &Shared, path: &str, cookie: Option<&str>, key: Option<&str>) -> (StatusCode, Value) {
    let mut r = Request::get(path);
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, c);
    }
    if let Some(k) = key {
        r = r.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    call(app, r.body(axum::body::Body::empty()).unwrap()).await
}

async fn post(app: &Shared, path: &str, body: Value, cookie: Option<&str>) -> (StatusCode, Value) {
    let mut r = Request::post(path).header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, c);
    }
    call(app, r.body(axum::body::Body::from(body.to_string())).unwrap()).await
}

async fn member(app: &Shared, email: &str) -> String {
    let resp = cms_harness::api::router(app.clone())
        .oneshot(Request::post("/api/v1/members/signup").header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(json!({"email": email, "password": "long-enough-1"}).to_string())).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string()
}

/// A collection with the given rules, one record from the admin, member login on, all published.
fn setup(app: &Shared, read: &str, create: &str) {
    ok(app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    ok(app, &builder(), "create_collection", json!({"name": "notes", "title": "留言", "read": read, "create": create, "note": "x",
        "fields": [{"name": "text", "title": "内容", "type": "text", "required": true}, {"name": "stars", "title": "评分", "type": "number"}]}));
    ok(app, &builder(), "create_record", json!({"collection": "notes", "data": {"text": "后台写的"}, "note": "x"}));
    publish_all(app);
}

#[tokio::test]
async fn each_read_level_from_each_caller() {
    for (level, anon, member_sees, key_sees) in [("public", true, true, true), ("members", false, true, true), ("private", false, false, true)] {
        let app = app();
        setup(&app, level, "none");
        let m = member(&app, "a@example.com").await;
        let k = key(&app, &[]);
        assert_eq!(read(&app, "/api/v1/content/notes", None, None).await.0 == StatusCode::OK, anon, "{level} anon");
        assert_eq!(read(&app, "/api/v1/content/notes", Some(&m), None).await.0 == StatusCode::OK, member_sees, "{level} member");
        assert_eq!(read(&app, "/api/v1/content/notes", None, Some(&k)).await.0 == StatusCode::OK, key_sees, "{level} key");
    }
}

#[tokio::test]
async fn owner_records_are_each_members_own() {
    let app = app();
    setup(&app, "owner", "members");
    let a = member(&app, "a@example.com").await;
    let b = member(&app, "b@example.com").await;
    let (s, v) = post(&app, "/api/v1/content/notes", json!({"text": "A 的留言"}), Some(&a)).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let id = v["id"].as_i64().unwrap();
    post(&app, "/api/v1/content/notes", json!({"text": "B 的留言"}), Some(&b)).await;

    let (_, mine) = read(&app, "/api/v1/content/notes", Some(&a), None).await;
    let texts: Vec<&str> = mine["items"].as_array().unwrap().iter().map(|i| i["text"].as_str().unwrap()).collect();
    assert_eq!(texts, vec!["A 的留言"], "only A's own; not B's, not the admin's");
    assert_eq!(read(&app, &format!("/api/v1/content/notes/{id}"), Some(&b), None).await.0, StatusCode::NOT_FOUND, "404, not 401");
    assert_eq!(read(&app, "/api/v1/content/notes", None, None).await.0, StatusCode::UNAUTHORIZED);
    let (_, all) = read(&app, "/api/v1/content/notes", None, Some(&key(&app, &[]))).await;
    assert_eq!(all["total"], 3);
    let owner: Option<i64> = app.conn().query_row("SELECT owner FROM records WHERE id = ?1", [id], |r| r.get(0)).unwrap();
    assert!(owner.is_some());
}

#[tokio::test]
async fn public_submissions_go_live_at_once_without_an_owner_and_are_rate_limited() {
    let app = app();
    setup(&app, "public", "public");
    let (s, v) = post(&app, "/api/v1/content/notes", json!({"text": "访客留言", "stars": 5}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let id = v["id"].as_i64().unwrap();
    let (_, item) = read(&app, &format!("/api/v1/content/notes/{id}"), None, None).await;
    assert_eq!(item["text"], "访客留言");
    let (owner, published, draft): (Option<i64>, Option<String>, Option<String>) =
        app.conn().query_row("SELECT owner, published, draft FROM records WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!((owner, published.is_some(), draft), (None, true, None));
    let actor: String = app.conn().query_row("SELECT actor FROM versions WHERE target = ?1", [format!("record:{id}")], |r| r.get(0)).unwrap();
    assert_eq!(actor, "访客", "no visitor address is kept in history");
    let session = admin_session(&app, "builder").await;
    let (_, _, history) = get_with(&app, "/admin/releases", Some(&session.0)).await;
    assert!(!history.contains("访客提交"), "submissions stay out of the publish history");
    // Validation still applies.
    assert_eq!(post(&app, "/api/v1/content/notes", json!({"stars": 3}), None).await.0, StatusCode::BAD_REQUEST);
    for _ in 0..19 {
        post(&app, "/api/v1/content/notes", json!({"text": "刷"}), None).await;
    }
    assert_eq!(post(&app, "/api/v1/content/notes", json!({"text": "又刷"}), None).await.0, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn create_rules_are_enforced() {
    let app = app();
    setup(&app, "public", "none");
    assert_eq!(post(&app, "/api/v1/content/notes", json!({"text": "x"}), None).await.0, StatusCode::FORBIDDEN);
    ok(&app, &builder(), "update_collection", json!({"name": "notes", "create": "members", "note": "只许会员"}));
    publish_all(&app);
    assert_eq!(post(&app, "/api/v1/content/notes", json!({"text": "x"}), None).await.0, StatusCode::UNAUTHORIZED);
    let m = member(&app, "c@example.com").await;
    assert_eq!(post(&app, "/api/v1/content/notes", json!({"text": "会员留言"}), Some(&m)).await.0, StatusCode::CREATED);
}

#[test]
fn the_old_public_read_still_means_public() {
    let app = app();
    ok(&app, &builder(), "create_collection", json!({"name": "old", "title": "旧", "public_read": true, "note": "x", "fields": [{"name": "t", "title": "t", "type": "text"}]}));
    let def = ok(&app, &builder(), "get_collection", json!({"name": "old"}))["definition"].clone();
    assert_eq!(def["access"], json!({"read": "public", "create": "none"}));
    let parsed: cms_harness::schema::Access = serde_json::from_value(json!({"public_read": true})).unwrap();
    assert!(parsed.is_public());
}

#[tokio::test]
async fn site_forms_submit_and_the_admin_shows_new_ones() {
    let app = app();
    setup(&app, "private", "public");
    let session = admin_session(&app, "editor").await;
    let (s, h, _) = post_form(&app, "/notes/new", None, &[("text", "表单留言"), ("stars", "4"), ("next", "/thanks")]).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(h[header::LOCATION], "/thanks?sent=1");
    let (_, _, page) = get_with(&app, "/admin", Some(&session.0)).await;
    assert!(page.contains("1 条新"), "the nav shows the new submission");
    let (_, _, list) = get_with(&app, "/admin/c/notes", Some(&session.0)).await;
    assert!(list.contains("表单留言"));
    let (_, _, page) = get_with(&app, "/admin", Some(&session.0)).await;
    assert!(!page.contains("条新"), "seen after opening the list");
    let (s, _, body) = post_form(&app, "/notes/new", None, &[("stars", "x")]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(body.contains("没有提交成功"));
}

#[tokio::test]
async fn members_only_content_on_the_site() {
    let app = app();
    setup(&app, "members", "none");
    ok(&app, &builder(), "write_site_file", json!({"path": "index.html", "note": "x",
        "content": "{% for n in collection('notes') %}<p>{{ n.text }}</p>{% else %}<p>登录后可见</p>{% endfor %}"}));
    publish_all(&app);
    let (_, _, anon) = get_with(&app, "/", None).await;
    assert!(anon.contains("登录后可见") && !anon.contains("后台写的"));
    let m = member(&app, "d@example.com").await;
    let (_, _, page) = get_with(&app, "/", Some(&m)).await;
    assert!(page.contains("后台写的"));
    assert_eq!(get_with(&app, "/notes", None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(get_with(&app, "/notes", Some(&m)).await.0, StatusCode::OK);
}

#[tokio::test]
async fn visitor_submissions_leak_nothing_and_carry_no_images() {
    let app = app();
    ok(&app, &builder(), "create_collection", json!({"name": "signups", "title": "报名", "read": "private", "create": "public", "note": "x",
        "fields": [{"name": "email", "title": "邮箱", "type": "text", "unique": true}, {"name": "intro", "title": "介绍", "type": "richtext"}]}));
    publish_all(&app);
    let (s, v) = post(&app, "/api/v1/content/signups", json!({"email": "victim@x.y", "intro": "<p>你好<img src=\"https://tracker.example/p.gif\"></p>"}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let id = v["id"].as_i64().unwrap();
    let doc: String = app.conn().query_row("SELECT published FROM records WHERE id = ?1", [id], |r| r.get(0)).unwrap();
    assert!(doc.contains("你好") && !doc.contains("<img"), "{doc}");
    let (s, v) = post(&app, "/api/v1/content/signups", json!({"email": "victim@x.y"}), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let e = v["error"].as_str().unwrap();
    assert!(e.contains("已经有人用过") && !e.contains("victim") && !e.contains(&id.to_string()), "{e}");
}

#[tokio::test]
async fn a_screen_over_a_collection_clears_its_new_badge() {
    let app = app();
    setup(&app, "private", "public");
    ok(&app, &builder(), "set_view", json!({"name": "note_log", "title": "留言记录", "collection": "notes", "columns": ["text"], "note": "x"}));
    publish_all(&app);
    let session = admin_session(&app, "builder").await;
    post(&app, "/api/v1/content/notes", json!({"text": "新留言"}), None).await;
    assert!(get_with(&app, "/admin", Some(&session.0)).await.2.contains("1 条新"));
    get_with(&app, "/admin/v/note_log", Some(&session.0)).await;
    assert!(!get_with(&app, "/admin", Some(&session.0)).await.2.contains("条新"));
}
