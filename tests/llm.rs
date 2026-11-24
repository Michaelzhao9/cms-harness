use axum::{body::Bytes, extract::State, http::HeaderMap, response::IntoResponse, routing::post, Router};
use harness_cms::llm::*;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Canned {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
}

fn ok_json(v: Value) -> Canned {
    Canned { status: 200, headers: vec![("content-type", "application/json".into())], body: v.to_string() }
}

#[derive(Default)]
struct Log {
    requests: Vec<(HeaderMap, Value)>,
    replies: VecDeque<Canned>,
}

type Shared = Arc<Mutex<Log>>;

async fn handle(State(log): State<Shared>, headers: HeaderMap, body: Bytes) -> impl IntoResponse {
    let mut l = log.lock().unwrap();
    l.requests.push((headers, serde_json::from_slice(&body).unwrap_or(Value::Null)));
    let c = l.replies.pop_front().expect("a canned reply for every request");
    let mut resp = (axum::http::StatusCode::from_u16(c.status).unwrap(), c.body).into_response();
    for (k, v) in c.headers {
        resp.headers_mut().insert(k, v.parse().unwrap());
    }
    resp
}

async fn server(replies: Vec<Canned>) -> (String, Shared) {
    let log: Shared = Arc::new(Mutex::new(Log { requests: vec![], replies: replies.into() }));
    let app = Router::new()
        .route("/v1/messages", post(handle))
        .route("/v1/chat/completions", post(handle))
        .route("/v1/models", axum::routing::get(handle))
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), log)
}

fn anthropic(base: &str) -> Anthropic {
    Anthropic::new(Settings { provider: Provider::Anthropic, api_key: Some("sk-test".into()), model: DEFAULT_MODEL.into(), base_url: base.into() })
}

fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef { name: "get_record".into(), description: "读一条记录".into(), input_schema: json!({"type": "object"}) },
        ToolDef { name: "update_record".into(), description: "改一条记录".into(), input_schema: json!({"type": "object"}) },
    ]
}

