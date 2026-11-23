mod common;
use common::*;
use harness_cms::auth::Scope;
use serde_json::{json, Value};

async fn tool(app: &harness_cms::Shared, key: &str, name: &str, args: Value) -> Value {
    let (_, out) = post_json(app, "/mcp", Some(key), json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": args } })).await;
    assert_eq!(out["result"]["isError"], false, "{name}: {out}");
    serde_json::from_str(out["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn an_agent_builds_a_site_over_mcp_and_a_frontend_reads_it() {
    let app = app();
    let k = key(&app, &[Scope::Write, Scope::Structure, Scope::Publish]);

    // Acceptance 1: two collections, content, published, readable by a frontend with no key.
    tool(&app, &k, "create_collection", json!({ "name": "news", "title": "新闻", "public_read": true, "note": "建新闻",
        "fields": [{ "name": "title", "title": "标题", "type": "text", "required": true },
                   { "name": "date", "title": "日期", "type": "date", "index": true },
                   { "name": "cover", "title": "封面", "type": "media" }] })).await;
    tool(&app, &k, "create_collection", json!({ "name": "products", "title": "产品", "public_read": true, "note": "建产品",
        "fields": [{ "name": "name", "title": "名称", "type": "text", "required": true },
                   { "name": "price", "title": "价格", "type": "number", "index": true }] })).await;
    let id = tool(&app, &k, "create_record", json!({ "collection": "news", "note": "第一条",
        "data": { "title": "开业", "date": "2026-11-23" } })).await["id"].as_i64().unwrap();
    tool(&app, &k, "create_record", json!({ "collection": "products", "note": "第一个", "data": { "name": "小军鼓", "price": 1200 } })).await;
    assert_eq!(get(&app, "/api/v1/content/news", None).await.0, axum::http::StatusCode::NOT_FOUND); // nothing live yet
    assert_eq!(tool(&app, &k, "publish", json!({ "note": "首发" })).await["published"].as_array().unwrap().len(), 4);
    assert_eq!(get(&app, "/api/v1/content/news?sort=-date", None).await.2["items"][0]["title"], "开业");
    assert_eq!(get(&app, "/api/v1/content/products?price__gte=1000", None).await.2["total"], 1);

    // Acceptance 3: add a field to a collection that already holds data; nothing is lost.
    tool(&app, &k, "update_collection", json!({ "name": "news", "note": "加摘要",
        "add_fields": [{ "name": "summary", "title": "摘要", "type": "text", "default": "" }] })).await;
    tool(&app, &k, "publish", json!({ "note": "加字段" })).await;
    let one = get(&app, &format!("/api/v1/content/news/{id}"), None).await.2;
    assert_eq!((one["title"].as_str(), one["summary"].as_str()), (Some("开业"), Some("")));
}
