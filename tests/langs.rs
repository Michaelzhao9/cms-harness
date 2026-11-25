mod common;
use axum::http::{header, Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn fetch(app: &cms_harness::Shared, path: &str, host: Option<&str>) -> (StatusCode, String) {
    let (p, q) = path.split_once('?').map_or((path, String::new()), |(p, q)| (p, format!("?{q}")));
    let mut req = Request::get(format!("{}{q}", cms_harness::routes::encode(p)).as_str());
    if let Some(h) = host {
        req = req.header(header::HOST, h);
    }
    let resp = cms_harness::api::router(app.clone()).oneshot(req.body(axum::body::Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    (status, String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned().replace("&#x2f;", "/"))
}

fn write(app: &cms_harness::Shared, path: &str, content: &str) {
    ok(app, &builder(), "write_site_file", json!({ "path": path, "content": content, "note": "页面" }));
}

fn languages(app: &cms_harness::Shared) {
    ok(app, &builder(), "set_site_settings", json!({ "note": "多语言", "languages": { "list": [
        { "code": "zh", "name": "中文" },
        { "code": "en", "name": "English" },
        { "code": "ja", "name": "日本語", "domain": "example.jp", "fallback": ["en"] },
    ] }, "strings": { "en": { "最新消息": "Latest news" } } }));
}

fn news(app: &cms_harness::Shared, untranslated: &str) {
    ok(app, &builder(), "create_collection", json!({ "name": "news", "title": "新闻", "read": "public", "untranslated": untranslated, "note": "建",
        "fields": [{ "name": "title", "title": "标题", "type": "text" }, { "name": "body", "title": "正文", "type": "richtext" },
                   { "name": "date", "title": "日期", "type": "date" }, { "name": "code", "title": "编号", "type": "text", "translate": false }] }));
    write(app, "news/_item.html", "<h1>{{ item.title }}</h1>{{ item.body }}<time>{{ item.date|date }}</time><b>{{ item.code }}</b>\
        <nav>{% for l in languages %}<a class=\"{{ 'on' if l.current }}\" href=\"{{ l.url }}\">{{ l.name }}</a>{% endfor %}</nav><html-lang>{{ lang }}|{{ dir }}</html-lang>");
    write(app, "index.html", "<h2>{{ \"最新消息\"|t }}</h2>{% for n in collection('news') %}<a href=\"{{ n.url }}\">{{ n.title }}</a>{% endfor %}");
}

fn add(app: &cms_harness::Shared) -> i64 {
    ok(app, &builder(), "create_record", json!({ "collection": "news", "note": "写", "data": { "title": "鼓手大赛", "body": "<p>欢迎报名</p>", "date": "2026-11-24", "code": "N-1" } }))["id"].as_i64().unwrap()
}

#[tokio::test]
async fn one_record_many_languages() {
    let app = app();
    languages(&app);
    news(&app, "fallback");
    let id = add(&app);
    let r = ok(&app, &builder(), "set_translation", json!({ "id": id, "lang": "en", "note": "翻译", "data": { "title": "Drum Contest", "body": "<p>Sign up now</p>" } }));
    assert_eq!(r["slug"], "drum-contest");
    publish_all(&app);

    let (s, zh) = fetch(&app, "/news/鼓手大赛", None).await;
    assert_eq!(s, StatusCode::OK, "{zh}");
    assert!(zh.contains("<h1>鼓手大赛</h1>") && zh.contains("2026年11月24日") && zh.contains("<b>N-1</b>") && zh.contains("zh|ltr"), "{zh}");
    // Search engines see the same page in every language.
    assert!(zh.contains("hreflang=\"en\" href=\"http://127.0.0.1:4200/en/news/drum-contest\"") && zh.contains("hreflang=\"x-default\""), "{zh}");

    let (s, en) = fetch(&app, "/en/news/drum-contest", None).await;
    assert_eq!(s, StatusCode::OK, "{en}");
    assert!(en.contains("<h1>Drum Contest</h1>") && en.contains("<p>Sign up now</p>") && en.contains("Nov 24, 2026"), "{en}");
    assert!(en.contains("<b>N-1</b>"), "shared fields stay shared");
    assert!(en.contains("class=\"on\" href=\"http://127.0.0.1:4200/en/news/drum-contest\">English"), "{en}");
    // The Chinese address in English redirects to the English one.
    let (s, _) = fetch(&app, "/en/news/鼓手大赛", None).await;
    assert_eq!(s, StatusCode::MOVED_PERMANENTLY);

    // Japanese has no translation: its fallback (English) shows, at the Chinese slug, on its own domain.
    let (s, ja) = fetch(&app, "/news/鼓手大赛", Some("example.jp")).await;
    assert_eq!(s, StatusCode::OK, "{ja}");
    assert!(ja.contains("<h1>Drum Contest</h1>") && ja.contains("ja|ltr") && ja.contains("2026年11月24日"), "{ja}");
    assert!(zh.contains("href=\"http://example.jp/news/"), "the Japanese alternate is on its domain: {zh}");

    // Lists and template text follow the language too.
    let (_, home) = fetch(&app, "/en", None).await;
    assert!(home.contains("<h2>Latest news</h2>") && home.contains("href=\"/en/news/drum-contest\">Drum Contest"), "{home}");
    let (_, home) = fetch(&app, "/", None).await;
    assert!(home.contains("<h2>最新消息</h2>") && home.contains("鼓手大赛"), "{home}");
}

#[tokio::test]
async fn collections_can_leave_untranslated_records_out() {
    let app = app();
    languages(&app);
    news(&app, "hide");
    let id = add(&app);
    publish_all(&app);
    assert_eq!(fetch(&app, "/en/news/鼓手大赛", None).await.0, StatusCode::NOT_FOUND);
    let (_, home) = fetch(&app, "/en", None).await;
    assert!(!home.contains("鼓手大赛"), "{home}");
    ok(&app, &builder(), "set_translation", json!({ "id": id, "lang": "en", "note": "翻译", "data": { "title": "Drum Contest" } }));
    publish_all(&app);
    assert_eq!(fetch(&app, "/en/news/drum-contest", None).await.0, StatusCode::OK);
    // A record can also be left out of one language on purpose.
    ok(&app, &builder(), "set_translation", json!({ "id": id, "lang": "en", "note": "不在英文站", "hide": true }));
    publish_all(&app);
    assert_eq!(fetch(&app, "/en/news/drum-contest", None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(fetch(&app, "/news/鼓手大赛", None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn translations_know_when_their_source_changed() {
    let app = app();
    languages(&app);
    news(&app, "fallback");
    let id = add(&app);
    let status = |app: &cms_harness::Shared| ok(app, &reader(), "translation_status", json!({ "collection": "news", "lang": "en" }))[0].clone();
    assert_eq!(status(&app)["missing_ids"], json!([id]));
    ok(&app, &builder(), "set_translation", json!({ "id": id, "lang": "en", "note": "翻译", "data": { "title": "Drum Contest", "body": "<p>Sign up</p>" } }));
    let s = status(&app);
    assert_eq!((s["done"].clone(), s["missing"].clone(), s["stale"].clone()), (json!(1), json!(0), json!(0)));
    // The Chinese title changes: the English one is now out of date, the body is not.
    ok(&app, &builder(), "update_record", json!({ "id": id, "note": "改标题", "data": { "title": "鼓手大赛（决赛）" } }));
    assert_eq!(status(&app)["stale_ids"], json!([id]));
    let r = ok(&app, &builder(), "set_translation", json!({ "id": id, "lang": "en", "note": "更新翻译", "data": { "title": "Drum Contest Final" } }));
    assert_eq!(r["status"], json!({ "title": "done", "body": "done" }));
    // Translations ride along with the record: kept through edits and versions.
    let rec = ok(&app, &reader(), "get_record", json!({ "id": id }));
    assert_eq!(rec["data"]["title"], "鼓手大赛（决赛）");
}

#[test]
fn translation_rules() {
    let app = app();
    assert!(err(&app, &builder(), "set_translation", json!({ "id": 1, "lang": "en", "note": "x", "data": {} })).contains("还没有设置多种语言"));
    languages(&app);
    news(&app, "fallback");
    let id = add(&app);
    let e = |args: Value| err(&app, &builder(), "set_translation", args);
    assert!(e(json!({ "id": id, "lang": "zh", "note": "x", "data": {} })).contains("不是要翻译的语言"));
    assert!(e(json!({ "id": id, "lang": "fr", "note": "x", "data": {} })).contains("不是要翻译的语言"));
    assert!(e(json!({ "id": id, "lang": "en", "note": "x", "data": { "code": "X" } })).contains("不翻译"));
    assert!(e(json!({ "id": id, "lang": "en", "note": "x", "data": { "date": "2026-01-01" } })).contains("不翻译"));
    for bad in [json!({ "list": [] }), json!({ "list": [{ "code": "EN", "name": "x" }] }), json!({ "list": [{ "code": "zh", "name": "中文", "fallback": ["xx"] }] })] {
        assert!(!err(&app, &builder(), "set_site_settings", json!({ "note": "x", "languages": bad })).is_empty());
    }
}

#[tokio::test]
async fn search_sitemap_and_export_speak_every_language() {
    let app = app();
    languages(&app);
    news(&app, "fallback");
    let id = add(&app);
    ok(&app, &builder(), "set_translation", json!({ "id": id, "lang": "en", "note": "翻译", "data": { "title": "Drum Contest", "body": "<p>Sign up now</p>" } }));
    write(&app, "search.html", "{% for r in search(request.query.q or '') %}<li>{{ r.title }}|{{ r.url }}</li>{% endfor %}");
    publish_all(&app);
    let (_, en) = fetch(&app, "/en/search?q=drum", None).await;
    assert!(en.contains("<li>Drum Contest|/en/news/drum-contest</li>"), "{en}");
    let (_, zh) = fetch(&app, "/search?q=%E9%BC%93%E6%89%8B", None).await; // 鼓手
    assert!(zh.contains("<li>鼓手大赛|"), "{zh}");

    let xml = get_text(&app, "/sitemap.xml").await;
    assert!(xml.contains("<loc>http://127.0.0.1:4200/en/news/drum-contest</loc>"), "{xml}");
    assert!(xml.contains("hreflang=\"ja\" href=\"http://example.jp/news/"), "{xml}");

    let dir = std::env::temp_dir().join(format!("hc-export-langs-{:016x}", rand::random::<u64>()));
    cms_harness::export::export(&app, &dir).await.unwrap();
    assert!(std::fs::read_to_string(dir.join("en/news/drum-contest.html")).unwrap().contains("<h1>Drum Contest</h1>"));
    assert!(std::fs::read_to_string(dir.join("news/鼓手大赛.html")).unwrap().contains("<h1>鼓手大赛</h1>"));
    assert!(std::fs::read_to_string(dir.join("example.jp/news/鼓手大赛.html")).unwrap().contains("ja|ltr"), "a domain language is its own folder");
}

#[tokio::test]
async fn the_admin_edits_languages_and_translations() {
    let app = app();
    let s = admin_session(&app, "builder").await;
    let (code, _, body) = post_form(&app, "/admin/settings/languages", Some(&s), &[("code", "zh"), ("name", "中文"), ("domain", ""), ("fallback", ""),
        ("code", "en"), ("name", "English"), ("domain", ""), ("fallback", ""), ("code", ""), ("name", ""), ("domain", ""), ("fallback", ""), ("sync", "1")]).await;
    assert_eq!(code, StatusCode::SEE_OTHER, "{body}");
    let langs = cms_harness::langs::load(&app.conn(), true).unwrap();
    assert_eq!((langs.list.len(), langs.sync), (2, true));
    news(&app, "fallback");
    let id = add(&app);
    let (_, _, page) = get_with(&app, &format!("/admin/r/{id}"), Some(&s.0)).await;
    assert!(page.contains("id=\"tr-en\"") && page.contains("缺翻译") && page.contains("name=\"tr.title\""), "{page}");
    let (code, _, body) = post_form(&app, &format!("/admin/r/{id}/translation/en"), Some(&s), &[("tr.title", "Drum Contest"), ("tr.body", "<p>Hi</p>"), ("slug", ""), ("note", "翻译")]).await;
    assert_eq!(code, StatusCode::SEE_OTHER, "{body}");
    let (_, _, page) = get_with(&app, &format!("/admin/r/{id}"), Some(&s.0)).await;
    assert!(page.contains("已翻译") && page.contains("value=\"Drum Contest\""), "{page}");
    // The chat is told about the languages and the sync promise.
    let tools: Vec<String> = cms_harness::chat::chat_tools(&builder()).into_iter().map(|t| t.name).collect();
    assert!(tools.iter().any(|t| t == "set_translation") && tools.iter().any(|t| t == "translation_status"));
}
