//! One test per finding of the whole-branch review, so none of them comes back.
mod common;
use base64::Engine;
use common::*;
use cms_harness::auth::Scope;
use serde_json::{json, Value};

fn items(app: &cms_harness::Shared, fields: Value) {
    ok(app, &builder(), "create_collection", json!({ "name": "items", "title": "条目", "public_read": true, "note": "n", "fields": fields }));
}

fn item(app: &cms_harness::Shared, data: Value) -> i64 {
    ok(app, &editor(), "create_record", json!({ "collection": "items", "note": "n", "data": data }))["id"].as_i64().unwrap()
}

fn live(app: &cms_harness::Shared, id: i64) -> Value {
    let json: String = app.conn().query_row("SELECT published FROM records WHERE id = ?1", [id], |r| r.get(0)).unwrap();
    serde_json::from_str(&json).unwrap()
}

#[test]
fn restoring_an_old_version_cleans_its_html_again() {
    let app = app();
    items(&app, json!([{ "name": "name", "title": "名称", "type": "text" }]));
    let a = item(&app, json!({ "name": "<img src=x onerror=alert(1)>a<b" }));
    publish_all(&app);
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "name", "type": "richtext" }] }));
    publish_all(&app);
    let old = ok(&app, &reader(), "list_versions", json!({ "target": format!("record:{a}") })).as_array().unwrap().last().unwrap()["id"].clone();
    ok(&app, &editor(), "restore_version", json!({ "target": format!("record:{a}"), "version_id": old, "note": "恢复" }));
    publish_all(&app);
    let name = live(&app, a)["name"].as_str().unwrap().to_string();
    assert!(!name.contains("onerror"), "{name}");
}

#[test]
fn text_becoming_richtext_is_escaped_not_parsed() {
    let app = app();
    items(&app, json!([{ "name": "name", "title": "名称", "type": "text" }]));
    let a = item(&app, json!({ "name": "Use <slot> & <b>x</b> if a<b" }));
    publish_all(&app);
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "name", "type": "richtext" }] }));
    publish_all(&app);
    assert_eq!(live(&app, a)["name"], "Use &lt;slot&gt; &amp; &lt;b&gt;x&lt;/b&gt; if a&lt;b");
    // And back to text gives the original string.
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "name", "type": "text" }] }));
    publish_all(&app);
    assert_eq!(live(&app, a)["name"], "Use <slot> & <b>x</b> if a<b");
}

#[test]
fn a_draft_is_converted_exactly_once() {
    let app = app();
    items(&app, json!([{ "name": "name", "title": "名称", "type": "text" }]));
    let a = item(&app, json!({ "name": "a<b" }));
    publish_all(&app);
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "n", "data": { "name": "c<d" } }));
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "name", "type": "richtext" }] }));
    publish_all(&app);
    assert_eq!(live(&app, a)["name"], "c&lt;d"); // not c&amp;lt;d
}

#[test]
fn discarding_a_never_published_collection_with_records_needs_the_count() {
    let app = app();
    news(&app);
    ok(&app, &editor(), "create_record", json!({ "collection": "news", "note": "n", "data": { "title": "A", "slug": "a" } }));
    let e = err(&app, &builder(), "discard_draft", json!({ "target": "collection:news" }));
    assert!(e.contains("delete_collection") && e.contains("confirm: 1"), "{e}");
    assert_eq!(ok(&app, &reader(), "query_records", json!({ "collection": "news" }))["total"], 1);
}

#[test]
fn a_deleted_collection_with_a_pending_definition_can_be_restored() {
    let app = app();
    items(&app, json!([{ "name": "name", "title": "名称", "type": "text" }, { "name": "views", "title": "浏览", "type": "number" }]));
    let a = item(&app, json!({ "name": "A", "views": 5 }));
    publish_all(&app);
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "n", "data": { "name": "A2" } }));
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "views", "type": "text" }] }));
    ok(&app, &builder(), "delete_collection", json!({ "name": "items", "note": "n", "confirm": 1 }));
    publish_all(&app);
    let v = ok(&app, &reader(), "list_versions", json!({ "target": "collection:items" }));
    ok(&app, &builder(), "restore_version", json!({ "target": "collection:items", "version_id": v[1]["id"], "note": "恢复" }));
    publish_all(&app);
    let got = ok(&app, &reader(), "get_record", json!({ "id": a }))["data"].clone();
    assert_eq!((got["name"].as_str(), got["views"].as_i64()), (Some("A2"), Some(5)));
}

