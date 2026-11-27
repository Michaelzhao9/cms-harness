//! Members changing and removing records from the site: the collection's `update` and `delete` rules.

mod common;

use axum::http::{header, Method, Request, StatusCode};
use common::*;
use cms_harness::Shared;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn send(app: &Shared, method: Method, path: &str, body: Option<Value>, cookie: Option<&str>, origin: Option<&str>) -> (StatusCode, Value) {
    let mut r = Request::builder().method(method).uri(path).header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, c);
    }
    if let Some(o) = origin {
        r = r.header(header::ORIGIN, o);
    }
    let body = body.map_or_else(axum::body::Body::empty, |b| axum::body::Body::from(b.to_string()));
    let resp = cms_harness::api::router(app.clone()).oneshot(r.body(body).unwrap()).await.unwrap();
    let s = resp.status();
    let b = resp.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

async fn patch(app: &Shared, id: i64, body: Value, cookie: Option<&str>) -> (StatusCode, Value) {
    send(app, Method::PATCH, &format!("/api/v1/content/notes/{id}"), Some(body), cookie, None).await
}

async fn remove(app: &Shared, id: i64, cookie: Option<&str>) -> (StatusCode, Value) {
    send(app, Method::DELETE, &format!("/api/v1/content/notes/{id}"), None, cookie, None).await
}

