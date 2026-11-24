mod common;

use common::*;
use serde_json::json;

fn seed(app: &cms_harness::Shared) {
    ok(app, &builder(), "create_collection", json!({"name": "products", "title": "产品", "note": "x", "fields": [
        {"name": "title", "title": "名称", "type": "text"},
        {"name": "price", "title": "价格", "type": "number"},
        {"name": "kind", "title": "分类", "type": "select", "options": ["茶", "器"], "index": true},
        {"name": "photo", "title": "照片", "type": "media"}
    ]}));
    for (t, k) in [("龙井", "茶"), ("青瓷杯", "器"), ("白茶", "茶")] {
        ok(app, &builder(), "create_record", json!({"collection": "products", "data": {"title": t, "price": 10, "kind": k}, "note": "x"}));
    }
}

fn view(app: &cms_harness::Shared, extra: serde_json::Value) {
    let mut v = json!({"name": "shelf", "title": "货架", "collection": "products", "columns": ["title", "price"], "note": "x"});
    v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    ok(app, &builder(), "set_view", v);
}

#[tokio::test]
async fn each_layout_renders() {
    let app = app();
    seed(&app);
    let s = admin_session(&app, "builder").await;
    view(&app, json!({}));
    let (_, _, page) = get_with(&app, "/admin/v/shelf", Some(&s.0)).await;
    assert!(page.contains("<table class=\"table\"") && page.contains("龙井"));
    view(&app, json!({"layout": "cards", "title_field": "title", "subtitle_field": "kind", "image_field": "photo", "density": "compact"}));
    let (_, _, page) = get_with(&app, "/admin/v/shelf", Some(&s.0)).await;
    assert!(page.contains("<ul class=\"cards compact\">") && page.contains("class=\"ttl\"") && !page.contains("<table"));
    view(&app, json!({"layout": "list"}));
    let (_, _, page) = get_with(&app, "/admin/v/shelf", Some(&s.0)).await;
    assert!(page.contains("<ul class=\"plain\">") && page.contains("青瓷杯"));
}

#[tokio::test]
async fn group_by_makes_sections_in_option_order() {
    let app = app();
    seed(&app);
    let s = admin_session(&app, "builder").await;
    view(&app, json!({"layout": "list", "group_by": "kind"}));
    let (_, _, page) = get_with(&app, "/admin/v/shelf", Some(&s.0)).await;
    let tea = page.find("<h2 class=\"group\">茶").expect("a 茶 section");
    let ware = page.find("<h2 class=\"group\">器").expect("a 器 section");
    assert!(tea < ware);
    let cup = page.find("青瓷杯").unwrap();
    assert!(cup > ware, "青瓷杯 sits under 器");
}

#[test]
fn invalid_layout_fields_are_refused() {
    let app = app();
    seed(&app);
    let set = |extra: serde_json::Value| {
        let mut v = json!({"name": "shelf", "title": "货架", "collection": "products", "columns": ["title"], "note": "x"});
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        err(&app, &builder(), "set_view", v)
    };
    assert!(set(json!({"title_field": "nope"})).contains("title_field"));
    assert!(set(json!({"image_field": "title"})).contains("media"));
    assert!(set(json!({"group_by": "title"})).contains("group_by"));
    assert!(set(json!({"group_by": "photo"})).contains("group_by"));
    assert!(set(json!({"layout": "grid"})).contains("layout") || set(json!({"layout": "grid"})).contains("grid"));
}

#[tokio::test]
async fn an_old_view_without_layout_still_renders_as_a_table() {
    let app = app();
    seed(&app);
    // A declaration stored before layouts existed.
    app.conn().execute(
        "INSERT INTO views(name, draft) VALUES ('legacy', ?1)",
        [json!({"name": "legacy", "title": "旧界面", "collection": "products", "columns": ["title"], "filters": [], "sort": null, "actions": []}).to_string()],
    ).unwrap();
    let s = admin_session(&app, "builder").await;
    let (status, _, page) = get_with(&app, "/admin/v/legacy", Some(&s.0)).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(page.contains("<table class=\"table\"") && page.contains("旧界面"));
    let listed = ok(&app, &builder(), "list_views", json!({}));
    assert!(!listed.to_string().contains("\"layout\""), "defaults are not written into old declarations");
}
