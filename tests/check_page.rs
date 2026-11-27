//! `check_page`: the builder (the chat, or an agent over MCP) looks at the draft page it just wrote, as the
//! preview would render it, and learns what is wrong with it without a person having to say so.

mod common;

use common::*;
use serde_json::{json, Value};

fn file(app: &cms_harness::Shared, path: &str, content: &str) {
    ok(app, &builder(), "write_site_file", json!({"path": path, "content": content, "note": "写页面"}));
}

fn check(app: &cms_harness::Shared, path: &str) -> Value {
    ok(app, &reader(), "check_page", json!({"path": path}))
}

fn list(v: &Value) -> Vec<&str> {
    v.as_array().unwrap_or_else(|| panic!("not a list: {v}")).iter().filter_map(Value::as_str).collect()
}

const GOOD: &str = "<!doctype html><html lang=\"zh\"><head><meta name=\"viewport\" content=\"width=device-width\"><title>青瓦陶器</title><link rel=\"stylesheet\" href=\"/style.css\"></head>\
<body><h1>慢慢做的器物</h1><h2>最近</h2><p><a href=\"/about\">关于</a> <a href=\"https://example.com/x\">外面</a> <a href=\"#top\">回顶部</a> <a href=\"mailto:a@b.c\">写信</a></p>\
<img src=\"/logo.svg\" alt=\"标志\"></body></html>";

#[test]
fn a_sound_draft_page_comes_back_clean_with_what_it_shows() {
    let app = app();
    file(&app, "index.html", GOOD);
    file(&app, "about.html", "<!doctype html><title>关于</title><h1>关于</h1>");
    file(&app, "style.css", "h1 { color: teal }");
    file(&app, "logo.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>");

    let r = check(&app, "/");
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!((r["status"].as_i64(), r["title"].as_str()), (Some(200), Some("青瓦陶器")));
    assert_eq!(list(&r["headings"]), vec!["h1 慢慢做的器物", "h2 最近"]);
    assert!(list(&r["broken_links"]).is_empty() && list(&r["missing_files"]).is_empty() && list(&r["warnings"]).is_empty(), "{r}");
    assert_eq!(r["images_without_alt"], 0);
}

#[test]
fn links_and_files_that_lead_nowhere_are_named() {
    let app = app();
    file(&app, "index.html", "<!doctype html><html lang=\"zh\"><head><meta name=\"viewport\" content=\"width=device-width\"><title>首页</title>\
<link rel=\"stylesheet\" href=\"/gone.css\"><script type=\"module\" src=\"/app.js\"></script></head>\
<body><h1>首页</h1><a href=\"/missing\">没有的页面</a><a href=\"/missing?x=1#y\">同一个</a><a href=\"/account/login\">登录</a><img src=\"/uploads/nope.png\" alt=\"\"></body></html>");

    let r = check(&app, "/");
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(list(&r["broken_links"]), vec!["/missing", "/account/login"], "each address once; the login page is off");
    assert_eq!(list(&r["missing_files"]), vec!["/gone.css", "/app.js", "/uploads/nope.png"]);
}

#[test]
fn a_page_that_fails_to_render_says_why() {
    let app = app();
    file(&app, "index.html", "<!doctype html><title>首页</title><h1>{{ no_such_function() }}</h1>");
    let r = check(&app, "/");
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["status"], 500);
    assert!(r["error"].as_str().unwrap().contains("no_such_function"), "{r}");
}

#[test]
fn a_page_that_is_not_there_is_a_404_not_a_pass() {
    let app = app();
    file(&app, "index.html", GOOD);
    let r = check(&app, "/nowhere");
    assert_eq!((r["ok"].as_bool(), r["status"].as_i64()), (Some(false), Some(404)), "{r}");
    let e = err(&app, &reader(), "check_page", json!({"path": "nowhere"}));
    assert!(e.contains('/'), "a path starts with a slash: {e}");
}

#[test]
fn what_a_careful_builder_would_fix_comes_as_warnings() {
    let app = app();
    file(&app, "index.html", "<html><body><h1>一</h1><h1>二</h1><img src=\"/a.png\"><img src=\"/b.png\" alt=\"\"></body></html>");
    file(&app, "a.png", "iVBORw0KGgo=");
    file(&app, "b.png", "iVBORw0KGgo=");
    let r = check(&app, "/");
    let warnings = list(&r["warnings"]).join("\n");
    for needs in ["<title>", "viewport", "lang", "h1"] {
        assert!(warnings.contains(needs), "{needs} missing from: {warnings}");
    }
    assert_eq!(r["images_without_alt"], 1, "alt=\"\" is a decision; no alt at all is an omission");
    assert_eq!(r["ok"], true, "warnings alone do not fail the page: {r}");
}

#[test]
fn it_reads_the_draft_not_the_live_site() {
    let app = app();
    file(&app, "index.html", "<!doctype html><title>第一版</title><h1>第一版</h1>");
    publish_all(&app);
    file(&app, "index.html", "<!doctype html><title>第二版</title><h1>第二版</h1>");
    assert_eq!(check(&app, "/")["title"], "第二版");
}
