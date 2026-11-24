//! The preview canvas: hand edits are drafts, reach the preview at once and the live site on publish.
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::{json, Value};

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
