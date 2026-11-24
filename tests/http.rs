mod common;
use axum::http::StatusCode;
use common::*;
use cms_harness::auth::Scope;
use serde_json::json;

#[tokio::test]
async fn rest_lists_and_calls_tools_by_scope() {
    let app = app();
    let read = key(&app, &[]);
    let build = key(&app, &[Scope::Write, Scope::Structure, Scope::Publish]);
    assert_eq!(get(&app, "/api/v1/tools", None).await.0, StatusCode::UNAUTHORIZED);
    let names = |v: &serde_json::Value| -> Vec<String> { v["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect() };
    let listed = get(&app, "/api/v1/tools", Some(&read)).await.2;
    assert!(names(&listed).contains(&"query_records".to_string()) && !names(&listed).contains(&"publish".to_string()));
    assert_eq!(listed["tools"][0]["inputSchema"]["type"], "object");
    assert!(names(&get(&app, "/api/v1/tools", Some(&build)).await.2).contains(&"publish".to_string()));

    let body = json!({ "name": "news", "title": "新闻", "note": "n", "fields": [{ "name": "title", "title": "标题", "type": "text" }] });
    let (status, out) = post_json(&app, "/api/v1/tools/create_collection", Some(&build), body.clone()).await;
    assert_eq!((status, out["ok"].as_bool(), out["result"]["status"].as_str()), (StatusCode::OK, Some(true), Some("draft")));
    let (status, out) = post_json(&app, "/api/v1/tools/create_collection", Some(&read), body).await;
    assert_eq!((status, out["ok"].as_bool()), (StatusCode::BAD_REQUEST, Some(false)));
    assert!(out["error"].as_str().unwrap().contains("structure"));
}

#[tokio::test]
async fn mcp_gives_the_same_results_as_rest() {
    let app = app();
    let k = key(&app, &[Scope::Write, Scope::Structure, Scope::Publish]);
    let rpc = |method: &str, params: serde_json::Value| json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });

    let (_, init) = post_json(&app, "/mcp", Some(&k), rpc("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {} }))).await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "cms-harness");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert_eq!(post_json(&app, "/mcp", Some(&k), rpc("ping", json!({}))).await.1["result"], json!({}));

    let (_, listed) = post_json(&app, "/mcp", Some(&k), rpc("tools/list", json!({}))).await;
    let rest = get(&app, "/api/v1/tools", Some(&k)).await.2;
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), rest["tools"].as_array().unwrap().len());

    let args = json!({ "name": "news", "title": "新闻", "note": "n", "fields": [] });
    let (_, called) = post_json(&app, "/mcp", Some(&k), rpc("tools/call", json!({ "name": "create_collection", "arguments": args.clone() }))).await;
    assert_eq!(called["result"]["isError"], false);
    let text: serde_json::Value = serde_json::from_str(called["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text["status"], "draft");
    let (_, again) = post_json(&app, "/mcp", Some(&k), rpc("tools/call", json!({ "name": "create_collection", "arguments": args.clone() }))).await;
    assert_eq!(again["result"]["isError"], true);
    let (_, rest_again) = post_json(&app, "/api/v1/tools/create_collection", Some(&k), args).await;
    assert_eq!(again["result"]["content"][0]["text"], rest_again["error"]); // identical message on both entrances

    assert_eq!(post_json(&app, "/mcp", Some(&k), rpc("nope", json!({}))).await.1["error"]["code"], -32601);
    let (status, _) = post_json(&app, "/mcp", Some(&k), json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(post_json(&app, "/mcp", None, rpc("ping", json!({}))).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oversize_and_malformed_bodies_do_not_crash() {
    let app = app();
    let k = key(&app, &[Scope::Write]);
    let big = json!({ "filename": "a.png", "data_base64": "A".repeat(9 * 1024 * 1024) });
    assert_eq!(post_json(&app, "/api/v1/tools/upload_media", Some(&k), big).await.0, StatusCode::PAYLOAD_TOO_LARGE);
    let (status, body) = post_raw(&app, "/api/v1/tools/get_record", Some(&k), "{not json").await;
    assert_eq!((status, body["ok"].as_bool()), (StatusCode::BAD_REQUEST, Some(false)));
    assert_eq!(get(&app, "/api/v1/tools", Some(&k)).await.0, StatusCode::OK); // still alive
}

#[tokio::test]
async fn a_fresh_site_has_a_front_page() {
    let app = app();
    let (status, headers, _) = get(&app, "/", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers["content-type"].to_str().unwrap().starts_with("text/html"));
    let html = get_text(&app, "/").await;
    assert!(html.contains("测试站") && html.contains("网站还没有内容"), "{html}");
}
