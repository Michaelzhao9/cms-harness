//! Members carry attributes the builder defines (`kind: teacher`, `class: 3`). The kernel gives them no meaning:
//! rules, the door of an 端, pages and functions read them.

mod common;

use axum::http::{header, Method, Request, StatusCode};
use common::*;
use cms_harness::Shared;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn send(app: &Shared, method: Method, path: &str, body: Option<Value>, cookie: Option<&str>) -> (StatusCode, Value) {
    let mut r = Request::builder().method(method).uri(path).header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, c);
    }
    let body = body.map_or_else(axum::body::Body::empty, |b| axum::body::Body::from(b.to_string()));
    let resp = cms_harness::api::router(app.clone()).oneshot(r.body(body).unwrap()).await.unwrap();
    let s = resp.status();
    let b = resp.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&b).unwrap_or_else(|_| json!(String::from_utf8_lossy(&b))))
}

/// Signs a member up; returns (cookie, id).
async fn member(app: &Shared, email: &str, extra: Value) -> (String, i64) {
    let mut body = json!({"email": email, "password": "long-enough-1"});
    body.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
    let resp = cms_harness::api::router(app.clone())
        .oneshot(Request::post("/api/v1/members/signup").header(header::CONTENT_TYPE, "application/json").body(axum::body::Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = resp.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
    let v: Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (cookie, v["member"]["id"].as_i64().unwrap())
}

fn login_on(app: &Shared) {
    ok(app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    publish_all(app);
}

fn give(app: &Shared, id: i64, data: Value) -> Value {
    ok(app, &editor(), "set_member_data", json!({"id": id, "data": data, "note": "设属性"}))
}

#[tokio::test]
async fn a_builder_gives_a_member_attributes_and_the_member_cannot() {
    let app = app();
    login_on(&app);
    let (cookie, id) = member(&app, "t@example.com", json!({"kind": "teacher", "data": {"kind": "teacher"}})).await;
    let me = send(&app, Method::GET, "/api/v1/members/me", None, Some(&cookie)).await.1;
    assert!(me["member"].get("kind").is_none(), "signing up sets no attributes: {me}");

    assert_eq!(give(&app, id, json!({"kind": "teacher", "class": 3}))["data"], json!({"kind": "teacher", "class": 3}));
    assert_eq!(give(&app, id, json!({"class": null, "senior": true}))["data"], json!({"kind": "teacher", "senior": true}), "given keys change, null removes, the rest stay");
    let me = send(&app, Method::GET, "/api/v1/members/me", None, Some(&cookie)).await.1;
    assert_eq!((me["member"]["kind"].as_str(), me["member"]["email"].as_str()), (Some("teacher"), Some("t@example.com")));
    let listed = ok(&app, &reader(), "list_members", json!({}));
    assert_eq!(listed["items"][0]["data"], json!({"kind": "teacher", "senior": true}));

    for bad in [json!({"email": "x@y.z"}), json!({"id": 9}), json!({"password": "x"}), json!({"Kind": "x"}), json!({"kind": {"deep": 1}}), json!({"kind": ["a"]})] {
        let e = err(&app, &editor(), "set_member_data", json!({"id": id, "data": bad, "note": "x"}));
        assert!(e.contains("属性"), "{bad}: {e}");
    }
    assert!(err(&app, &reader(), "set_member_data", json!({"id": id, "data": {"kind": "x"}, "note": "x"})).contains("write"));
    assert!(err(&app, &editor(), "set_member_data", json!({"id": 999, "data": {"kind": "x"}, "note": "x"})).contains("999"));
}

#[tokio::test]
async fn rules_read_a_members_attributes() {
    let app = app();
    ok(&app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    ok(&app, &builder(), "create_collection", json!({"name": "homework", "title": "作业", "read": "members", "create": "members", "note": "x",
        "update": "member.kind == 'teacher' and record.class == member.class",
        "fields": [{"name": "title", "title": "标题", "type": "text", "required": true}, {"name": "class", "title": "班级", "type": "number"}, {"name": "score", "title": "分数", "type": "number"}]}));
    publish_all(&app);
    let (student, _) = member(&app, "s@example.com", json!({})).await;
    let (teacher, tid) = member(&app, "t@example.com", json!({})).await;
    let (other, oid) = member(&app, "o@example.com", json!({})).await;
    give(&app, tid, json!({"kind": "teacher", "class": 3}));
    give(&app, oid, json!({"kind": "teacher", "class": 4}));
    let (_, made) = send(&app, Method::POST, "/api/v1/content/homework", Some(json!({"title": "第一课", "class": 3})), Some(&student)).await;
    let path = format!("/api/v1/content/homework/{}", made["id"]);

    assert_eq!(send(&app, Method::PATCH, &path, Some(json!({"score": 100})), Some(&student)).await.0, StatusCode::FORBIDDEN, "a student does not grade");
    assert_eq!(send(&app, Method::PATCH, &path, Some(json!({"score": 60})), Some(&other)).await.0, StatusCode::FORBIDDEN, "nor the teacher of another class");
    assert_eq!(send(&app, Method::PATCH, &path, Some(json!({"score": 90})), Some(&teacher)).await.0, StatusCode::OK);
    assert_eq!(send(&app, Method::GET, &path, None, Some(&student)).await.1["score"], 90);

    let e = err(&app, &builder(), "update_collection", json!({"name": "homework", "update": "member.password == 'x'", "note": "x"}));
    assert!(e.contains("password"), "{e}");
}

#[tokio::test]
async fn the_door_of_an_end_can_be_a_condition_on_the_member() {
    let app = app();
    ok(&app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    ok(&app, &builder(), "set_end", json!({"name": "teacher", "title": "老师端", "enter": "member.kind == 'teacher'", "note": "建老师端"}));
    ok(&app, &builder(), "write_site_file", json!({"path": "teacher/index.html", "content": "<h1>{{ member.kind }} 的工作台</h1>", "note": "x"}));
    ok(&app, &builder(), "write_site_file", json!({"path": "teacher/_fn/who.js", "content": "export default (req) => ({ kind: req.member.kind })", "note": "x"}));
    ok(&app, &builder(), "write_site_file", json!({"path": "about.html", "content": "<h1>关于</h1>", "note": "x"}));
    publish_all(&app);
    let (student, _) = member(&app, "s@example.com", json!({})).await;
    let (teacher, tid) = member(&app, "t@example.com", json!({})).await;
    give(&app, tid, json!({"kind": "teacher"}));

    let (s, h, _) = get_with(&app, "/teacher/", None).await;
    assert_eq!((s, h[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/account/login?next=%2Fteacher%2F"), "a visitor goes to log in");
    assert_eq!(get_with(&app, "/teacher/", Some(&student)).await.0, StatusCode::FORBIDDEN, "a member who does not fit is told no, not sent to log in again");
    let (s, _, page) = get_with(&app, "/teacher/", Some(&teacher)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(page.contains("teacher 的工作台"), "pages see the attributes: {page}");

    assert_eq!(send(&app, Method::GET, "/teacher/_fn/who", None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&app, Method::GET, "/teacher/_fn/who", None, Some(&student)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(send(&app, Method::GET, "/teacher/_fn/who", None, Some(&teacher)).await.1, json!({"kind": "teacher"}));

    let map = get_text(&app, "/sitemap.xml").await;
    assert!(map.contains("/about") && !map.contains("teacher"), "{map}");
    assert_eq!(ok(&app, &reader(), "list_ends", json!({}))[1]["enter"], "member.kind == 'teacher'");
}

#[test]
fn the_door_of_an_end_is_about_the_member_only() {
    let app = app();
    ok(&app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    for (bad, names) in [("record.owner == member.id", "record"), ("data.x == 1", "data"), ("member.kind ==", "=="), ("owner", "owner")] {
        let e = err(&app, &builder(), "set_end", json!({"name": "t", "title": "T", "enter": bad, "note": "x"}));
        assert!(e.contains(names), "{bad}: {e}");
    }
    for good in ["public", "members", "member.kind == 'teacher' or member.senior"] {
        ok(&app, &builder(), "set_end", json!({"name": "t", "title": "T", "enter": good, "note": "x"}));
    }
}

#[tokio::test]
async fn the_admin_sets_attributes_by_hand_on_the_members_page() {
    let app = app();
    login_on(&app);
    let (_, id) = member(&app, "t@example.com", json!({})).await;
    give(&app, id, json!({"old": "x"}));
    let session = admin_session(&app, "editor").await;
    let (s, _, body) = post_form(&app, &format!("/admin/members/{id}/data"), Some(&session), &[("data", "kind=teacher, class=3，senior=true")]).await;
    assert_eq!(s, StatusCode::SEE_OTHER, "{body}");
    let listed = ok(&app, &reader(), "list_members", json!({}));
    assert_eq!(listed["items"][0]["data"], json!({"kind": "teacher", "class": 3, "senior": true}), "what is typed is the whole set: the old attribute is gone");
    let (_, _, page) = get_with(&app, "/admin/members", Some(&session.0)).await;
    assert!(page.contains("class=3") && page.contains(&format!("action=\"/admin/members/{id}/data\"")), "{page}");
    let (s, _, body) = post_form(&app, &format!("/admin/members/{id}/data"), Some(&session), &[("data", "Bad Key=1")]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
}