#[tokio::test]
async fn defaults_are_stored_so_filters_find_them() {
    let app = app();
    items(&app, json!([
        { "name": "name", "title": "名称", "type": "text" },
        { "name": "featured", "title": "推荐", "type": "boolean", "default": false, "index": true }
    ]));
    item(&app, json!({ "name": "A" }));
    publish_all(&app);
    assert_eq!(get(&app, "/api/v1/content/items?featured=false", None).await.2["total"], 1);
    // A field added later, with a default, is filled into existing records.
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n",
        "add_fields": [{ "name": "rank", "title": "排序", "type": "number", "default": 0, "index": true }] }));
    publish_all(&app);
    assert_eq!(get(&app, "/api/v1/content/items?rank=0", None).await.2["total"], 1);
}

#[test]
fn an_editors_publish_all_is_not_blocked_by_someone_elses_structure_draft() {
    let app = app();
    news(&app);
    publish_all(&app);
    // The builder starts a new collection; the editor drafts a record in it and one in news.
    ok(&app, &builder(), "create_collection", json!({ "name": "products", "title": "产品", "note": "n", "fields": [{ "name": "name", "title": "名称", "type": "text" }] }));
    let p = ok(&app, &editor(), "create_record", json!({ "collection": "products", "note": "n", "data": { "name": "鼓" } }))["id"].as_i64().unwrap();
    let n = ok(&app, &editor(), "create_record", json!({ "collection": "news", "note": "n", "data": { "title": "A", "slug": "a" } }))["id"].as_i64().unwrap();
    let release = ok(&app, &editor(), "publish", json!({ "note": "n" }));
    assert_eq!(release["published"], json!([format!("record:{n}")]));
    let skipped: Vec<String> = serde_json::from_value(release["skipped"].clone()).unwrap();
    assert!(skipped.contains(&"collection:products".to_string()) && skipped.contains(&format!("record:{p}")), "{skipped:?}");
}

#[test]
fn a_published_record_stays_editable_while_its_collection_changes_type() {
    let app = app();
    items(&app, json!([{ "name": "name", "title": "名称", "type": "text" }, { "name": "views", "title": "浏览", "type": "number" }]));
    let a = item(&app, json!({ "name": "A", "views": 5 }));
    publish_all(&app);
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "views", "type": "text" }] }));
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "n", "data": { "name": "A2" } }));
    publish_all(&app);
    assert_eq!(live(&app, a)["views"], "5");
}

#[test]
fn a_record_being_deleted_does_not_block_a_structure_change() {
    let app = app();
    items(&app, json!([{ "name": "kind", "title": "种类", "type": "select", "options": ["a", "b"] }]));
    let a = item(&app, json!({ "kind": "a" }));
    publish_all(&app);
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "n", "data": { "kind": "b" } }));
    ok(&app, &editor(), "delete_record", json!({ "id": a, "note": "删" }));
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "kind", "options": ["a"] }] }));
}

#[test]
fn a_default_can_be_removed_with_null() {
    let app = app();
    items(&app, json!([{ "name": "name", "title": "名称", "type": "text", "default": "d" }]));
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "name", "default": null }] }));
    let def = ok(&app, &reader(), "get_collection", json!({ "name": "items" }))["definition"].clone();
    assert!(def["fields"][0]["default"].is_null(), "{def}");
}

#[test]
fn a_missing_key_inside_a_field_is_not_reported_as_a_missing_parameter() {
    let app = app();
    let e = err(&app, &builder(), "create_collection", json!({ "name": "x", "title": "X", "note": "n", "fields": [{ "name": "a", "title": "A" }] }));
    assert!(e.contains("fields") && !e.contains("缺少必填参数“type”"), "{e}");
}

#[test]
fn a_view_pending_deletion_is_not_silently_revived() {
    let app = app();
    news(&app);
    let view = json!({ "name": "v", "title": "V", "collection": "news", "columns": ["title"], "note": "n" });
    ok(&app, &builder(), "set_view", view.clone());
    publish_all(&app);
    ok(&app, &builder(), "delete_view", json!({ "name": "v", "note": "n" }));
    assert!(err(&app, &builder(), "set_view", view).contains("discard_draft"));
}

#[test]
fn wrapped_base64_is_accepted() {
    let app = app();
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbImage::new(2, 2).write_to(&mut png, image::ImageFormat::Png).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    let wrapped: String = encoded.as_bytes().chunks(20).map(|c| format!("{}\r\n", std::str::from_utf8(c).unwrap())).collect();
    let path = ok(&app, &editor(), "upload_media", json!({ "filename": "a.png", "data_base64": wrapped }))["path"].clone();
    assert!(path.as_str().unwrap().ends_with(".png"));
}

