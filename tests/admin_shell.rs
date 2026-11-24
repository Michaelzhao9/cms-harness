mod common;
use axum::http::{header, HeaderMap, StatusCode};
use common::*;
use cms_harness::web::{admin, session};
use serde_json::{json, Value};

fn user(app: &cms_harness::Shared, cookie: &str) -> session::AdminUser {
    let mut h = HeaderMap::new();
    h.insert(header::COOKIE, cookie.parse().unwrap());
    session::from_cookie(&app.conn(), &h).unwrap()
}

fn nav_of(app: &cms_harness::Shared, cookie: &str) -> Value {
    let u = user(app, cookie);
    admin::nav(&app.conn(), &u, "web")
}

fn products(app: &cms_harness::Shared) {
    ok(app, &builder(), "create_collection", json!({ "name": "products", "title": "产品", "note": "n",
        "fields": [{ "name": "name", "title": "名称", "type": "text" }] }));
}

fn record(app: &cms_harness::Shared, collection: &str, title: &str) -> Value {
    ok(app, &editor(), "create_record", json!({ "collection": collection, "note": "n", "data": { "title": title } }))["id"].clone()
}

fn location(h: &HeaderMap) -> &str {
    h[header::LOCATION].to_str().unwrap()
}

/// Every admin page a test renders goes through here: only the external admin.js, and no inline styles the CSP would drop.
fn assert_clean(page: &str) {
    assert_only_admin_js(page);
}

