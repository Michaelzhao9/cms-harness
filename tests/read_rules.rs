//! `read` and `create` as conditions. A read rule is also the filter of every list: a member gets the records the
//! rule lets them see, counted and paged as if the others were not there.

mod common;

use axum::http::{header, Method, Request, StatusCode};
use common::*;
use cms_harness::Shared;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn send(app: &Shared, method: Method, path: &str, body: Option<Value>, cookie: Option<&str>, key: Option<&str>) -> (StatusCode, Value) {
    let mut r = Request::builder().method(method).uri(path).header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, c);
    }
    if let Some(k) = key {
        r = r.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    let body = body.map_or_else(axum::body::Body::empty, |b| axum::body::Body::from(b.to_string()));
    let resp = cms_harness::api::router(app.clone()).oneshot(r.body(body).unwrap()).await.unwrap();
    let s = resp.status();
    let b = resp.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

async fn list(app: &Shared, cookie: Option<&str>) -> Value {
    let (s, v) = send(app, Method::GET, "/api/v1/content/homework?sort=id", None, cookie, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

fn titles(v: &Value) -> Vec<&str> {
    v["items"].as_array().unwrap().iter().map(|i| i["title"].as_str().unwrap()).collect()
}

async fn member(app: &Shared, email: &str, data: Value) -> String {
    let resp = cms_harness::api::router(app.clone())
        .oneshot(Request::post("/api/v1/members/signup").header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(json!({"email": email, "password": "long-enough-1"}).to_string())).unwrap())
        .await
        .unwrap();
    let cookie = resp.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
    let v: Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    if data.as_object().is_some_and(|o| !o.is_empty()) {
        ok(app, &editor(), "set_member_data", json!({"id": v["member"]["id"], "data": data, "note": "x"}));
    }
    cookie
}

/// A `homework` collection with the given read and create rules; two students and a teacher; each student has
/// handed in one piece, and one more was written in the admin and marked open.
async fn school(app: &Shared, read: &str, create: &str) -> (String, String, String) {
    ok(app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    ok(app, &builder(), "create_collection", json!({"name": "homework", "title": "作业", "read": read, "create": create, "note": "x", "fields": [
        {"name": "title", "title": "标题", "type": "text", "required": true, "index": true},
        {"name": "class", "title": "班级", "type": "number"},
        {"name": "open", "title": "公开", "type": "boolean"}]}));
    ok(app, &builder(), "create_record", json!({"collection": "homework", "data": {"title": "范文", "open": true}, "note": "x"}));
    publish_all(app);
    let a = member(app, "a@example.com", json!({"kind": "student", "class": 3})).await;
    let b = member(app, "b@example.com", json!({"kind": "student", "class": 4})).await;
    let t = member(app, "t@example.com", json!({"kind": "teacher"})).await;
    (a, b, t)
}

const READ: &str = "record.open == true or record.owner == member.id or member.kind == 'teacher'";

#[tokio::test]
async fn a_read_rule_filters_every_list_and_every_single_read() {
    let app = app();
    let (a, b, t) = school(&app, READ, "members").await;
    let (_, made) = send(&app, Method::POST, "/api/v1/content/homework", Some(json!({"title": "A 的作业"})), Some(&a), None).await;
    send(&app, Method::POST, "/api/v1/content/homework", Some(json!({"title": "B 的作业"})), Some(&b), None).await;
    let mine = format!("/api/v1/content/homework/{}", made["id"]);

    let seen = list(&app, Some(&a)).await;
    assert_eq!((titles(&seen), seen["total"].as_i64()), (vec!["范文", "A 的作业"], Some(2)), "what is open, and her own; the total counts only those");
    assert_eq!(titles(&list(&app, Some(&b)).await), vec!["范文", "B 的作业"]);
    assert_eq!(titles(&list(&app, Some(&t)).await), vec!["范文", "A 的作业", "B 的作业"], "the teacher sees all of it");
    assert_eq!(titles(&list(&app, None).await), vec!["范文"], "a visitor gets what the rule gives a nobody, not an error");

    assert_eq!(send(&app, Method::GET, &mine, None, Some(&a), None).await.0, StatusCode::OK);
    assert_eq!(send(&app, Method::GET, &mine, None, Some(&b), None).await.0, StatusCode::NOT_FOUND, "another's record is simply not there");
    let (_, paged) = send(&app, Method::GET, "/api/v1/content/homework?sort=id&limit=1&offset=1", None, Some(&a), None).await;
    assert_eq!(titles(&paged), vec!["A 的作业"], "paging walks the visible records");
    let (_, filtered) = send(&app, Method::GET, "/api/v1/content/homework?title=B%20%E7%9A%84%E4%BD%9C%E4%B8%9A", None, Some(&a), None).await;
    assert_eq!(filtered["total"], 0, "a filter cannot reach behind the rule");
    let k = key(&app, &[]);
    assert_eq!(send(&app, Method::GET, "/api/v1/content/homework", None, None, Some(&k)).await.1["total"], 3, "a key reads everything");
}

#[tokio::test]
async fn a_page_lists_what_the_rule_lets_its_reader_see() {
    let app = app();
    let (a, b, _) = school(&app, READ, "members").await;
    send(&app, Method::POST, "/api/v1/content/homework", Some(json!({"title": "A 的作业"})), Some(&a), None).await;
    ok(&app, &builder(), "write_site_file", json!({"path": "mine.html", "note": "x",
        "content": "<ul>{% for h in collection('homework', sort='id') %}<li>{{ h.title }}</li>{% endfor %}</ul><p>共 {{ count('homework') }} 份</p>"}));
    publish_all(&app);
    let (_, _, hers) = get_with(&app, "/mine", Some(&a)).await;
    assert!(hers.contains("A 的作业") && hers.contains("范文") && hers.contains("共 2 份"), "{hers}");
    let (_, _, his) = get_with(&app, "/mine", Some(&b)).await;
    assert!(!his.contains("A 的作业") && his.contains("共 1 份"), "{his}");
    let anon = get_text(&app, "/mine").await;
    assert!(!anon.contains("A 的作业") && anon.contains("范文"), "{anon}");
}

#[tokio::test]
async fn a_collection_behind_a_rule_does_not_leak_through_search_or_relations() {
    let app = app();
    let (a, b, _) = school(&app, "record.owner == member.id", "members").await;
    let (_, made) = send(&app, Method::POST, "/api/v1/content/homework", Some(json!({"title": "机密作业"})), Some(&a), None).await;
    ok(&app, &builder(), "create_collection", json!({"name": "comments", "title": "评语", "read": "public", "note": "x",
        "fields": [{"name": "on", "title": "哪份作业", "type": "relation", "target": "homework"}]}));
    ok(&app, &builder(), "create_record", json!({"collection": "comments", "data": {"on": made["id"]}, "note": "x"}));
    publish_all(&app);

    let (_, found) = send(&app, Method::GET, "/api/v1/search?q=%E6%9C%BA%E5%AF%86", None, Some(&b), None).await;
    assert_eq!(found["items"].as_array().map(Vec::len), Some(0), "search: {found}");
    let (_, c) = send(&app, Method::GET, "/api/v1/content/comments?expand=on", None, Some(&b), None).await;
    assert_eq!(c["items"][0]["on"], made["id"], "a relation into it stays an id, not the record: {c}");
    assert!(!get_text(&app, "/sitemap.xml").await.contains("homework"));
}

#[tokio::test]
async fn a_create_rule_decides_who_may_add_and_what() {
    let app = app();
    let (a, b, t) = school(&app, "members", "member.kind == 'student' and data.class == member.class").await;
    let add = |class: i64, who: Option<String>| {
        let app = app.clone();
        async move { send(&app, Method::POST, "/api/v1/content/homework", Some(json!({"title": "作业", "class": class})), who.as_deref(), None).await.0 }
    };
    assert_eq!(add(3, Some(a.clone())).await, StatusCode::CREATED, "a student, into her own class");
    assert_eq!(add(4, Some(a)).await, StatusCode::FORBIDDEN, "not into another class");
    assert_eq!(add(4, Some(b)).await, StatusCode::CREATED);
    assert_eq!(add(3, Some(t)).await, StatusCode::FORBIDDEN, "the teacher is not a student");
    assert_eq!(add(3, None).await, StatusCode::UNAUTHORIZED, "a visitor is asked to log in");
}

#[test]
fn rules_are_kept_as_written_and_checked_when_saved() {
    let app = app();
    let fields = json!([{"name": "title", "title": "标题", "type": "text"}]);
    ok(&app, &builder(), "create_collection", json!({"name": "a", "title": "A", "read": "record.owner == member.id", "create": "member.kind == 'x'", "note": "x", "fields": fields}));
    let def = ok(&app, &reader(), "get_collection", json!({"name": "a"})).to_string();
    assert!(def.contains("\"read\":\"record.owner == member.id\"") && def.contains("\"create\":\"member.kind == 'x'\""), "{def}");
    ok(&app, &builder(), "update_collection", json!({"name": "a", "read": "public", "create": "none", "note": "x"}));
    let def = ok(&app, &reader(), "get_collection", json!({"name": "a"})).to_string();
    assert!(def.contains("\"read\":\"public\"") && def.contains("\"create\":\"none\""), "a keyword replaces the rule: {def}");

    for (field, bad, names) in [("read", "record.nope == 1", "nope"), ("read", "record.title ==", "=="), ("create", "record.title == 'x'", "data"), ("create", "owner", "owner")] {
        let e = err(&app, &builder(), "update_collection", json!({"name": "a", field: bad, "note": "x"}));
        assert!(e.contains(names) && e.contains(field), "{field} = {bad}: {e}");
    }
}
