//! Members signing in from a client that keeps no cookies (a mini-program, a phone or desktop app): the session
//! comes back as a token and is sent as a bearer.

mod common;

use axum::http::{header, Method, Request, StatusCode};
use common::*;
use cms_harness::Shared;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn send(app: &Shared, method: Method, path: &str, body: Option<Value>, bearer: Option<&str>) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut r = Request::builder().method(method).uri(path).header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = bearer {
        r = r.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let body = body.map_or_else(axum::body::Body::empty, |b| axum::body::Body::from(b.to_string()));
    let resp = cms_harness::api::router(app.clone()).oneshot(r.body(body).unwrap()).await.unwrap();
    let (s, h) = (resp.status(), resp.headers().clone());
    let b = resp.into_body().collect().await.unwrap().to_bytes();
    (s, h, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

/// Member login on, a members-only `notes` collection members may add to and change their own, and a function.
fn site(app: &Shared) {
    ok(app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    ok(app, &builder(), "create_collection", json!({"name": "notes", "title": "留言", "read": "members", "create": "members", "update": "owner", "note": "x",
        "fields": [{"name": "text", "title": "内容", "type": "text", "required": true}]}));
    ok(app, &builder(), "write_site_file", json!({"path": "_fn/who.js", "content": "export default (req) => ({ email: req.member && req.member.email })", "note": "x"}));
    publish_all(app);
}

async fn token(app: &Shared, email: &str) -> String {
    let (s, h, v) = send(app, Method::POST, "/api/v1/members/signup", Some(json!({"email": email, "password": "long-enough-1", "session": "token"})), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(h.get(header::SET_COOKIE).is_none(), "a token session sets no cookie");
    v["token"].as_str().unwrap_or_else(|| panic!("no token: {v}")).to_string()
}

#[tokio::test]
async fn a_client_without_cookies_signs_in_for_a_token_and_a_browser_still_gets_a_cookie() {
    let app = app();
    site(&app);
    let t = token(&app, "a@example.com").await;
    assert!(t.len() >= 32);
    let (_, h, v) = send(&app, Method::POST, "/api/v1/members/login", Some(json!({"email": "a@example.com", "password": "long-enough-1"})), None).await;
    assert!(h.get(header::SET_COOKIE).is_some() && v.get("token").is_none(), "the default keeps the token out of the page's reach: {v}");
    let (s, _, v) = send(&app, Method::POST, "/api/v1/members/login", Some(json!({"email": "a@example.com", "password": "long-enough-1", "session": "token"})), None).await;
    assert_eq!((s, v["member"]["email"].as_str()), (StatusCode::OK, Some("a@example.com")));
    assert_ne!(v["token"].as_str().unwrap(), t, "each sign-in is its own session");
}

#[tokio::test]
async fn the_token_is_the_member_everywhere_a_cookie_is() {
    let app = app();
    site(&app);
    let (a, b) = (token(&app, "a@example.com").await, token(&app, "b@example.com").await);

    let (_, _, me) = send(&app, Method::GET, "/api/v1/members/me", None, Some(&a)).await;
    assert_eq!(me["member"]["email"], "a@example.com");
    assert_eq!(send(&app, Method::GET, "/api/v1/content/notes", None, None).await.0, StatusCode::UNAUTHORIZED, "without it: a visitor");
    assert_eq!(send(&app, Method::GET, "/api/v1/content/notes", None, Some(&a)).await.0, StatusCode::OK);

    let (s, _, made) = send(&app, Method::POST, "/api/v1/content/notes", Some(json!({"text": "A 写的"})), Some(&a)).await;
    assert_eq!(s, StatusCode::CREATED, "{made}");
    let path = format!("/api/v1/content/notes/{}", made["id"]);
    assert_eq!(send(&app, Method::PATCH, &path, Some(json!({"text": "B 来改"})), Some(&b)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(send(&app, Method::PATCH, &path, Some(json!({"text": "A 改了"})), Some(&a)).await.0, StatusCode::OK);
    assert_eq!(send(&app, Method::GET, &path, None, Some(&b)).await.2["text"], "A 改了");

    assert_eq!(send(&app, Method::GET, "/_fn/who", None, Some(&a)).await.2, json!({"email": "a@example.com"}), "functions see the member too");
    assert_eq!(send(&app, Method::GET, "/_fn/who", None, Some("not-a-session")).await.2, json!({"email": null}), "a made-up token is nobody");
    let k = key(&app, &[]);
    assert_eq!(send(&app, Method::GET, "/api/v1/content/notes", None, Some(&k)).await.0, StatusCode::OK, "API keys work as before");
}

#[tokio::test]
async fn signing_out_ends_the_token() {
    let app = app();
    site(&app);
    let t = token(&app, "a@example.com").await;
    assert_eq!(send(&app, Method::POST, "/api/v1/members/logout", None, Some(&t)).await.0, StatusCode::OK);
    assert_eq!(send(&app, Method::GET, "/api/v1/members/me", None, Some(&t)).await.2["member"], Value::Null);
    assert_eq!(send(&app, Method::GET, "/api/v1/content/notes", None, Some(&t)).await.0, StatusCode::UNAUTHORIZED);
}
