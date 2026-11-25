//! The preview canvas: hand edits are drafts, reach the preview at once and the live site on publish.
mod common;
use axum::http::{header, Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// A multipart POST with the admin session: fields, then one image file `file`.
async fn post_image(app: &cms_harness::Shared, s: &(String, String), fields: &[(&str, &str)], file: &[u8]) -> (StatusCode, String) {
    let b = "XBOUNDARYX";
    let mut body = vec![];
    for (name, value) in fields.iter().chain([("csrf", s.1.as_str())].iter()) {
        body.extend(format!("--{b}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
    }
    body.extend(format!("--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.png\"\r\nContent-Type: image/png\r\n\r\n").as_bytes());
    body.extend(file);
    body.extend(format!("\r\n--{b}--\r\n").as_bytes());
    let req = Request::post("/admin/canvas/image").header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={b}")).header(header::COOKIE, s.0.as_str());
    let resp = cms_harness::api::router(app.clone()).oneshot(req.body(axum::body::Body::from(body)).unwrap()).await.unwrap();
    let status = resp.status();
    (status, String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned())
}

fn png() -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbImage::new(4, 3).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[tokio::test]
async fn images_are_added_and_replaced_with_uploads() {
    let (app, s, _) = setup().await;
    let (status, body) = post_image(&app, &s, &[("src", "index.html#5"), ("mode", "add")], &png()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let added: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(added["src"], "index.html#7");
    let draft = ok(&app, &reader(), "get_site_file", json!({ "path": "index.html" }))["content"].as_str().unwrap().to_string();
    let url = draft.split("<img src=\"").nth(1).unwrap().split('"').next().unwrap().to_string();
    assert!(url.starts_with("/uploads/") && url.ends_with(".png"), "{draft}");
    let (status, body) = post_image(&app, &s, &[("src", "index.html#7"), ("mode", "replace")], &png()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = post_image(&app, &s, &[("src", "index.html#5"), ("mode", "replace")], &png()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = post_image(&app, &s, &[("src", "index.html#5"), ("mode", "add")], b"not an image").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

const PAGE: &str = "<!doctype html><html><head><title>首页</title></head><body><h1>春季新品</h1><p>{{ site.name }}</p></body></html>";

async fn setup() -> (cms_harness::Shared, (String, String), String) {
    let app = app();
    ok(&app, &builder(), "write_site_file", json!({ "path": "index.html", "content": PAGE, "note": "首页" }));
    publish_all(&app);
    let s = admin_session(&app, "builder").await;
    let base = preview_base(&app, &s.0).await;
    (app, s, base)
}

#[tokio::test]
async fn a_style_edit_is_a_draft_until_published() {
    let (app, s, base) = setup().await;
    let (_, _, preview) = get_with(&app, &format!("{base}/"), Some(&s.0)).await;
    assert!(preview.contains("<h1 data-hc-src=\"index.html#5\">"), "{preview}");
    assert!(!preview.contains("<body data-hc-src"));

    let (status, _, body) = post_form(&app, "/admin/canvas/style", Some(&s), &[("src", "index.html#5"), ("prop", "color"), ("value", "#FF0000")]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["pending"], 1);

    let id = cms_harness::canvas::id("index.html#5");
    let (_, _, preview) = get_with(&app, &format!("{base}/"), Some(&s.0)).await;
    assert!(preview.contains(&format!("<h1 data-hc-src=\"index.html#5\" data-hc=\"{id}\">")), "{preview}");
    assert!(preview.contains(&format!("<style data-hc-canvas>[data-hc=\"{id}\"]{{color:#ff0000 !important}}</style></head>")));

    let live = get_text(&app, "/").await;
    assert!(!live.contains("data-hc"), "{live}");

    publish_all(&app);
    let live = get_text(&app, "/").await;
    assert!(live.contains(&format!("<h1 data-hc=\"{id}\">")), "{live}");
    assert!(live.contains("color:#ff0000"));
    assert!(!live.contains("data-hc-src"));

    // Clearing it is a change too; once published the page is as it was.
    let (status, _, _) = post_form(&app, "/admin/canvas/style", Some(&s), &[("src", "index.html#5"), ("prop", "color"), ("value", "")]).await;
    assert_eq!(status, StatusCode::OK);
    publish_all(&app);
    let live = get_text(&app, "/").await;
    assert!(!live.contains("data-hc"), "{live}");
}

#[tokio::test]
async fn edits_are_checked() {
    let (app, s, _) = setup().await;
    for (src, prop, value) in [
        ("index.html#5", "color", "red;}body{display:none"),
        ("index.html#5", "position", "fixed"),
        ("index.html#99", "color", "#fff"),
        ("../etc#1", "color", "#fff"),
        ("index.html#5", "font-size", "9999px"),
    ] {
        let (status, _, body) = post_form(&app, "/admin/canvas/style", Some(&s), &[("src", src), ("prop", prop), ("value", value)]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{src} {prop} {value}: {body}");
    }
    let (status, _, _) = post_form(&app, "/admin/canvas/style", None, &[("src", "index.html#5"), ("prop", "color"), ("value", "#fff"), ("csrf", "x")]).await;
    assert_ne!(status, StatusCode::OK);
    let editor = admin_session(&app, "editor").await;
    let (status, _, _) = post_form(&app, "/admin/canvas/style", Some(&editor), &[("src", "index.html#5"), ("prop", "color"), ("value", "#fff")]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn text_written_in_the_template_can_be_edited_and_keeps_its_style() {
    let (app, s, base) = setup().await;
    post_form(&app, "/admin/canvas/style", Some(&s), &[("src", "index.html#5"), ("prop", "font-size"), ("value", "40px"), ("tier", "mobile")]).await;
    let (status, _, body) = post_form(&app, "/admin/canvas/text", Some(&s), &[("src", "index.html#5"), ("text", "夏季<新品>")]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, _, preview) = get_with(&app, &format!("{base}/"), Some(&s.0)).await;
    let id = cms_harness::canvas::id("index.html#5");
    assert!(preview.contains(&format!("data-hc=\"{id}\">夏季&lt;新品&gt;</h1>")), "{preview}");
    assert!(preview.contains("@media (max-width: 599.98px){"));
    // Text that comes from data is not edited here.
    let (status, _, _) = post_form(&app, "/admin/canvas/text", Some(&s), &[("src", "index.html#6"), ("text", "x")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    publish_all(&app);
    let live = get_text(&app, "/").await;
    assert!(live.contains(&format!("<h1 data-hc=\"{id}\">夏季&lt;新品&gt;</h1>")), "{live}");
}

#[tokio::test]
async fn elements_are_copied_and_deleted_and_edits_move_with_them() {
    let app = app();
    let page = "<!doctype html><html><head><title>t</title></head><body><section><h2>甲</h2></section><p>乙</p></body></html>";
    ok(&app, &builder(), "write_site_file", json!({ "path": "index.html", "content": page, "note": "首页" }));
    publish_all(&app);
    let s = admin_session(&app, "builder").await;
    // <p> is the 7th start tag: html head title body section h2 p.
    let set = |v: &'static str| [("src", "index.html#7"), ("prop", "translate"), ("value", v), ("tier", "desktop")];
    let (_, _, body) = post_form(&app, "/admin/canvas/style", Some(&s), &set("10px 20px")).await;
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["old"], Value::Null);
    let (_, _, body) = post_form(&app, "/admin/canvas/style", Some(&s), &set("11px 20px")).await;
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["old"], "10px 20px");

    let (status, _, body) = post_form(&app, "/admin/canvas/element", Some(&s), &[("src", "index.html#5"), ("action", "duplicate")]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let draft = ok(&app, &reader(), "get_site_file", json!({ "path": "index.html" }))["content"].as_str().unwrap().to_string();
    assert!(draft.contains("<section><h2>甲</h2></section><section><h2>甲</h2></section><p>乙</p>"), "{draft}");
    let canvas = cms_harness::changes::setting(&app.conn(), "canvas", true);
    assert!(canvas["els"].get("index.html#9").is_some(), "{canvas}");

    let (status, _, _) = post_form(&app, "/admin/canvas/element", Some(&s), &[("src", "index.html#9"), ("action", "remove")]).await;
    assert_eq!(status, StatusCode::OK);
    let canvas = cms_harness::changes::setting(&app.conn(), "canvas", true);
    assert!(canvas["els"].as_object().is_none_or(|m| m.is_empty()), "{canvas}");
    let (status, _, _) = post_form(&app, "/admin/canvas/element", Some(&s), &[("src", "index.html#1"), ("action", "remove")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_ai_sees_hand_edits_and_does_not_drop_them_silently() {
    let (app, s, _) = setup().await;
    post_form(&app, "/admin/canvas/style", Some(&s), &[("src", "index.html#5"), ("prop", "color"), ("value", "#123456")]).await;
    let file = ok(&app, &builder(), "get_site_file", json!({ "path": "index.html" }));
    assert_eq!(file["hand_edits"][0]["element"], "index.html#5", "{file}");
    // Something added above: the edit moves with its element.
    let moved = PAGE.replace("<body>", "<body><nav>导航</nav>");
    ok(&app, &builder(), "write_site_file", json!({ "path": "index.html", "content": moved, "note": "加导航" }));
    let canvas = cms_harness::changes::setting(&app.conn(), "canvas", true);
    assert!(canvas["els"].get("index.html#6").is_some(), "{canvas}");
    // The element itself rewritten: refused until the user agrees.
    let gone = moved.replace("春季新品", "冬季");
    let e = err(&app, &builder(), "write_site_file", json!({ "path": "index.html", "content": gone, "note": "改标题" }));
    assert!(e.contains("replace_hand_edits") && e.contains("index.html#6"), "{e}");
    ok(&app, &builder(), "write_site_file", json!({ "path": "index.html", "content": gone, "note": "改标题", "replace_hand_edits": true }));
    let canvas = cms_harness::changes::setting(&app.conn(), "canvas", true);
    assert!(canvas["els"].as_object().is_none_or(|m| m.is_empty()), "{canvas}");
}

#[tokio::test]
async fn a_shared_element_changes_every_page_or_only_this_one() {
    let app = app();
    let w = |p: &str, c: &str| ok(&app, &builder(), "write_site_file", json!({ "path": p, "content": c, "note": "页" }));
    w("layout.html", "<!doctype html><html><head><title>t</title></head><body><header>站点</header>{% block main %}{% endblock %}</body></html>");
    w("index.html", "{% extends 'layout.html' %}{% block main %}<p>首页</p>{% endblock %}");
    w("about.html", "{% extends 'layout.html' %}{% block main %}<p>关于</p>{% endblock %}");
    publish_all(&app);
    let s = admin_session(&app, "builder").await;
    let (_, _, body) = get_with(&app, "/admin/canvas/info?src=layout.html%235&page=%2Fabout", Some(&s.0)).await;
    let info: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(info["pages"], 2, "{body}");
    post_form(&app, "/admin/canvas/style", Some(&s), &[("src", "layout.html#5"), ("prop", "color"), ("value", "#111111")]).await;
    post_form(&app, "/admin/canvas/style", Some(&s), &[("src", "layout.html#5"), ("prop", "color"), ("value", "#222222"), ("page", "/about")]).await;
    post_form(&app, "/admin/canvas/style", Some(&s), &[("src", "layout.html#5"), ("prop", "display"), ("value", "none"), ("tier", "mobile")]).await;
    publish_all(&app);
    let home = get_text(&app, "/").await;
    let about = get_text(&app, "/about").await;
    assert!(home.contains("#111111") && !home.contains("#222222"), "{home}");
    assert!(about.contains("#111111") && about.contains("#222222"), "{about}");
    let (_, _, body) = get_with(&app, "/admin/canvas/info?src=layout.html%235&page=%2F", Some(&s.0)).await;
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["tiers"], json!(["mobile"]));
    let (status, _, _) = post_form(&app, "/admin/canvas/reset", Some(&s), &[("src", "layout.html#5"), ("tier", "mobile")]).await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, body) = get_with(&app, "/admin/canvas/info?src=layout.html%235&page=%2F", Some(&s.0)).await;
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["tiers"], json!([]));
}
