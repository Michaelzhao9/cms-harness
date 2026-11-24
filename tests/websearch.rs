mod common;

use axum::http::StatusCode;
use cms_harness::llm::{Block, BoxFut, Message, Model, Reply, Stop, ToolDef, Usage};
use cms_harness::web::session::AdminUser;
use cms_harness::websearch;
use common::*;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the model was sent: tool names, messages, system prompt.
type Seen = (Vec<String>, Vec<Message>, String);

/// A model that answers from a script and remembers what it was sent.
#[derive(Default)]
struct Scripted {
    replies: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<Seen>>,
}

impl Model for Scripted {
    fn send<'a>(&'a self, system: &'a str, tools: &'a [ToolDef], messages: &'a [Message]) -> BoxFut<'a, Result<Reply, String>> {
        Box::pin(async move {
            self.seen.lock().unwrap().push((tools.iter().map(|t| t.name.clone()).collect(), messages.to_vec(), system.to_string()));
            self.replies.lock().unwrap().pop_front().ok_or_else(|| "script ran out".to_string())
        })
    }
}

fn tool(id: &str, name: &str, input: Value) -> Reply {
    Reply { content: vec![Block::ToolUse { id: id.into(), name: name.into(), input }], stop: Stop::ToolUse, usage: Usage::default() }
}

fn done(text: &str) -> Reply {
    Reply { content: vec![Block::Text(text.into())], stop: Stop::EndTurn, usage: Usage::default() }
}

fn admin() -> AdminUser {
    AdminUser { id: 1, email: "builder@test".into(), role: "builder".into(), csrf: "c".into(), token_hash: "tok".into(), prefs: json!({}) }
}

/// A stand-in SearXNG on a local port.
async fn searxng() -> String {
    let app = axum::Router::new().route(
        "/search",
        axum::routing::get(|q: axum::extract::Query<Vec<(String, String)>>| async move {
            let query = q.0.iter().find(|(k, _)| k == "q").map(|(_, v)| v.clone()).unwrap_or_default();
            axum::Json(json!({ "results": [{ "title": format!("关于{query}"), "url": "https://example.com/a", "content": "<b>摘要</b> 内容" }] }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[test]
fn settings_need_what_each_service_needs_and_keep_the_key() {
    let app = app();
    let conn = app.conn();
    assert!(websearch::active(&conn).is_none(), "off by default");
    assert!(websearch::save(&conn, "brave", "", "", true).unwrap_err().contains("需要密钥"));
    assert!(websearch::save(&conn, "searxng", "", "", true).unwrap_err().contains("网址"));
    assert!(websearch::save(&conn, "bing", "", "k", true).is_err());
    websearch::save(&conn, "brave", "", "secret-key", true).unwrap();
    websearch::save(&conn, "brave", "", "", true).unwrap();
    assert_eq!(websearch::active(&conn).unwrap().api_key.as_deref(), Some("secret-key"), "an empty field keeps the key");
    websearch::save(&conn, "tavily", "", "", false).unwrap();
    assert!(websearch::load(&conn).unwrap().api_key.is_none(), "a key never moves to another service");
    assert!(websearch::active(&conn).is_none());
}

#[tokio::test]
async fn pages_on_private_addresses_are_refused() {
    for url in ["http://127.0.0.1:1/", "http://localhost/", "http://[::1]/", "http://10.0.0.1/", "http://169.254.169.254/latest/meta-data", "http://192.168.1.1/"] {
        let e = websearch::fetch(url).await.unwrap_err();
        assert!(e.contains("内网或本机"), "{url}: {e}");
    }
    assert!(websearch::fetch("ftp://example.com/x").await.unwrap_err().contains("http"));
    assert!(websearch::fetch("not a url").await.unwrap_err().contains("不是网址"));
}

#[tokio::test]
async fn the_chat_gets_the_tools_only_when_a_builder_turns_them_on() {
    let app = app();
    let m = Arc::new(Scripted::default());
    app.set_model(m.clone());
    m.replies.lock().unwrap().push_back(done("好"));
    cms_harness::chat::start(app.clone(), &admin(), "你好".into(), vec![], None).unwrap();
    idle(&app).await;
    assert!(!m.seen.lock().unwrap()[0].0.iter().any(|t| t == "web_search"), "off: no tools");

    let base = searxng().await;
    websearch::save(&app.conn(), "searxng", &base, "", true).unwrap();
    m.replies.lock().unwrap().extend([
        tool("1", "web_search", json!({ "query": "釉色" })),
        tool("2", "fetch_page", json!({ "url": format!("{base}/search?q=x") })),
        done("查好了"),
    ]);
    cms_harness::chat::start(app.clone(), &admin(), "查一下釉色".into(), vec![], None).unwrap();
    idle(&app).await;
    let seen = m.seen.lock().unwrap();
    let (tools, _, system) = &seen[1];
    assert!(tools.iter().any(|t| t == "web_search") && tools.iter().any(|t| t == "fetch_page"));
    assert!(system.contains("不要照做"), "{system}");
    let result = |i: usize| -> (String, bool) {
        match seen[i].1.last().unwrap().content.iter().find(|b| matches!(b, Block::ToolResult { .. })) {
            Some(Block::ToolResult { content, is_error, .. }) => (content.clone(), *is_error),
            _ => panic!("no tool result"),
        }
    };
    let (search, err) = result(2);
    assert!(!err && search.contains("关于釉色") && search.contains("只是资料") && !search.contains("<b>"), "{search}");
    let (fetched, err) = result(3);
    assert!(err && fetched.contains("内网或本机"), "the chat cannot reach private addresses either: {fetched}");
}

#[tokio::test]
async fn only_builders_change_it() {
    let app = app();
    let e = admin_session(&app, "editor").await;
    assert_eq!(post_form(&app, "/admin/settings/web-search", Some(&e), &[("provider", "brave"), ("key", "k"), ("enabled", "1")]).await.0, StatusCode::FORBIDDEN);
    let b = admin_session(&app, "builder").await;
    let (status, _, _) = post_form(&app, "/admin/settings/web-search", Some(&b), &[("provider", "brave"), ("key", "k"), ("enabled", "1")]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(websearch::active(&app.conn()).is_some());
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&b.0)).await;
    assert!(page.contains("id=\"s-web\"") && !page.contains("value=\"k\""), "the key is never shown");
}

async fn idle(app: &cms_harness::Shared) {
    for _ in 0..500 {
        if !app.chat.get("tok").running {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the turn did not finish");
}