#[tokio::test]
async fn nav_lists_collections_and_their_views() {
    let app = app();
    news(&app);
    products(&app);
    ok(&app, &builder(), "create_collection", json!({ "name": "old", "title": "旧集合", "note": "n",
        "fields": [{ "name": "title", "title": "标题", "type": "text" }] }));
    publish_all(&app);
    record(&app, "news", "一");
    record(&app, "news", "二");
    publish_all(&app);
    record(&app, "news", "草稿");
    ok(&app, &builder(), "delete_collection", json!({ "name": "old", "note": "n" }));
    publish_all(&app);
    ok(&app, &builder(), "set_view", json!({ "name": "news_by_date", "title": "新闻（按日期）", "collection": "news", "note": "n",
        "columns": ["title", "date"], "sort": "-date" }));
    // A view whose collection is not listed is left out rather than shown loose.
    app.conn().execute("INSERT INTO views(name, draft) VALUES('stray', '{\"name\":\"stray\",\"title\":\"游离\",\"collection\":\"members\",\"columns\":[\"id\"]}')", []).unwrap();

    let s = admin_session(&app, "builder").await;
    let nav = nav_of(&app, &s.0);
    let names: Vec<&str> = nav["collections"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["news", "products"], "{nav}");
    let news_item = &nav["collections"][0];
    assert_eq!(news_item["title"], "新闻");
    assert_eq!(news_item["count"], 3, "drafts count, as in the content table");
    assert_eq!(news_item["views"], json!([{ "name": "news_by_date", "title": "新闻（按日期）" }]));
    assert_eq!(nav["collections"][1]["count"], 0);
    assert_eq!(nav["collections"][1]["views"], json!([]));
    assert_eq!(nav["members"], false);

    let (status, _, body) = get_with(&app, "/admin", Some(&s.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&body);
    for needle in [
        "href=\"/admin\"", "网页", "内容与表单", "href=\"/admin/c/news\"", "新闻", "href=\"/admin/c/products\"", "产品",
        "href=\"/admin/v/news_by_date\"", "新闻（按日期）", "后台", "href=\"/admin/media\"", "素材库",
        "href=\"/admin/releases\"", "发布记录", "href=\"/admin/settings\"", "设置", "aria-current=\"page\"",
    ] {
        assert!(body.contains(needle), "missing {needle}: {body}");
    }
    assert!(!body.contains("/admin/c/old") && !body.contains("stray"), "{body}");
    assert!(!body.contains("用户管理"), "member login is off");
    // Nav order: web, collections, back office, settings last.
    let at = |s: &str| body.find(s).unwrap();
    assert!(at("href=\"/admin/c/news\"") < at("href=\"/admin/v/news_by_date\""));
    assert!(at("href=\"/admin/v/news_by_date\"") < at("href=\"/admin/c/products\""));
    assert!(at("href=\"/admin/c/products\"") < at("href=\"/admin/media\""));
    assert!(at("href=\"/admin/media\"") < at("href=\"/admin/releases\""));
    assert!(at("href=\"/admin/releases\"") < at("href=\"/admin/settings\""));
    // The home page frames the site preview.
    assert!(frame_src(&body, "draft").starts_with("/_hc/preview/"), "{body}");

    // Editors see the same nav.
    let e = admin_session(&app, "editor").await;
    let (_, _, body) = get_with(&app, "/admin", Some(&e.0)).await;
    assert_clean(&body);
    assert!(body.contains("href=\"/admin/v/news_by_date\"") && body.contains("href=\"/admin/settings\""), "{body}");
}

#[tokio::test]
async fn nav_marks_collections_with_pending_changes() {
    let app = app();
    news(&app);
    products(&app);
    let s = admin_session(&app, "builder").await;
    let pending = |nav: &Value| -> Vec<bool> { nav["collections"].as_array().unwrap().iter().map(|c| c["pending"].as_bool().unwrap()).collect() };
    assert_eq!(pending(&nav_of(&app, &s.0)), [true, true], "draft definitions are pending");
    publish_all(&app);
    assert_eq!(pending(&nav_of(&app, &s.0)), [false, false]);
    let (_, _, body) = get_with(&app, "/admin", Some(&s.0)).await;
    assert_eq!(body.matches("class=\"mk\"").count(), 0, "{body}");

    // A draft record marks its collection only.
    let id = record(&app, "news", "一");
    assert_eq!(pending(&nav_of(&app, &s.0)), [true, false]);
    let (_, _, body) = get_with(&app, "/admin", Some(&s.0)).await;
    assert_clean(&body);
    assert_eq!(body.matches("class=\"mk\"").count(), 1, "{body}");
    publish_all(&app);
    assert_eq!(pending(&nav_of(&app, &s.0)), [false, false]);

    // A pending deletion counts too.
    ok(&app, &editor(), "delete_record", json!({ "id": id, "note": "n" }));
    assert_eq!(pending(&nav_of(&app, &s.0)), [true, false]);
    publish_all(&app);
    assert_eq!(pending(&nav_of(&app, &s.0)), [false, false]);

    // So does a changed definition.
    ok(&app, &builder(), "update_collection", json!({ "name": "products", "title": "货品", "note": "n" }));
    let nav = nav_of(&app, &s.0);
    assert_eq!(pending(&nav), [false, true]);
    assert_eq!(nav["collections"][1]["title"], "货品", "the draft title is shown");
}

#[tokio::test]
async fn collapsing_the_nav_is_remembered_for_the_session() {
    let app = app();
    let b = admin_session(&app, "builder").await;
    let e = admin_session(&app, "editor").await;
    let shell = |body: &str| if body.contains("class=\"shell chat-docked collapsed") { "collapsed" } else { "open" };
    let referer = |path: &str| format!("http://127.0.0.1:4200{path}");

    assert_eq!(shell(&get_with(&app, "/admin", Some(&b.0)).await.2), "open");
    let (status, h, _) = post_form_with(&app, "/admin/prefs", Some(&b), &[("nav", "collapsed")], &[("referer", &referer("/admin/c/news?page=2"))]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&h), "/admin/c/news?page=2");
    let (_, _, body) = get_with(&app, "/admin", Some(&b.0)).await;
    assert_clean(&body);
    assert_eq!(shell(&body), "collapsed");
    assert!(body.contains("aria-label=\"展开导航\""), "{body}");
    assert_eq!(user(&app, &b.0).prefs["nav"], "collapsed");
    // Only this session.
    assert_eq!(shell(&get_with(&app, "/admin", Some(&e.0)).await.2), "open");

    // A wrong token, a missing one or an unknown value changes nothing.
    let forged = (b.0.clone(), e.1.clone());
    assert_eq!(post_form(&app, "/admin/prefs", Some(&forged), &[("nav", "open")]).await.0, StatusCode::FORBIDDEN);
    assert_eq!(post_form(&app, "/admin/prefs", None, &[("nav", "open")]).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(post_form(&app, "/admin/prefs", Some(&b), &[("nav", "wide")]).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(user(&app, &b.0).prefs["nav"], "collapsed");

    // Only same-origin /admin referers are followed.
    for bad in [
        "https://evil.example/admin".to_string(),
        "http://127.0.0.1:4200.evil.example/admin".to_string(),
        referer("/"),
        referer("/administrator"),
        referer("//evil.example/admin"),
        "not a url".to_string(),
    ] {
        let (status, h, _) = post_form_with(&app, "/admin/prefs", Some(&b), &[("nav", "open")], &[("referer", &bad)]).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{bad}");
        assert_eq!(location(&h), "/admin", "{bad}");
    }
    let (_, h, _) = post_form(&app, "/admin/prefs", Some(&b), &[("nav", "open")]).await;
    assert_eq!(location(&h), "/admin", "no referer");
    let (_, _, body) = get_with(&app, "/admin", Some(&b.0)).await;
    assert_eq!(shell(&body), "open");
    assert!(body.contains("aria-label=\"收起导航\""), "{body}");
}

#[tokio::test]
async fn the_shell_has_no_script_tags() {
    let app = app();
    news(&app);
    publish_all(&app);
    record(&app, "news", "<script>alert(1)</script>");
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "title": "<script>x</script>", "note": "n" }));
    assert_clean(&get_with(&app, "/admin/login", None).await.2);
    for role in ["builder", "editor"] {
        let s = admin_session(&app, role).await;
        for nav in ["open", "collapsed"] {
            post_form(&app, "/admin/prefs", Some(&s), &[("nav", nav)]).await;
            let (status, _, body) = get_with(&app, "/admin", Some(&s.0)).await;
            assert_eq!(status, StatusCode::OK);
            assert_clean(&body);
            assert!(body.contains("&lt;script&gt;x&lt;"), "titles are escaped: {body}");
        }
    }
}

#[tokio::test]
async fn stylesheet_is_served_with_cache_headers() {
    let app = app();
    let (status, h, css) = get_with(&app, "/admin/static/admin.css", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(h[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/css"));
    let cache = h[header::CACHE_CONTROL].to_str().unwrap();
    assert!(cache.contains("max-age=31536000") && cache.contains("immutable"), "{cache}");
    assert!(css.contains(".shell"));
    // Pages link a versioned URL, so a new build is fetched despite the long cache.
    let s = admin_session(&app, "builder").await;
    for page in [get_with(&app, "/admin/login", None).await.2, get_with(&app, "/admin", Some(&s.0)).await.2] {
        let start = page.find("/admin/static/admin.css?v=").unwrap_or_else(|| panic!("{page}"));
        let v: String = page[start + "/admin/static/admin.css?v=".len()..].chars().take_while(char::is_ascii_hexdigit).collect();
        assert_eq!(v.len(), 12, "{page}");
    }
}
