//! 端: the environment holds any number of them. Each is a named folder of site files with its own entry and its
//! own rule for who may come in; the site itself is the first one.

mod common;

use axum::http::{header, Request, StatusCode};
use common::*;
use cms_harness::Shared;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn member(app: &Shared, email: &str) -> String {
    let resp = cms_harness::api::router(app.clone())
        .oneshot(Request::post("/api/v1/members/signup").header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(json!({"email": email, "password": "long-enough-1"}).to_string())).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string()
}

fn file(app: &Shared, path: &str, content: &str) {
    ok(app, &builder(), "write_site_file", json!({"path": path, "content": content, "note": "写页面"}));
}

/// A site with a home page and a members-only 老师端 holding one page and one stylesheet, all published.
fn school(app: &Shared) {
    ok(app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    file(app, "index.html", "<h1>学校首页</h1>");
    ok(app, &builder(), "set_end", json!({"name": "teacher", "title": "老师端", "enter": "members", "note": "建老师端"}));
    file(app, "teacher/index.html", "<h1>老师的工作台</h1>");
    file(app, "teacher/app.css", "h1 { color: teal }");
    publish_all(app);
}

fn end<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.as_array().unwrap().iter().find(|e| e["name"] == name).unwrap_or_else(|| panic!("no end {name} in {list}"))
}

#[test]
fn the_site_is_the_first_end_and_new_ones_are_drafts_until_published() {
    let app = app();
    let list = ok(&app, &reader(), "list_ends", json!({}));
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    assert_eq!((end(&list, "")["path"].as_str(), end(&list, "")["enter"].as_str()), (Some("/"), Some("public")));

    ok(&app, &builder(), "set_end", json!({"name": "shop", "title": "商店", "note": "建商店端"}));
    file(&app, "shop/index.html", "<h1>商店</h1>");
    file(&app, "about.html", "<h1>关于</h1>");
    let list = ok(&app, &reader(), "list_ends", json!({}));
    let shop = end(&list, "shop");
    assert_eq!((shop["title"].as_str(), shop["path"].as_str(), shop["enter"].as_str(), shop["status"].as_str()), (Some("商店"), Some("/shop/"), Some("public"), Some("draft")));
    assert_eq!((shop["files"].as_i64(), end(&list, "")["files"].as_i64()), (Some(1), Some(1)), "each end counts its own files");

    publish_all(&app);
    assert_eq!(end(&ok(&app, &reader(), "list_ends", json!({})), "shop")["status"], "published");
}

