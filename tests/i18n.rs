mod common;
use axum::http::{header, Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get_lang(app: &cms_harness::Shared, path: &str, cookie: Option<&str>, accept: &str) -> (StatusCode, String) {
    let mut req = Request::get(path).header(header::ACCEPT_LANGUAGE, accept);
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    let resp = cms_harness::api::router(app.clone()).oneshot(req.body(axum::body::Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    (status, String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned())
}

/// The visible Chinese left on a page: text and attribute values, minus the language switch's own names.
fn chinese_left(page: &str) -> Vec<String> {
    // The site name (测试站) is the owner's text, not the admin's.
    let page = page.replace("语言 / Language", "").replace(">中文<", "><").replace("测试站", "");
    let mut out = vec![];
    for piece in page.split(['<', '>', '"']) {
        if piece.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) && !out.contains(&piece.to_string()) {
            out.push(piece.to_string());
        }
    }
    out
}

async fn check_english(app: &cms_harness::Shared, s: &(String, String), pages: &[&str]) {
    for path in pages {
        let (status, _, page) = get_with(app, path, Some(&s.0)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(page.contains("<html lang=\"en\">"), "{path}");
        assert_eq!(chinese_left(&page), Vec::<String>::new(), "{path} still shows Chinese");
    }
}

#[tokio::test]
async fn the_admin_switches_to_english_and_back() {
    let app = app();
    let s = admin_session(&app, "builder").await;
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&s.0)).await;
    assert!(page.contains("<html lang=\"zh-CN\">") && page.contains("外观"), "Chinese by default");

    let (status, _, _) = post_form(&app, "/admin/settings/appearance", Some(&s), &[("lang", "en")]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    // Owner content in English, so any Chinese left on these pages is the admin's own.
    ok(&app, &builder(), "create_collection", serde_json::json!({ "name": "posts", "title": "Posts", "note": "posts", "fields": [
        { "name": "title", "title": "Title", "type": "text", "required": true },
        { "name": "kind", "title": "Kind", "type": "select", "options": ["a", "b"], "index": true }
    ] }));
    let id = ok(&app, &builder(), "create_record", serde_json::json!({ "collection": "posts", "note": "first", "data": { "title": "Hello" } }))["id"].as_i64().unwrap();
    ok(&app, &builder(), "set_view", serde_json::json!({ "name": "board", "title": "Board", "collection": "posts", "note": "n",
        "columns": ["title", "kind"], "filters": ["kind"], "sort": "-id", "actions": ["create", "edit"] }));
    ok(&app, &builder(), "set_member_login", serde_json::json!({ "enabled": true, "note": "login" }));
    let record = format!("/admin/r/{id}");
    let pages = ["/admin", "/admin/settings", "/admin/publish", "/admin/media", "/admin/c/posts", record.as_str(), "/admin/v/board", "/admin/releases"];
    check_english(&app, &s, &pages).await;
    // After publishing: release history, published records, the members screen.
    publish_all(&app);
    check_english(&app, &s, &["/admin", "/admin/publish", "/admin/releases", "/admin/c/posts", record.as_str(), "/admin/members"]).await;
    // Plain-text errors follow the setting too.
    let (status, _, body) = post_form(&app, "/admin/settings/appearance", Some(&s), &[("lang", "fr")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.starts_with("Choose a listed marker colour"), "{body}");

    assert_eq!(post_form(&app, "/admin/settings/appearance", Some(&s), &[("lang", "zh")]).await.0, StatusCode::SEE_OTHER);
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&s.0)).await;
    assert!(page.contains("<html lang=\"zh-CN\">") && page.contains("外观"));
}

#[tokio::test]
async fn the_first_login_keeps_the_browsers_language() {
    let app = app();
    cms_harness::auth::create_admin(&app.conn(), "en@test", "editor", Some("secret-pass")).unwrap();
    let fields = [("email", "en@test"), ("password", "secret-pass")];
    let (status, _, _) = post_form_with(&app, "/admin/login", None, &fields, &[("accept-language", "en-GB,en;q=0.9")]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let lang: String = app.conn().query_row("SELECT json_extract(prefs, '$.lang') FROM admins WHERE email = 'en@test'", [], |r| r.get(0)).unwrap();
    assert_eq!(lang, "en");
    // A later login from a Chinese browser keeps the saved choice.
    post_form_with(&app, "/admin/login", None, &fields, &[("accept-language", "zh-CN")]).await;
    let lang: String = app.conn().query_row("SELECT json_extract(prefs, '$.lang') FROM admins WHERE email = 'en@test'", [], |r| r.get(0)).unwrap();
    assert_eq!(lang, "en");
    // The login page itself follows the browser.
    let (_, page) = get_lang(&app, "/admin/login", None, "en-US").await;
    assert!(page.contains("<html lang=\"en\">") && chinese_left(&page).is_empty(), "{:?}", chinese_left(&page));
}

#[tokio::test]
async fn built_in_site_pages_follow_the_visitor() {
    let app = app();
    let (status, page) = get_lang(&app, "/no-such-page", None, "en-US,en").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(page.contains("<html lang=\"en\">"), "{page}");
    assert!(page.contains("Page not found") || chinese_left(&page).is_empty(), "{page}");
    let (_, page) = get_lang(&app, "/no-such-page", None, "zh-CN").await;
    assert!(page.contains("页面不存在"), "{page}");
    // The switch wins over the browser and is remembered in a cookie.
    let req = Request::get("/?lang=en").header(header::ACCEPT_LANGUAGE, "zh-CN").body(axum::body::Body::empty()).unwrap();
    let resp = cms_harness::api::router(app.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.headers()[header::SET_COOKIE].to_str().unwrap(), "hc_lang=en; Path=/; Max-Age=31536000; SameSite=Lax");
    let page = String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned();
    assert!(page.contains("<html lang=\"en\">") && chinese_left(&page).is_empty(), "{:?}", chinese_left(&page));
    let (_, page) = get_lang(&app, "/", Some("hc_lang=en"), "zh-CN").await;
    assert!(page.contains("<html lang=\"en\">"), "{page}");
    // The admin's login page has the same switch.
    let (_, page) = get_lang(&app, "/admin/login?lang=en", None, "zh-CN").await;
    assert!(page.contains("<html lang=\"en\">") && chinese_left(&page).is_empty(), "{:?}", chinese_left(&page));
}

#[tokio::test]
async fn a_sites_own_text_files_reach_english_browsers_untouched() {
    let app = app();
    let big = "中文内容\n".repeat(20_000);
    ok(&app, &builder(), "write_site_file", serde_json::json!({ "path": "notes.txt", "content": big, "note": "x" }));
    publish_all(&app);
    let (status, body) = get_lang(&app, "/notes.txt", None, "en-US").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, big);
}