async fn item(app: &Shared, id: i64) -> (StatusCode, Value) {
    send(app, Method::GET, &format!("/api/v1/content/notes/{id}"), None, None, None).await
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

/// A public `notes` collection members may add to, with the given `update` and `delete` rules, all published.
fn setup(app: &Shared, update: Option<&str>, delete: Option<&str>) {
    ok(app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    let mut args = json!({"name": "notes", "title": "留言", "read": "public", "create": "members", "note": "x",
        "fields": [{"name": "text", "title": "内容", "type": "text", "required": true}, {"name": "stars", "title": "评分", "type": "number"}]});
    if let Some(u) = update {
        args["update"] = json!(u);
    }
    if let Some(d) = delete {
        args["delete"] = json!(d);
    }
    ok(app, &builder(), "create_collection", args);
    publish_all(app);
}

/// A note written by the member behind `cookie`.
async fn note(app: &Shared, cookie: &str, text: &str, stars: i64) -> i64 {
    let (s, v) = send(app, Method::POST, "/api/v1/content/notes", Some(json!({"text": text, "stars": stars})), Some(cookie), None).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v["id"].as_i64().unwrap()
}

#[tokio::test]
async fn the_owner_changes_their_own_record_and_it_is_live_at_once() {
    let app = app();
    setup(&app, Some("owner"), None);
    let a = member(&app, "a@example.com").await;
    let id = note(&app, &a, "第一版", 3).await;

    let (s, v) = patch(&app, id, json!({"text": "改过了"}), Some(&a)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, now) = item(&app, id).await;
    assert_eq!((now["text"].as_str(), now["stars"].as_i64()), (Some("改过了"), Some(3)), "only the sent field changes");
    let draft: Option<String> = app.conn().query_row("SELECT draft FROM records WHERE id = ?1", [id], |r| r.get(0)).unwrap();
    assert_eq!(draft, None, "no draft: a member's change is live");
    let (actor, prev): (String, Option<String>) = app
        .conn()
        .query_row("SELECT actor, prev FROM versions WHERE target = ?1 ORDER BY id DESC LIMIT 1", [format!("record:{id}")], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert!(actor.starts_with("member:"), "{actor}");
    assert!(prev.unwrap().contains("第一版"), "history keeps what it was");
}

#[tokio::test]
async fn others_and_visitors_cannot_change_it() {
    let app = app();
    setup(&app, Some("owner"), None);
    let a = member(&app, "a@example.com").await;
    let b = member(&app, "b@example.com").await;
    let id = note(&app, &a, "A 的", 3).await;

    assert_eq!(patch(&app, id, json!({"text": "B 来改"}), Some(&b)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(patch(&app, id, json!({"text": "访客来改"}), None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(patch(&app, 9999, json!({"text": "不存在"}), Some(&b)).await.0, StatusCode::FORBIDDEN, "same answer as a record that exists");
    assert_eq!(item(&app, id).await.1["text"], "A 的");
}

#[tokio::test]
async fn without_a_rule_nobody_changes_or_removes_records_from_the_site() {
    let app = app();
    setup(&app, None, None);
    let a = member(&app, "a@example.com").await;
    let id = note(&app, &a, "A 的", 3).await;
    assert_eq!(patch(&app, id, json!({"text": "改"}), Some(&a)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(remove(&app, id, Some(&a)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(item(&app, id).await.1["text"], "A 的");
}

#[tokio::test]
async fn a_rule_can_keep_one_field_from_changing() {
    let app = app();
    setup(&app, Some("record.owner == member.id and data.stars == record.stars"), None);
    let a = member(&app, "a@example.com").await;
    let id = note(&app, &a, "A 的", 3).await;
    assert_eq!(patch(&app, id, json!({"stars": 5}), Some(&a)).await.0, StatusCode::FORBIDDEN, "stars may not change");
    assert_eq!(patch(&app, id, json!({"text": "只改文字"}), Some(&a)).await.0, StatusCode::OK);
    let (_, now) = item(&app, id).await;
    assert_eq!((now["text"].as_str(), now["stars"].as_i64()), (Some("只改文字"), Some(3)));
}

#[tokio::test]
async fn a_change_is_validated_like_any_other_write() {
    let app = app();
    setup(&app, Some("owner"), None);
    let a = member(&app, "a@example.com").await;
    let id = note(&app, &a, "A 的", 3).await;
    assert_eq!(patch(&app, id, json!({"nope": 1}), Some(&a)).await.0, StatusCode::BAD_REQUEST, "unknown field");
    assert_eq!(patch(&app, id, json!({"text": ""}), Some(&a)).await.0, StatusCode::BAD_REQUEST, "required field emptied");
    assert_eq!(patch(&app, id, json!({"stars": "很多"}), Some(&a)).await.0, StatusCode::BAD_REQUEST, "wrong type");
    assert_eq!(item(&app, id).await.1["text"], "A 的");
}

#[tokio::test]
async fn the_owner_removes_their_own_record() {
    let app = app();
    setup(&app, None, Some("owner"));
    let a = member(&app, "a@example.com").await;
    let b = member(&app, "b@example.com").await;
    let id = note(&app, &a, "A 的", 3).await;

    assert_eq!(remove(&app, id, Some(&b)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(remove(&app, id, None).await.0, StatusCode::UNAUTHORIZED);
    let (s, v) = remove(&app, id, Some(&a)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(item(&app, id).await.0, StatusCode::NOT_FOUND);
    let (doc, prev): (Option<String>, Option<String>) = app
        .conn()
        .query_row("SELECT doc, prev FROM versions WHERE target = ?1 ORDER BY id DESC LIMIT 1", [format!("record:{id}")], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert!(doc.is_none() && prev.unwrap().contains("A 的"), "history can bring it back");
}

#[tokio::test]
async fn a_record_other_live_records_point_at_stays() {
    let app = app();
    setup(&app, None, Some("owner"));
    ok(&app, &builder(), "create_collection", json!({"name": "replies", "title": "回复", "note": "x",
        "fields": [{"name": "to", "title": "回复哪条", "type": "relation", "target": "notes"}]}));
    let a = member(&app, "a@example.com").await;
    let id = note(&app, &a, "A 的", 3).await;
    ok(&app, &builder(), "create_record", json!({"collection": "replies", "data": {"to": id}, "note": "x"}));
    publish_all(&app);

    let (s, v) = remove(&app, id, Some(&a)).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(item(&app, id).await.0, StatusCode::OK);
    publish_all_is_still_possible(&app);
}

/// Publishing checks every live relation: a removal that left one dangling would block all later publishes.
fn publish_all_is_still_possible(app: &Shared) {
    ok(app, &builder(), "create_record", json!({"collection": "notes", "data": {"text": "后台写的"}, "note": "x"}));
    publish_all(app);
}

#[tokio::test]
async fn a_pending_admin_draft_gets_the_members_change_too() {
    let app = app();
    setup(&app, Some("owner"), None);
    let a = member(&app, "a@example.com").await;
    let id = note(&app, &a, "第一版", 3).await;
    ok(&app, &builder(), "update_record", json!({"id": id, "data": {"stars": 4}, "note": "后台改评分"}));

    assert_eq!(patch(&app, id, json!({"text": "会员改文字"}), Some(&a)).await.0, StatusCode::OK);
    publish_all(&app);
    let (_, now) = item(&app, id).await;
    assert_eq!((now["text"].as_str(), now["stars"].as_i64()), (Some("会员改文字"), Some(4)), "publishing the draft does not undo the member's change");
}

#[tokio::test]
async fn a_request_from_another_site_is_refused() {
    let app = app();
    setup(&app, Some("owner"), Some("owner"));
    let a = member(&app, "a@example.com").await;
    let id = note(&app, &a, "A 的", 3).await;
    let path = format!("/api/v1/content/notes/{id}");
    assert_eq!(send(&app, Method::PATCH, &path, Some(json!({"text": "别的网站"})), Some(&a), Some("https://evil.example")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(send(&app, Method::DELETE, &path, None, Some(&a), Some("https://evil.example")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(item(&app, id).await.1["text"], "A 的");
}

#[test]
fn a_rule_that_does_not_parse_or_names_a_missing_field_is_refused_when_the_collection_is_saved() {
    let app = app();
    let fields = json!([{"name": "text", "title": "内容", "type": "text"}]);
    let e = err(&app, &builder(), "create_collection", json!({"name": "a", "title": "A", "update": "record.owner ==", "note": "x", "fields": fields}));
    assert!(e.contains("update"), "{e}");
    let e = err(&app, &builder(), "create_collection", json!({"name": "b", "title": "B", "delete": "record.nope == 1", "note": "x", "fields": fields}));
    assert!(e.contains("nope"), "{e}");
    ok(&app, &builder(), "create_collection", json!({"name": "c", "title": "C", "update": "owner", "delete": "record.text == 'x' or member", "note": "x", "fields": fields}));
}

#[test]
fn update_collection_sets_and_clears_the_rules() {
    let app = app();
    setup(&app, Some("owner"), None);
    ok(&app, &builder(), "update_collection", json!({"name": "notes", "delete": "owner", "update": "none", "note": "改规则"}));
    let def = ok(&app, &builder(), "get_collection", json!({"name": "notes"}));
    let text = def.to_string();
    assert!(text.contains("\"delete\":\"owner\""), "{text}");
    assert!(!text.contains("\"update\""), "none clears the rule: {text}");
}