#[tokio::test]
async fn a_members_end_lets_members_in_and_sends_visitors_to_log_in() {
    let app = app();
    school(&app);
    assert!(get_text(&app, "/").await.contains("学校首页"), "the site itself stays open");

    let (s, h, _) = get_with(&app, "/teacher/", None).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(h[header::LOCATION], "/account/login?next=%2Fteacher%2F");
    assert_ne!(get_with(&app, "/teacher/app.css", None).await.0, StatusCode::OK, "its files are behind the same door");

    let m = member(&app, "t@example.com").await;
    let (s, _, page) = get_with(&app, "/teacher/", Some(&m)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(page.contains("老师的工作台"), "{page}");
    assert_eq!(get_with(&app, "/teacher/app.css", Some(&m)).await.0, StatusCode::OK);
}

#[tokio::test]
async fn closing_an_end_to_visitors_takes_effect_on_pages_they_already_saw() {
    let app = app();
    ok(&app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    ok(&app, &builder(), "set_end", json!({"name": "teacher", "title": "老师端", "note": "先公开"}));
    file(&app, "teacher/index.html", "<h1>老师的工作台</h1>");
    publish_all(&app);
    assert_eq!(get_with(&app, "/teacher/", None).await.0, StatusCode::OK);
    assert_eq!(get_with(&app, "/teacher/", None).await.0, StatusCode::OK, "served from the page cache");

    ok(&app, &builder(), "set_end", json!({"name": "teacher", "enter": "members", "note": "只给会员"}));
    assert_eq!(get_with(&app, "/teacher/", None).await.0, StatusCode::OK, "a draft changes nothing live");
    publish_all(&app);
    assert_eq!(get_with(&app, "/teacher/", None).await.0, StatusCode::SEE_OTHER, "not the cached page");
}

#[tokio::test]
async fn a_members_end_stays_out_of_the_sitemap() {
    let app = app();
    school(&app);
    file(&app, "teacher/classes.html", "<h1>班级</h1>");
    file(&app, "about.html", "<h1>关于</h1>");
    publish_all(&app);
    let map = get_text(&app, "/sitemap.xml").await;
    assert!(map.contains("/about"), "{map}");
    assert!(!map.contains("teacher"), "{map}");
}

#[test]
fn an_end_is_checked_before_it_is_kept() {
    let app = app();
    for bad in ["admin", "api", "account", "uploads", "_hc", "mcp", "Teacher", "老师", "a/b", ""] {
        let e = err(&app, &builder(), "set_end", json!({"name": bad, "title": "x", "note": "x"}));
        assert!(e.contains("name") || e.contains("名字"), "{bad}: {e}");
    }
    let e = err(&app, &builder(), "set_end", json!({"name": "teacher", "note": "x"}));
    assert!(e.contains("title"), "a new end needs a title: {e}");
    let e = err(&app, &builder(), "set_end", json!({"name": "teacher", "title": "老师端", "enter": "members", "note": "x"}));
    assert!(e.contains("set_member_login"), "members need accounts first: {e}");
    assert!(err(&app, &reader(), "set_end", json!({"name": "teacher", "title": "老师端", "note": "x"})).contains("structure"), "building is for builders");
}

#[tokio::test]
async fn deleting_an_end_takes_its_files_with_it_once_published() {
    let app = app();
    school(&app);
    let m = member(&app, "t@example.com").await;
    let done = ok(&app, &builder(), "delete_end", json!({"name": "teacher", "note": "不要老师端了"}));
    assert_eq!(done["files"], 2, "{done}");
    assert_eq!(get_with(&app, "/teacher/", Some(&m)).await.0, StatusCode::OK, "still there until published");
    let pending = ok(&app, &reader(), "list_pending", json!({})).to_string();
    assert!(pending.contains("teacher/index.html") && pending.contains("teacher/app.css"), "{pending}");

    publish_all(&app);
    assert_eq!(get_with(&app, "/teacher/", Some(&m)).await.0, StatusCode::NOT_FOUND);
    assert_eq!(get_with(&app, "/teacher/", None).await.0, StatusCode::NOT_FOUND, "no door left either");
    assert_eq!(ok(&app, &reader(), "list_ends", json!({})).as_array().unwrap().len(), 1);
    assert!(err(&app, &builder(), "delete_end", json!({"name": "teacher", "note": "x"})).contains("teacher"));
}

#[tokio::test]
async fn the_admin_lists_every_end_and_opens_each_in_the_workbench() {
    let app = app();
    school(&app);
    file(&app, "teacher/classes.html", "<h1>班级</h1>");
    let (cookie, _) = admin_session(&app, "builder").await;

    let (_, _, home) = get_with(&app, "/admin", Some(&cookie)).await;
    assert!(home.contains("href=\"/admin?p=/teacher/\""), "the nav links to the end: {home}");
    assert!(home.contains("老师端") && home.contains("data-ends") && home.contains("href=\"/admin/env\""));

    let (s, _, env) = get_with(&app, "/admin/env", Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    // 老师端 is published but one of its pages is not yet: the overview says so, on its own row.
    for text in ["老师端", "/teacher/", "测试站", "<span class=\"kind pending\">有改动</span>"] {
        assert!(env.contains(text), "{text} missing from the overview: {env}");
    }
    assert!(get_with(&app, "/admin/env", None).await.0 != StatusCode::OK, "the overview needs a session");
}

#[tokio::test]
async fn an_environment_with_one_end_keeps_its_plain_nav() {
    let app = app();
    file(&app, "index.html", "<h1>首页</h1>");
    publish_all(&app);
    let (cookie, _) = admin_session(&app, "builder").await;
    let (_, _, home) = get_with(&app, "/admin", Some(&cookie)).await;
    assert!(!home.contains("data-ends") && !home.contains("/admin/env"), "nothing to switch between: no list of ends");
}