#[tokio::test]
async fn anthropic_request_has_the_required_shape() {
    let (base, log) = server(vec![ok_json(json!({"content": [{"type": "text", "text": "好"}], "stop_reason": "end_turn", "usage": {"input_tokens": 5, "output_tokens": 1}}))]).await;
    let reply = anthropic(&base).send("系统说明", &tools(), &[Message::user_text("你好")]).await.unwrap();
    assert_eq!(reply.stop, Stop::EndTurn);
    assert_eq!(reply.text(), "好");
    assert_eq!(reply.usage, Usage { input_tokens: 5, output_tokens: 1 });

    let l = log.lock().unwrap();
    let (h, body) = &l.requests[0];
    assert_eq!(h["x-api-key"], "sk-test");
    assert_eq!(h["anthropic-version"], "2026-10-21");
    assert_eq!(h["anthropic-beta"], "server-side-fallback-2026-07-01");
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["max_tokens"], 16000);
    assert_eq!(body["output_config"], json!({"effort": "medium"}));
    assert_eq!(body["fallbacks"], "default");
    assert_eq!(body["system"], "系统说明");
    assert!(body.get("thinking").is_none(), "no thinking field");
    assert!(body.get("tool_choice").is_none(), "tool choice stays auto");
    assert!(body.get("stream").is_none());
    let t = body["tools"].as_array().unwrap();
    assert!(t[0].get("cache_control").is_none());
    assert_eq!(t[1]["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!(body["messages"], json!([{"role": "user", "content": [{"type": "text", "text": "你好"}]}]));
}

#[tokio::test]
async fn opaque_blocks_round_trip_unchanged() {
    let thinking = json!({"type": "thinking", "thinking": "先读记录", "signature": "sig=="});
    let cited = json!({"type": "text", "text": "见上", "citations": [{"type": "char_location", "cited_text": "x"}]});
    let first = json!({"content": [thinking, cited, {"type": "tool_use", "id": "tu_1", "name": "get_record", "input": {"id": 3}}], "stop_reason": "tool_use", "usage": {}});
    let second = json!({"content": [{"type": "text", "text": "完成"}], "stop_reason": "end_turn", "usage": {}});
    let (base, log) = server(vec![ok_json(first), ok_json(second)]).await;
    let m = anthropic(&base);

    let mut history = vec![Message::user_text("改一下")];
    let reply = m.send("", &tools(), &history).await.unwrap();
    assert_eq!(reply.stop, Stop::ToolUse);
    assert_eq!(reply.tool_uses().map(|(id, name, _)| (id.to_string(), name.to_string())).collect::<Vec<_>>(), vec![("tu_1".into(), "get_record".into())]);
    assert_eq!(reply.text(), "见上");
    history.push(reply.into_message());
    history.push(Message { role: Role::User, content: vec![Block::ToolResult { tool_use_id: "tu_1".into(), content: "{}".into(), is_error: false }] });
    m.send("", &tools(), &history).await.unwrap();

    let l = log.lock().unwrap();
    let sent = &l.requests[1].1["messages"];
    assert_eq!(sent[1]["role"], "assistant");
    assert_eq!(sent[1]["content"][0], thinking);
    assert_eq!(sent[1]["content"][1], cited);
    assert_eq!(sent[1]["content"][2], json!({"type": "tool_use", "id": "tu_1", "name": "get_record", "input": {"id": 3}}));
    assert_eq!(sent[2]["content"][0], json!({"type": "tool_result", "tool_use_id": "tu_1", "content": "{}", "is_error": false}));
}

#[tokio::test]
async fn refusal_maps_with_its_category() {
    let (base, _) = server(vec![
        ok_json(json!({"content": [], "stop_reason": "refusal", "stop_details": {"type": "refusal", "category": "cyber"}, "usage": {}})),
        ok_json(json!({"content": [], "stop_reason": "refusal", "stop_details": null, "usage": {}})),
        ok_json(json!({"content": [], "stop_reason": "pause_turn", "usage": {}})),
    ])
    .await;
    let m = anthropic(&base);
    let hi = [Message::user_text("hi")];
    assert_eq!(m.send("", &[], &hi).await.unwrap().stop, Stop::Refusal(Some("cyber".into())));
    assert_eq!(m.send("", &[], &hi).await.unwrap().stop, Stop::Refusal(None));
    assert_eq!(m.send("", &[], &hi).await.unwrap().stop, Stop::PauseTurn);
}

#[tokio::test]
async fn a_429_then_200_succeeds_and_a_400_does_not_retry() {
    let busy = Canned { status: 429, headers: vec![("retry-after", "0".into())], body: r#"{"error":{"message":"slow down"}}"#.into() };
    let bad = Canned { status: 400, headers: vec![], body: r#"{"error":{"message":"bad field"}}"#.into() };
    let (base, log) = server(vec![busy, ok_json(json!({"content": [], "stop_reason": "end_turn", "usage": {}})), bad]).await;
    let m = anthropic(&base);
    m.send("", &[], &[Message::user_text("hi")]).await.unwrap();
    assert_eq!(log.lock().unwrap().requests.len(), 2);
    let err = m.send("", &[], &[Message::user_text("hi")]).await.unwrap_err();
    assert!(err.contains("400") && err.contains("bad field"), "{err}");
    assert_eq!(log.lock().unwrap().requests.len(), 3);
}

#[tokio::test]
async fn streaming_rebuilds_the_message_and_forwards_text() {
    let events = [
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 9}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "想一"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "下"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "s1"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "改好"}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "了"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "tu_9", "name": "update_record", "input": {}}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"id\": 1"}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": ", \"x\": \"y\"}"}}),
        json!({"type": "content_block_stop", "index": 2}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 30}}),
        json!({"type": "message_stop"}),
    ];
    let body: String = events.iter().map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e)).collect();
    let (base, log) = server(vec![Canned { status: 200, headers: vec![("content-type", "text/event-stream".into())], body }]).await;
    let seen = Arc::new(Mutex::new(String::new()));
    let sink = seen.clone();
    let reply = anthropic(&base)
        .stream("", &tools(), &[Message::user_text("改")], Arc::new(move |t: &str| sink.lock().unwrap().push_str(t)))
        .await
        .unwrap();
    assert_eq!(*seen.lock().unwrap(), "改好了");
    assert_eq!(log.lock().unwrap().requests[0].1["stream"], true);
    assert_eq!(reply.stop, Stop::ToolUse);
    assert_eq!(reply.usage, Usage { input_tokens: 9, output_tokens: 30 });
    assert_eq!(
        reply.content,
        vec![
            Block::Opaque(json!({"type": "thinking", "thinking": "想一下", "signature": "s1"})),
            Block::Text("改好了".into()),
            Block::ToolUse { id: "tu_9".into(), name: "update_record".into(), input: json!({"id": 1, "x": "y"}) },
        ]
    );
}