#[tokio::test]
async fn mcp_rejects_batches_and_accepts_a_lowercase_scheme() {
    let app = app();
    let k = key(&app, &[Scope::Write]);
    let (status, body) = post_json(&app, "/mcp", Some(&k), json!([{ "jsonrpc": "2.0", "id": 1, "method": "ping" }])).await;
    assert_eq!((status.as_u16(), body["error"]["code"].as_i64()), (200, Some(-32600)));
    let resp = cms_harness::api::router(app.clone())
        .oneshot(
            axum::http::Request::get("/api/v1/tools")
                .header("authorization", format!("bearer {k}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

use tower::ServiceExt;

// ---- found by the re-review of the fix wave ----

fn draft(app: &cms_harness::Shared, id: i64) -> Value {
    let json: String = app.conn().query_row("SELECT coalesce(draft, published) FROM records WHERE id = ?1", [id], |r| r.get(0)).unwrap();
    serde_json::from_str(&json).unwrap()
}

#[test]
fn deleting_then_discarding_or_restoring_keeps_the_text() {
    let app = app();
    items(&app, json!([{ "name": "name", "title": "名称", "type": "text" }]));
    let a = item(&app, json!({ "name": "a" }));
    publish_all(&app);
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "n", "data": { "name": "c<d" } }));
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "name", "type": "richtext" }] }));
    ok(&app, &builder(), "delete_collection", json!({ "name": "items", "note": "n", "confirm": 1 }));
    ok(&app, &builder(), "discard_draft", json!({ "target": "collection:items" }));
    assert_eq!(draft(&app, a)["name"], "c<d");
    // The same, published and then restored.
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "name", "type": "richtext" }] }));
    ok(&app, &builder(), "delete_collection", json!({ "name": "items", "note": "n", "confirm": 1 }));
    publish_all(&app);
    let v = ok(&app, &reader(), "list_versions", json!({ "target": "collection:items" }));
    ok(&app, &builder(), "restore_version", json!({ "target": "collection:items", "version_id": v[1]["id"], "note": "恢复" }));
    publish_all(&app);
    assert_eq!(live(&app, a)["name"], "c<d");
}

#[test]
fn discarding_a_deletion_with_a_number_to_text_change_pending_works() {
    let app = app();
    items(&app, json!([{ "name": "views", "title": "浏览", "type": "number" }]));
    let a = item(&app, json!({ "views": 5 }));
    publish_all(&app);
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "n", "data": { "views": 6 } }));
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "views", "type": "text" }] }));
    ok(&app, &builder(), "delete_collection", json!({ "name": "items", "note": "n", "confirm": 1 }));
    ok(&app, &builder(), "discard_draft", json!({ "target": "collection:items" }));
    assert_eq!(draft(&app, a)["views"], 6);
}

#[test]
fn a_record_published_ahead_of_its_definition_is_converted_once() {
    let app = app();
    items(&app, json!([{ "name": "name", "title": "名称", "type": "text" }]));
    let a = item(&app, json!({ "name": "a" }));
    publish_all(&app);
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "name", "type": "richtext" }] }));
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "n", "data": { "name": "c&lt;d" } }));
    ok(&app, &editor(), "publish", json!({ "note": "n", "targets": [format!("record:{a}")] }));
    assert_eq!(live(&app, a)["name"], "c<d"); // back in the live (text) shape
    publish_all(&app);
    assert_eq!(live(&app, a)["name"], "c&lt;d"); // converted once by the definition
}

#[test]
fn raw_html_cannot_go_live_ahead_of_a_richtext_to_text_change() {
    let app = app();
    items(&app, json!([{ "name": "body", "title": "正文", "type": "richtext" }]));
    let a = item(&app, json!({ "body": "<p>x</p>" }));
    publish_all(&app);
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "body", "type": "text" }] }));
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "n", "data": { "body": "<img src=x onerror=alert(1)> 1<2" } }));
    ok(&app, &editor(), "publish", json!({ "note": "n", "targets": [format!("record:{a}")] }));
    let body = live(&app, a)["body"].as_str().unwrap().to_string();
    // Escaped into harmless text: no real tag survives.
    assert!(!body.contains("<img") && body.contains("1&lt;2"), "{body}");
}

#[tokio::test]
async fn a_default_added_to_an_existing_field_is_stored() {
    let app = app();
    items(&app, json!([
        { "name": "name", "title": "名称", "type": "text" },
        { "name": "featured", "title": "推荐", "type": "boolean", "index": true }
    ]));
    item(&app, json!({ "name": "A" }));
    publish_all(&app);
    ok(&app, &builder(), "update_collection", json!({ "name": "items", "note": "n", "change_fields": [{ "name": "featured", "default": false }] }));
    publish_all(&app);
    assert_eq!(get(&app, "/api/v1/content/items?featured=false", None).await.2["total"], 1);
}

#[test]
fn unique_and_default_cannot_be_combined() {
    let app = app();
    let e = err(&app, &builder(), "create_collection", json!({ "name": "x", "title": "X", "note": "n",
        "fields": [{ "name": "code", "title": "编号", "type": "text", "unique": true, "default": "x" }] }));
    assert!(e.contains("code") && e.contains("unique"), "{e}");
}
