mod common;

use axum::http::{header, HeaderMap, Request, StatusCode};
use common::*;
use cms_harness::Shared;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// POST JSON from the site; returns status, the member cookie pair if one was set, and the body.
async fn api(app: &Shared, path: &str, body: Value, cookie: Option<&str>) -> (StatusCode, Option<String>, Value) {
    let mut req = Request::post(path).header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    let resp = cms_harness::api::router(app.clone()).oneshot(req.body(axum::body::Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = resp.status();
    let set = resp.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, set, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn login_on(app: &Shared) {
    ok(app, &builder(), "set_member_login", json!({"enabled": true, "note": "网站加登录"}));
    publish_all(app);
}

async fn signup(app: &Shared, email: &str) -> String {
    let (s, cookie, v) = api(app, "/api/v1/members/signup", json!({"email": email, "password": "long-enough-1", "name": "小王"}), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    cookie.expect("a member cookie")
}

#[tokio::test]
async fn everything_is_404_until_member_login_is_live() {
    let app = app();
    let (s, _, _) = api(&app, "/api/v1/members/signup", json!({"email": "a@b.co", "password": "long-enough-1"}), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(get_with(&app, "/account/login", None).await.0, StatusCode::NOT_FOUND);
    // A draft is not live yet.
    ok(&app, &builder(), "set_member_login", json!({"enabled": true, "note": "草稿"}));
    assert_eq!(api(&app, "/api/v1/members/signup", json!({"email": "a@b.co", "password": "long-enough-1"}), None).await.0, StatusCode::NOT_FOUND);
    publish_all(&app);
    let (s, _, page) = get_with(&app, "/account/login", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(page.contains("action=\"/account/login\""));
}

#[tokio::test]
async fn signup_login_me_and_logout() {
    let app = app();
    login_on(&app);
    let cookie = signup(&app, " Wang@Example.COM ").await;
    assert!(cookie.starts_with("hc_member="));
    let (s, _, me) = get(&app, "/api/v1/members/me", None).await;
    assert_eq!((s, me["member"].clone()), (StatusCode::OK, Value::Null));
    let (_, _, body) = get_with(&app, "/api/v1/members/me", Some(&cookie)).await;
    let me: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(me["member"]["email"], "wang@example.com");

    let (s, _, _) = api(&app, "/api/v1/members/logout", json!({}), Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    let (_, _, body) = get_with(&app, "/api/v1/members/me", Some(&cookie)).await;
    assert!(serde_json::from_str::<Value>(&body).unwrap()["member"].is_null(), "the session is gone");

    let (s, again, v) = api(&app, "/api/v1/members/login", json!({"email": "wang@example.com", "password": "long-enough-1"}), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(again.is_some());
    let (s, _, v) = api(&app, "/api/v1/members/login", json!({"email": "wang@example.com", "password": "wrong-password"}), None).await;
    assert_eq!((s, v["error"].as_str()), (StatusCode::UNAUTHORIZED, Some(cms_harness::members::BAD_LOGIN)));
}

#[tokio::test]
async fn bad_signups_are_refused() {
    let app = app();
    login_on(&app);
    signup(&app, "dup@example.com").await;
    let (s, _, v) = api(&app, "/api/v1/members/signup", json!({"email": "DUP@example.com", "password": "long-enough-1"}), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (s, _, v) = api(&app, "/api/v1/members/signup", json!({"email": "x@example.com", "password": "short"}), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v["error"].as_str().unwrap().contains("10"));
    assert_eq!(api(&app, "/api/v1/members/signup", json!({"email": "not-an-email", "password": "long-enough-1"}), None).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn failed_logins_lock_the_email() {
    let app = app();
    login_on(&app);
    signup(&app, "lock@example.com").await;
    for _ in 0..5 {
        api(&app, "/api/v1/members/login", json!({"email": "lock@example.com", "password": "nope-nope-nope"}), None).await;
    }
    let (s, _, _) = api(&app, "/api/v1/members/login", json!({"email": "lock@example.com", "password": "long-enough-1"}), None).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "even the right password waits");
}

#[tokio::test]
async fn disabled_members_are_locked_out_and_their_sessions_end() {
    let app = app();
    login_on(&app);
    let cookie = signup(&app, "gone@example.com").await;
    let id = ok(&app, &builder(), "list_members", json!({}))["items"][0]["id"].as_i64().unwrap();
    ok(&app, &editor(), "set_member_status", json!({"id": id, "status": "disabled", "note": "违规"}));
    let (_, _, body) = get_with(&app, "/api/v1/members/me", Some(&cookie)).await;
    assert!(serde_json::from_str::<Value>(&body).unwrap()["member"].is_null());
    let (s, _, _) = api(&app, "/api/v1/members/login", json!({"email": "gone@example.com", "password": "long-enough-1"}), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_member_cookie_opens_nothing_in_the_admin_and_hashes_never_leave() {
    let app = app();
    login_on(&app);
    let cookie = signup(&app, "m@example.com").await;
    let (s, h, _) = get_with(&app, "/admin", Some(&cookie)).await;
    assert!(s == StatusCode::SEE_OTHER || s == StatusCode::FOUND, "{s}");
    assert!(h[header::LOCATION].to_str().unwrap().contains("/admin/login"));
    let list = ok(&app, &reader(), "list_members", json!({}));
    let text = list.to_string();
    assert!(text.contains("m@example.com") && !text.contains("argon2") && !text.contains("password"));
}

#[tokio::test]
async fn the_admin_nav_follows_member_login_both_ways() {
    let app = app();
    let session = admin_session(&app, "builder").await;
    let (_, _, page) = get_with(&app, "/admin", Some(&session.0)).await;
    assert!(!page.contains("用户管理"), "no login, no 用户管理");
    assert_eq!(get_with(&app, "/admin/v/members", Some(&session.0)).await.0, StatusCode::NOT_FOUND);

    // Turning login on (even as a draft) brings it; the page lists members.
    ok(&app, &builder(), "set_member_login", json!({"enabled": true, "note": "加登录"}));
    let (_, _, page) = get_with(&app, "/admin", Some(&session.0)).await;
    assert!(page.contains("/admin/v/members") && page.contains("用户管理"));
    publish_all(&app);
    signup(&app, "list@example.com").await;
    let (s, _, page) = get_with(&app, "/admin/v/members", Some(&session.0)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(page.contains("list@example.com") && page.contains("停用"));

    // The chat can reshape it like any screen.
    ok(&app, &builder(), "set_view", json!({"name": "members", "title": "会员", "collection": "members", "columns": ["name", "email"], "note": "精简"}));
    let (_, _, page) = get_with(&app, "/admin/v/members", Some(&session.0)).await;
    assert!(page.contains("<h1>会员</h1>") && !page.contains("注册时间"));

    // Off again: gone.
    ok(&app, &builder(), "set_member_login", json!({"enabled": false, "note": "不要登录了"}));
    let (_, _, page) = get_with(&app, "/admin", Some(&session.0)).await;
    assert!(!page.contains("/admin/v/members"));
}

#[tokio::test]
async fn admins_can_disable_from_the_members_page() {
    let app = app();
    login_on(&app);
    signup(&app, "x@example.com").await;
    let session = admin_session(&app, "editor").await;
    let id = ok(&app, &builder(), "list_members", json!({}))["items"][0]["id"].as_i64().unwrap().to_string();
    let (s, _, _) = post_form(&app, &format!("/admin/members/{id}/status"), Some(&session), &[("status", "disabled")]).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(ok(&app, &builder(), "list_members", json!({}))["items"][0]["status"], "disabled");
}

#[tokio::test]
async fn site_forms_log_in_and_pages_see_the_member() {
    let app = app();
    login_on(&app);
    ok(&app, &builder(), "write_site_file", json!({"path": "index.html", "content": "<p>{% if member %}你好 {{ member.email }}{% else %}请登录{% endif %}</p>", "note": "首页"}));
    publish_all(&app);
    let (s, h, _) = post_form(&app, "/account/signup", None, &[("email", "page@example.com"), ("password", "long-enough-1"), ("next", "//evil.example/x")]).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(h[header::LOCATION], "/", "next may not leave the site");
    let cookie = h[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
    assert!(h[header::SET_COOKIE].to_str().unwrap().contains("SameSite=Lax"));
    let (_, hh, page) = get_with(&app, "/", Some(&cookie)).await;
    assert!(page.contains("你好 page@example.com"));
    assert!(hh[header::CACHE_CONTROL].to_str().unwrap().contains("private"));
    let (_, _, anon) = get_with(&app, "/", None).await;
    assert!(anon.contains("请登录"));
    // A wrong password shows the form again with the error.
    let (s, _, page) = post_form(&app, "/account/login", None, &[("email", "page@example.com"), ("password", "wrong-password")]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(page.contains(cms_harness::members::BAD_LOGIN) && page.contains("page@example.com"));
    let _ = HeaderMap::new();
}

#[test]
fn members_is_a_reserved_collection_name() {
    let app = app();
    let e = err(&app, &builder(), "create_collection", json!({"name": "members", "title": "会员", "fields": [{"name": "email", "title": "邮箱", "type": "text"}], "note": "x"}));
    assert!(e.contains("set_member_login"));
}