#[tokio::test]
async fn openai_compatible_maps_requests_and_tool_calls() {
    let reply_json = json!({
        "choices": [{"finish_reason": "tool_calls", "message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "get_record", "arguments": "{\"id\":7}"}}]}}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 4}
    });
    let (base, log) = server(vec![ok_json(reply_json)]).await;
    let m = OpenAiCompatible::new(Settings { provider: Provider::OpenAi, api_key: Some("local".into()), model: "qwen".into(), base_url: format!("{base}/v1") });
    let history = vec![
        Message::user_text("读 7"),
        Message { role: Role::Assistant, content: vec![Block::Opaque(json!({"type": "thinking"})), Block::Text("好".into()), Block::ToolUse { id: "c0".into(), name: "get_record".into(), input: json!({"id": 6}) }] },
        Message { role: Role::User, content: vec![Block::ToolResult { tool_use_id: "c0".into(), content: "{\"id\":6}".into(), is_error: false }] },
    ];
    let reply = m.send("系统", &tools(), &history).await.unwrap();
    assert_eq!(reply.stop, Stop::ToolUse);
    assert_eq!(reply.content, vec![Block::ToolUse { id: "call_1".into(), name: "get_record".into(), input: json!({"id": 7}) }]);
    assert_eq!(reply.usage, Usage { input_tokens: 11, output_tokens: 4 });

    let l = log.lock().unwrap();
    let (h, body) = &l.requests[0];
    assert_eq!(h["authorization"], "Bearer local");
    assert_eq!(body["model"], "qwen");
    assert_eq!(body["tools"][0], json!({"type": "function", "function": {"name": "get_record", "description": "读一条记录", "parameters": {"type": "object"}}}));
    assert_eq!(
        body["messages"],
        json!([
            {"role": "system", "content": "系统"},
            {"role": "user", "content": "读 7"},
            {"role": "assistant", "content": "好", "tool_calls": [{"id": "c0", "type": "function", "function": {"name": "get_record", "arguments": "{\"id\":6}"}}]},
            {"role": "tool", "tool_call_id": "c0", "content": "{\"id\":6}"}
        ])
    );
}

#[test]
fn settings_come_from_the_environment_and_hide_the_key() {
    let vars = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
    assert_eq!(Settings::from_vars(vars(&[])), None, "anthropic without a key cannot run");
    let s = Settings::from_vars(vars(&[("ANTHROPIC_API_KEY", "sk-test-secret")])).unwrap();
    assert_eq!((s.provider, s.model.as_str(), s.base_url.as_str()), (Provider::Anthropic, "claude-opus-5-5", "https://api.anthropic.com"));
    assert!(!format!("{s:?}").contains("sk-test-secret"));
    let s = Settings::from_vars(vars(&[("LLM_API_KEY", "a"), ("ANTHROPIC_API_KEY", "b")])).unwrap();
    assert_eq!(s.api_key.as_deref(), Some("a"));
    assert_eq!(Settings::from_vars(vars(&[("LLM_PROVIDER", "openai"), ("LLM_MODEL", "qwen")])), None, "openai needs a base url");
    let s = Settings::from_vars(vars(&[("LLM_PROVIDER", "OpenAI"), ("LLM_MODEL", "qwen"), ("LLM_BASE_URL", "http://localhost:11434/v1")])).unwrap();
    assert_eq!((s.provider, s.api_key), (Provider::OpenAi, None));
}

