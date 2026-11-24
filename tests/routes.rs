mod common;
use axum::http::{header, Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn fetch(app: &cms_harness::Shared, path: &str) -> (StatusCode, String, String) {
    let resp = cms_harness::api::router(app.clone()).oneshot(Request::get(path).body(axum::body::Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    let to = resp.headers().get(header::LOCATION).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
    let body = String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned();
    (status, to, body)
}

fn enc(s: &str) -> String {
    cms_harness::routes::encode(s)
}

fn write(app: &cms_harness::Shared, path: &str, content: &str) {
    ok(app, &builder(), "write_site_file", json!({ "path": path, "content": content, "note": "页面" }));
}

fn post(app: &cms_harness::Shared, title: &str, body: &str) -> Value {
    ok(app, &builder(), "create_record", json!({ "collection": "news", "note": "写", "data": { "title": title, "body": body } }))
}

fn setup(app: &cms_harness::Shared) {
    ok(app, &builder(), "create_collection", json!({ "name": "news", "title": "新闻", "read": "public", "note": "建",
        "fields": [{ "name": "title", "title": "标题", "type": "text" }, { "name": "body", "title": "正文", "type": "richtext" }] }));
    write(app, "news/_item.html", "<h1>{{ item.title }}</h1>{{ item.body }}<a class=self href=\"{{ item.url }}\">self</a>");
}

#[tokio::test]
async fn every_record_gets_a_page_from_one_template() {
    let app = app();
    setup(&app);
    let r = post(&app, "鼓手大赛 2026！", "<p>报名开始</p>");
    assert_eq!(r["slug"], "鼓手大赛-2026");
    let path = enc("/news/鼓手大赛-2026");
    assert_eq!(fetch(&app, &path).await.0, StatusCode::NOT_FOUND, "a draft is not live");
    // The preview shows it before publishing.
    let s = admin_session(&app, "builder").await;
    let base = preview_base(&app, &s.0).await;
    let (status, _, page) = fetch(&app, &format!("{base}{path}")).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("鼓手大赛 2026！"), "{page}");

    publish_all(&app);
    let (status, _, page) = fetch(&app, &path).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("<h1>鼓手大赛 2026！</h1>") && page.contains("<p>报名开始</p>"), "{page}");
    assert!(page.replace("&#x2f;", "/").contains("href=\"/news/鼓手大赛-2026\""), "item.url: {page}");
    // Same title, another record: its own address. The template itself is not a page.
    assert_eq!(post(&app, "鼓手大赛 2026！", "")["slug"], "鼓手大赛-2026-2");
    assert_eq!(fetch(&app, "/news/_item").await.0, StatusCode::NOT_FOUND);
    assert_eq!(fetch(&app, "/news/nothing-here").await.0, StatusCode::NOT_FOUND);
    // The sitemap lists the page, encoded.
    let xml = get_text(&app, "/sitemap.xml").await;
    assert!(xml.contains(&format!("http://127.0.0.1:4200{path}")), "{xml}");
}

#[tokio::test]
async fn no_template_no_pages_and_private_stays_private() {
    let app = app();
    ok(&app, &builder(), "create_collection", json!({ "name": "notes", "title": "笔记", "note": "建",
        "fields": [{ "name": "title", "title": "标题", "type": "text" }] }));
    ok(&app, &builder(), "create_record", json!({ "collection": "notes", "note": "写", "data": { "title": "内部" } }));
    publish_all(&app);
    assert_eq!(fetch(&app, &enc("/notes/内部")).await.0, StatusCode::NOT_FOUND, "no template yet");
    write(&app, "_item.html", "<h1>{{ item.title }}</h1>");
    publish_all(&app);
    assert_eq!(fetch(&app, &enc("/notes/内部")).await.0, StatusCode::NOT_FOUND, "private collection");
    ok(&app, &builder(), "update_collection", json!({ "name": "notes", "read": "public", "note": "公开" }));
    publish_all(&app);
    let (status, _, page) = fetch(&app, &enc("/notes/内部")).await;
    assert_eq!(status, StatusCode::OK, "the site-wide template serves any collection");
    assert!(page.contains("<h1>内部</h1>"));
}

#[tokio::test]
async fn a_changed_address_is_a_draft_and_the_old_one_redirects() {
    let app = app();
    setup(&app);
    let id = post(&app, "Old name", "")["id"].as_i64().unwrap();
    publish_all(&app);
    assert_eq!(fetch(&app, "/news/old-name").await.0, StatusCode::OK);
    // The title changes, the address stays.
    ok(&app, &builder(), "update_record", json!({ "id": id, "note": "改", "data": { "title": "New name" } }));
    publish_all(&app);
    assert_eq!(fetch(&app, "/news/old-name").await.0, StatusCode::OK);
    // The address changes as a draft: live keeps the old one until published.
    ok(&app, &builder(), "update_record", json!({ "id": id, "note": "改网址", "data": {}, "slug": "new-name" }));
    assert_eq!(fetch(&app, "/news/old-name").await.0, StatusCode::OK);
    assert_eq!(fetch(&app, "/news/new-name").await.0, StatusCode::NOT_FOUND);
    publish_all(&app);
    assert_eq!(fetch(&app, "/news/new-name").await.0, StatusCode::OK);
    let (status, to, _) = fetch(&app, "/news/old-name").await;
    assert_eq!((status, to.as_str()), (StatusCode::MOVED_PERMANENTLY, "/news/new-name"));
    // A taken or bad name is refused.
    let other = post(&app, "Other", "")["id"].as_i64().unwrap();
    assert!(err(&app, &builder(), "update_record", json!({ "id": other, "note": "x", "data": {}, "slug": "old-name" })).contains("已经被"));
    assert!(err(&app, &builder(), "update_record", json!({ "id": other, "note": "x", "data": {}, "slug": "a/b" })).contains("不合法"));
}

#[tokio::test]
async fn links_follow_the_record_they_point_at() {
    let app = app();
    setup(&app);
    let target = post(&app, "Target", "")["id"].as_i64().unwrap();
    publish_all(&app);
    // A link pasted as an address of this site is kept as the record, not the address.
    let id = post(&app, "Linker", "<p><a href=\"http://127.0.0.1:4200/news/target\">see</a> <a href=\"https://example.com/x\">out</a></p>")["id"].as_i64().unwrap();
    let stored = ok(&app, &reader(), "get_record", json!({ "id": id }));
    let body = stored["data"]["body"].as_str().unwrap().to_string();
    assert!(body.contains(&format!("href=\"/_r/{target}\"")) && body.contains("https://example.com/x"), "{body}");
    ok(&app, &builder(), "update_record", json!({ "id": target, "note": "改网址", "data": {}, "slug": "moved" }));
    publish_all(&app);
    let (_, _, page) = fetch(&app, "/news/linker").await;
    assert!(page.contains("href=\"/news/moved\""), "rendered at the record's current address: {page}");
    let (status, to, _) = fetch(&app, &format!("/_r/{target}")).await;
    assert_eq!((status, to.as_str()), (StatusCode::MOVED_PERMANENTLY, "/news/moved"));
}

#[tokio::test]
async fn collections_choose_their_address_pattern() {
    let app = app();
    ok(&app, &builder(), "create_collection", json!({ "name": "categories", "title": "分类", "read": "public", "note": "建",
        "fields": [{ "name": "name", "title": "名称", "type": "text" }] }));
    ok(&app, &builder(), "create_collection", json!({ "name": "products", "title": "产品", "read": "public", "note": "建",
        "url": "/products/{category.slug}/{slug}",
        "fields": [{ "name": "name", "title": "名称", "type": "text" }, { "name": "category", "title": "分类", "type": "relation", "target": "categories" }] }));
    let drums = ok(&app, &builder(), "create_record", json!({ "collection": "categories", "note": "x", "data": { "name": "Drums" } }))["id"].clone();
    let bells = ok(&app, &builder(), "create_record", json!({ "collection": "categories", "note": "x", "data": { "name": "Bells" } }))["id"].clone();
    let snare = ok(&app, &builder(), "create_record", json!({ "collection": "products", "note": "x", "data": { "name": "Snare", "category": drums } }))["id"].as_i64().unwrap();
    write(&app, "products/_item.html", "<h1>{{ item.name }}</h1>");
    publish_all(&app);
    assert_eq!(fetch(&app, "/products/drums/snare").await.0, StatusCode::OK);
    // Moved to another category: the old address redirects to the new one.
    ok(&app, &builder(), "update_record", json!({ "id": snare, "note": "x", "data": { "category": bells } }));
    publish_all(&app);
    let (status, to, _) = fetch(&app, "/products/drums/snare").await;
    assert_eq!((status, to.as_str()), (StatusCode::MOVED_PERMANENTLY, "/products/bells/snare"));
    // Patterns are checked.
    for bad in ["news/{slug}", "/p/{name}", "/admin/{slug}", "/p/{slug}/{id}", "/p/{nope}/{slug}"] {
        let e = err(&app, &builder(), "update_collection", json!({ "name": "products", "note": "x", "url": bad }));
        assert!(e.contains("网址规则"), "{bad}: {e}");
    }
    ok(&app, &builder(), "update_collection", json!({ "name": "products", "note": "x", "url": "/{slug}" }));
    publish_all(&app);
    assert_eq!(fetch(&app, "/snare").await.0, StatusCode::OK, "root-level pages");
}

#[tokio::test]
async fn publishing_never_leaves_a_dead_link() {
    let app = app();
    setup(&app);
    let target = post(&app, "Target", "")["id"].as_i64().unwrap();
    publish_all(&app);
    let linker = post(&app, "Linker", "<p><a href=\"/news/target\">see</a></p>")["id"].as_i64().unwrap();
    publish_all(&app);
    // Taking the linked record offline while something live still links to it is stopped.
    ok(&app, &builder(), "delete_record", json!({ "id": target, "note": "删" }));
    let e = err(&app, &builder(), "publish", json!({ "note": "删", "targets": [format!("record:{target}")] }));
    assert!(e.contains("死链") && e.contains(&format!("record:{linker}")), "{e}");
    // Publishing both together (the link removed) is fine.
    ok(&app, &builder(), "update_record", json!({ "id": linker, "note": "去掉链接", "data": { "body": "<p>no link</p>" } }));
    publish_all(&app);
    // A page linking to a record that is not live is stopped too.
    let draft = post(&app, "Draft only", "")["id"].as_i64().unwrap();
    write(&app, "about.html", &format!("<a href=\"/_r/{draft}\">x</a>"));
    let e = err(&app, &builder(), "publish", json!({ "note": "x", "targets": ["file:about.html"] }));
    assert!(e.contains("file:about.html 链接到"), "{e}");
}
