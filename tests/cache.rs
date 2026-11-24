mod common;
use axum::http::Request;
use common::*;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

/// (x-cms-cache, body)
async fn fetch(app: &cms_harness::Shared, path: &str) -> (String, String) {
    let resp = cms_harness::api::router(app.clone()).oneshot(Request::get(path).body(axum::body::Body::empty()).unwrap()).await.unwrap();
    let cache = resp.headers().get("x-cms-cache").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
    (cache, String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned())
}

fn write(app: &cms_harness::Shared, path: &str, content: &str) {
    ok(app, &builder(), "write_site_file", json!({ "path": path, "content": content, "note": "页面" }));
}

fn collection(app: &cms_harness::Shared, name: &str) {
    ok(app, &builder(), "create_collection", json!({ "name": name, "title": name, "read": "public", "create": "public", "note": "建",
        "fields": [{ "name": "title", "title": "标题", "type": "text" }] }));
}

fn record(app: &cms_harness::Shared, c: &str, title: &str) -> i64 {
    ok(app, &builder(), "create_record", json!({ "collection": c, "note": "写", "data": { "title": title } }))["id"].as_i64().unwrap()
}

#[tokio::test]
async fn a_publish_recomputes_only_the_pages_it_touched() {
    let app = app();
    collection(&app, "news");
    collection(&app, "jobs");
    record(&app, "news", "First news");
    write(&app, "index.html", "{% for n in collection('news') %}<p>{{ n.title }}</p>{% endfor %}");
    write(&app, "jobs.html", "{% for j in collection('jobs') %}<p>{{ j.title }}</p>{% endfor %}");
    publish_all(&app);
    assert_eq!(fetch(&app, "/").await.0, "miss");
    assert_eq!(fetch(&app, "/").await.0, "hit");
    assert_eq!(fetch(&app, "/jobs").await.0, "miss");

    // Something the home page does not read: it stays cached.
    record(&app, "jobs", "Drummer wanted");
    publish_all(&app);
    assert_eq!(fetch(&app, "/").await.0, "hit");
    let (state, page) = fetch(&app, "/jobs").await;
    assert_eq!(state, "miss");
    assert!(page.contains("Drummer wanted"));

    // A draft changes nothing live.
    let id = record(&app, "news", "Second news");
    assert_eq!(fetch(&app, "/").await.0, "hit");
    // Its publish does.
    ok(&app, &builder(), "publish", json!({ "note": "x", "targets": [format!("record:{id}")] }));
    let (state, page) = fetch(&app, "/").await;
    assert_eq!(state, "miss");
    assert!(page.contains("Second news"));
    assert_eq!(fetch(&app, "/jobs").await.0, "hit", "the jobs page did not read news");

    // A visitor's submission is live at once, and so is the page that lists it.
    let (s, _) = post_json(&app, "/api/v1/content/news", None, json!({ "title": "From a visitor" })).await;
    assert_eq!(s, axum::http::StatusCode::CREATED);
    let (state, page) = fetch(&app, "/").await;
    assert_eq!(state, "miss");
    assert!(page.contains("From a visitor"));

    // The site name is in every page's settings: everything is recomputed.
    ok(&app, &builder(), "set_site_settings", json!({ "name": "新名字", "note": "改名" }));
    assert_eq!(fetch(&app, "/jobs").await.0, "hit", "a draft setting changes nothing");
    publish_all(&app);
    assert_eq!(fetch(&app, "/jobs").await.0, "miss");
}

#[tokio::test]
async fn files_and_addresses_are_part_of_what_a_page_depends_on() {
    let app = app();
    collection(&app, "news");
    record(&app, "news", "About");
    write(&app, "_item.html", "<h1>record page {{ item.title }}</h1>");
    write(&app, "layout.html", "<main>{% block main %}{% endblock %}</main>v1");
    write(&app, "page.html", "{% extends 'layout.html' %}{% block main %}page{% endblock %}");
    publish_all(&app);
    assert_eq!(fetch(&app, "/news/about").await.0, "miss");
    assert_eq!(fetch(&app, "/page").await.0, "miss");
    assert_eq!(fetch(&app, "/page").await.0, "hit");
    // A layout the page extends.
    write(&app, "layout.html", "<main>{% block main %}{% endblock %}</main>v2");
    publish_all(&app);
    let (state, page) = fetch(&app, "/page").await;
    assert_eq!(state, "miss");
    assert!(page.contains("v2"));
    // A site file that now answers an address a record page used to.
    write(&app, "news/about.html", "<h1>a file wins</h1>");
    publish_all(&app);
    let (state, page) = fetch(&app, "/news/about").await;
    assert_eq!(state, "miss");
    assert!(page.contains("a file wins"), "{page}");
}

#[tokio::test]
async fn signed_in_members_and_the_preview_are_never_cached() {
    let app = app();
    write(&app, "index.html", "<p>home</p>");
    publish_all(&app);
    fetch(&app, "/").await;
    assert_eq!(fetch(&app, "/").await.0, "hit");
    let s = admin_session(&app, "builder").await;
    let base = preview_base(&app, &s.0).await;
    assert_eq!(fetch(&app, &format!("{base}/")).await.0, "", "the preview has no cache");
}