#[tokio::test]
async fn anthropic_models_are_listed_across_pages() {
    let (base, log) = server(vec![
        ok_json(json!({"data": [{"id": "claude-opus-5-5", "display_name": "Claude Opus 5.5"}], "has_more": true, "last_id": "claude-opus-5-5"})),
        ok_json(json!({"data": [{"id": "claude-haiku-5-5", "display_name": "Claude Haiku 5.5"}], "has_more": false, "last_id": "claude-haiku-5-5"})),
    ])
    .await;
    let s = Settings { provider: Provider::Anthropic, api_key: Some("sk-test".into()), model: String::new(), base_url: base };
    let models = list_models(&s).await.unwrap();
    assert_eq!(models.iter().map(|m| (m.id.as_str(), m.name.as_str())).collect::<Vec<_>>(), vec![("claude-opus-5-5", "Claude Opus 5.5"), ("claude-haiku-5-5", "Claude Haiku 5.5")]);
    assert_eq!(default_model(Provider::Anthropic, &models).as_deref(), Some("claude-opus-5-5"));
    let l = log.lock().unwrap();
    assert_eq!(l.requests[0].0["x-api-key"], "sk-test");
    assert_eq!(l.requests[0].0["anthropic-version"], "2026-10-21");
}

#[tokio::test]
async fn openai_compatible_models_skip_non_chat_ones() {
    let (base, log) = server(vec![ok_json(json!({"data": [{"id": "text-embedding-3"}, {"id": "qwen-max"}, {"id": "deepseek-chat"}, {"id": "whisper-1"}]}))]).await;
    let s = Settings { provider: Provider::OpenAi, api_key: Some("k".into()), model: String::new(), base_url: format!("{base}/v1") };
    let models = list_models(&s).await.unwrap();
    assert_eq!(models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["deepseek-chat", "qwen-max"]);
    assert_eq!(log.lock().unwrap().requests[0].0["authorization"], "Bearer k");
}

#[test]
fn every_preset_is_usable_and_saved_settings_keep_the_key() {
    for p in PRESETS {
        assert!(p.id == "custom" || p.base_url.starts_with("https://") || p.base_url.starts_with("http://localhost"), "{}", p.id);
    }
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    assert_eq!(load_saved(&conn).unwrap(), None);
    assert!(save(&conn, "deepseek", "", "deepseek-chat", None).unwrap_err().contains("密钥"));
    let s = save(&conn, "deepseek", "", "deepseek-chat", Some("sk-1")).unwrap();
    assert_eq!((s.provider, s.base_url.as_str(), s.api_key.as_deref()), (Provider::OpenAi, "https://api.deepseek.com/v1", Some("sk-1")));
    // Saving again without a key keeps the old one: the form never shows it back.
    let s = save(&conn, "deepseek", "", "deepseek-reasoner", None).unwrap();
    assert_eq!((s.model.as_str(), s.api_key.as_deref()), ("deepseek-reasoner", Some("sk-1")));
    assert_eq!(load_saved(&conn).unwrap().unwrap().0, "deepseek");
    assert!(save(&conn, "ollama", "http://localhost:11434/v1", "qwen3", None).is_ok(), "local models need no key");
    assert!(save(&conn, "custom", "ftp://x", "m", None).unwrap_err().contains("http"));
    assert!(save(&conn, "nope", "", "m", None).is_err());
}
