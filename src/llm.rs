//! Model clients for the admin chat: the Anthropic Messages API over raw HTTP (Rust has no official SDK)
//! and any OpenAI-compatible endpoint, behind one provider-neutral `Model` trait.

use serde_json::{json, Map, Value};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub const ANTHROPIC_VERSION: &str = "2026-10-21";
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
pub const DEFAULT_ANTHROPIC_URL: &str = "https://api.anthropic.com";
pub const MAX_TOKENS: u32 = 16000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_RETRIES: u32 = 2;
const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAi,
}

/// Where the chat model lives. The key is never printed: `Debug` hides it.
#[derive(Clone, PartialEq, Eq)]
pub struct Settings {
    pub provider: Provider,
    pub api_key: Option<String>,
    pub model: String,
    pub base_url: String,
}

impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Settings")
            .field("provider", &self.provider)
            .field("api_key", &self.api_key.as_ref().map(|_| "<set>"))
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl Settings {
    pub fn from_env() -> Option<Settings> {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    /// `LLM_PROVIDER` (anthropic | openai), `LLM_API_KEY` (falls back to `ANTHROPIC_API_KEY`), `LLM_MODEL`, `LLM_BASE_URL`.
    /// None when the chat cannot run: Anthropic without a key, OpenAI-compatible without a model or base URL.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Option<Settings> {
        let var = |k: &str| get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let api_key = var("LLM_API_KEY").or_else(|| var("ANTHROPIC_API_KEY"));
        let provider = match var("LLM_PROVIDER").as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("openai") => Provider::OpenAi,
            _ => Provider::Anthropic,
        };
        match provider {
            Provider::Anthropic => Some(Settings {
                provider,
                api_key: Some(api_key?),
                model: var("LLM_MODEL").unwrap_or_else(|| DEFAULT_MODEL.into()),
                base_url: var("LLM_BASE_URL").unwrap_or_else(|| DEFAULT_ANTHROPIC_URL.into()),
            }),
            Provider::OpenAi => Some(Settings { provider, api_key, model: var("LLM_MODEL")?, base_url: var("LLM_BASE_URL")? }),
        }
    }

    pub fn client(&self) -> Arc<dyn Model> {
        match self.provider {
            Provider::Anthropic => Arc::new(Anthropic::new(self.clone())),
            Provider::OpenAi => Arc::new(OpenAiCompatible::new(self.clone())),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Text(String),
    Image { media_type: String, data_base64: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
    /// Any other block (thinking, citations-bearing text, server blocks): sent back exactly as received.
    Opaque(Value),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Message {
        Message { role: Role::User, content: vec![Block::Text(text.into())] }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stop {
    EndTurn,
    ToolUse,
    MaxTokens,
    /// The policy category from `stop_details`, when the API gives one.
    Refusal(Option<String>),
    PauseTurn,
    Other(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub content: Vec<Block>,
    pub stop: Stop,
    pub usage: Usage,
}

impl Reply {
    /// The visible text of the reply, including text blocks kept opaque because they carry extra fields.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for b in &self.content {
            match b {
                Block::Text(t) => out.push_str(t),
                Block::Opaque(v) if v["type"] == "text" => out.push_str(v["text"].as_str().unwrap_or("")),
                _ => {}
            }
        }
        out
    }

    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.content.iter().filter_map(|b| match b {
            Block::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }

    /// The assistant message to append to the history: the content unchanged.
    pub fn into_message(self) -> Message {
        Message { role: Role::Assistant, content: self.content }
    }
}

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// Receives visible text as it streams in.
pub type TextSink = Arc<dyn Fn(&str) + Send + Sync>;

pub trait Model: Send + Sync {
    fn send<'a>(&'a self, system: &'a str, tools: &'a [ToolDef], messages: &'a [Message]) -> BoxFut<'a, Result<Reply, String>>;

    /// Like `send`, reporting visible text as it arrives. Providers that cannot stream deliver it all at the end.
    fn stream<'a>(&'a self, system: &'a str, tools: &'a [ToolDef], messages: &'a [Message], on_text: TextSink) -> BoxFut<'a, Result<Reply, String>> {
        Box::pin(async move {
            let reply = self.send(system, tools, messages).await?;
            let text = reply.text();
            if !text.is_empty() {
                on_text(&text);
            }
            Ok(reply)
        })
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build().expect("http client")
}

/// Sends with up to two retries on 429, 5xx and connection errors, honouring `retry-after` (capped at 30 s).
async fn send_with_retries(build: impl Fn() -> reqwest::RequestBuilder) -> Result<reqwest::Response, String> {
    let mut attempt = 0;
    loop {
        let wait = match build().send().await {
            Ok(r) if r.status().is_success() => return Ok(r),
            Ok(r) => {
                let status = r.status();
                let retryable = status.as_u16() == 429 || status.is_server_error();
                let after = r.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
                let body = r.text().await.unwrap_or_default();
                if !retryable || attempt >= MAX_RETRIES {
                    return Err(format!("模型接口返回 {}：{}", status.as_u16(), error_message(&body)));
                }
                after.map(Duration::from_secs).unwrap_or(Duration::from_secs(1 << attempt))
            }
            Err(e) if attempt < MAX_RETRIES && e.is_connect() => Duration::from_secs(1 << attempt),
            Err(e) => return Err(format!("连不上模型接口：{e}")),
        };
        tokio::time::sleep(wait.min(MAX_RETRY_WAIT)).await;
        attempt += 1;
    }
}

fn error_message(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    v["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| body.chars().take(300).collect())
}

// ---------- Anthropic ----------

pub struct Anthropic {
    settings: Settings,
    http: reqwest::Client,
}

impl Anthropic {
    pub fn new(settings: Settings) -> Anthropic {
        Anthropic { settings, http: http() }
    }

    pub fn body(&self, system: &str, tools: &[ToolDef], messages: &[Message], stream: bool) -> Value {
        let mut body = json!({
            "model": self.settings.model,
            "max_tokens": MAX_TOKENS,
            "output_config": {"effort": "medium"},
            "fallbacks": "default",
            "messages": messages.iter().map(anthropic_message).collect::<Vec<_>>(),
        });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if !tools.is_empty() {
            let mut list: Vec<Value> =
                tools.iter().map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema})).collect();
            if let Some(last) = list.last_mut() {
                last["cache_control"] = json!({"type": "ephemeral"});
            }
            body["tools"] = Value::Array(list);
        }
        if stream {
            body["stream"] = json!(true);
        }
        body
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response, String> {
        let url = format!("{}/v1/messages", self.settings.base_url.trim_end_matches('/'));
        let key = self.settings.api_key.clone().unwrap_or_default();
        send_with_retries(|| {
            self.http
                .post(&url)
                .header("x-api-key", &key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("anthropic-beta", FALLBACK_BETA)
                .header("content-type", "application/json")
                .json(body)
        })
        .await
    }
}

fn anthropic_message(m: &Message) -> Value {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let content: Vec<Value> = m
        .content
        .iter()
        .map(|b| match b {
            Block::Text(t) => json!({"type": "text", "text": t}),
            Block::Image { media_type, data_base64 } => {
                json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": data_base64}})
            }
            Block::ToolUse { id, name, input } => json!({"type": "tool_use", "id": id, "name": name, "input": input}),
            Block::ToolResult { tool_use_id, content, is_error } => {
                json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": content, "is_error": is_error})
            }
            Block::Opaque(v) => v.clone(),
        })
        .collect();
    json!({"role": role, "content": content})
}

fn has_only(o: &Map<String, Value>, keys: &[&str]) -> bool {
    o.keys().all(|k| keys.contains(&k.as_str()))
}

/// Text and tool_use become typed blocks only when they carry nothing else; everything else stays opaque so the
/// next request sends it back byte-for-byte equivalent.
fn anthropic_block(v: Value) -> Block {
    if let Some(o) = v.as_object() {
        match o.get("type").and_then(Value::as_str) {
            Some("text") if has_only(o, &["type", "text"]) => return Block::Text(o["text"].as_str().unwrap_or("").into()),
            Some("tool_use") if has_only(o, &["type", "id", "name", "input"]) => {
                return Block::ToolUse {
                    id: o["id"].as_str().unwrap_or("").into(),
                    name: o["name"].as_str().unwrap_or("").into(),
                    input: o["input"].clone(),
                }
            }
            _ => {}
        }
    }
    Block::Opaque(v)
}

fn anthropic_stop(reason: &str, details: &Value) -> Stop {
    match reason {
        "end_turn" | "stop_sequence" => Stop::EndTurn,
        "tool_use" => Stop::ToolUse,
        "max_tokens" => Stop::MaxTokens,
        "refusal" => Stop::Refusal(details["category"].as_str().map(str::to_string)),
        "pause_turn" => Stop::PauseTurn,
        other => Stop::Other(other.to_string()),
    }
}

fn anthropic_usage(u: &Value) -> Usage {
    Usage { input_tokens: u["input_tokens"].as_u64().unwrap_or(0), output_tokens: u["output_tokens"].as_u64().unwrap_or(0) }
}

pub fn parse_anthropic(v: Value) -> Reply {
    let content = v["content"].as_array().cloned().unwrap_or_default().into_iter().map(anthropic_block).collect();
    Reply {
        content,
        stop: anthropic_stop(v["stop_reason"].as_str().unwrap_or(""), &v["stop_details"]),
        usage: anthropic_usage(&v["usage"]),
    }
}

/// Rebuilds the final message from a Messages API event stream, forwarding text deltas.
#[derive(Default)]
struct StreamState {
    blocks: Vec<Value>,
    partial_json: Vec<String>,
    stop_reason: String,
    stop_details: Value,
    usage: Usage,
}

impl StreamState {
    fn event(&mut self, ev: &Value, on_text: &TextSink) -> Result<(), String> {
        match ev["type"].as_str().unwrap_or("") {
            "message_start" => {
                let u = &ev["message"]["usage"];
                self.usage.input_tokens = u["input_tokens"].as_u64().unwrap_or(0);
            }
            "content_block_start" => {
                let i = ev["index"].as_u64().unwrap_or(0) as usize;
                while self.blocks.len() <= i {
                    self.blocks.push(Value::Null);
                    self.partial_json.push(String::new());
                }
                self.blocks[i] = ev["content_block"].clone();
            }
            "content_block_delta" => {
                let i = ev["index"].as_u64().unwrap_or(0) as usize;
                let d = &ev["delta"];
                let block = self.blocks.get_mut(i).ok_or("模型流里出现了未开始的内容块")?;
                let append = |block: &mut Value, field: &str, piece: &str| {
                    let cur = block[field].as_str().unwrap_or("").to_string();
                    block[field] = json!(cur + piece);
                };
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or("");
                        append(block, "text", t);
                        on_text(t);
                    }
                    "input_json_delta" => self.partial_json[i].push_str(d["partial_json"].as_str().unwrap_or("")),
                    "thinking_delta" => append(block, "thinking", d["thinking"].as_str().unwrap_or("")),
                    "signature_delta" => block["signature"] = d["signature"].clone(),
                    "citations_delta" => {
                        if !block["citations"].is_array() {
                            block["citations"] = json!([]);
                        }
                        block["citations"].as_array_mut().expect("array").push(d["citation"].clone());
                    }
                    other => return Err(format!("模型流里有不认识的增量类型：{other}")),
                }
            }
            "content_block_stop" => {
                let i = ev["index"].as_u64().unwrap_or(0) as usize;
                if let Some(raw) = self.partial_json.get(i).filter(|s| !s.is_empty()) {
                    let input: Value = serde_json::from_str(raw).map_err(|e| format!("模型给的工具参数不是合法 JSON：{e}"))?;
                    self.blocks[i]["input"] = input;
                }
            }
            "message_delta" => {
                if let Some(r) = ev["delta"]["stop_reason"].as_str() {
                    self.stop_reason = r.to_string();
                }
                if !ev["delta"]["stop_details"].is_null() {
                    self.stop_details = ev["delta"]["stop_details"].clone();
                }
                if let Some(n) = ev["usage"]["output_tokens"].as_u64() {
                    self.usage.output_tokens = n;
                }
            }
            "error" => return Err(format!("模型接口出错：{}", ev["error"]["message"].as_str().unwrap_or("未知错误"))),
            _ => {} // message_stop, ping
        }
        Ok(())
    }

    fn finish(self) -> Reply {
        Reply {
            content: self.blocks.into_iter().filter(|b| !b.is_null()).map(anthropic_block).collect(),
            stop: anthropic_stop(&self.stop_reason, &self.stop_details),
            usage: self.usage,
        }
    }
}

impl Model for Anthropic {
    fn send<'a>(&'a self, system: &'a str, tools: &'a [ToolDef], messages: &'a [Message]) -> BoxFut<'a, Result<Reply, String>> {
        Box::pin(async move {
            let resp = self.post(&self.body(system, tools, messages, false)).await?;
            let v: Value = resp.json().await.map_err(|e| format!("模型接口的回复读不懂：{e}"))?;
            Ok(parse_anthropic(v))
        })
    }

    fn stream<'a>(&'a self, system: &'a str, tools: &'a [ToolDef], messages: &'a [Message], on_text: TextSink) -> BoxFut<'a, Result<Reply, String>> {
        Box::pin(async move {
            let mut resp = self.post(&self.body(system, tools, messages, true)).await?;
            let mut state = StreamState::default();
            let mut buf: Vec<u8> = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|e| format!("模型流中断了：{e}"))? {
                buf.extend_from_slice(&chunk);
                while let Some(end) = find_event_end(&buf) {
                    let raw: Vec<u8> = buf.drain(..end).collect();
                    let text = String::from_utf8_lossy(&raw);
                    for line in text.lines() {
                        if let Some(data) = line.strip_prefix("data:") {
                            let ev: Value = serde_json::from_str(data.trim()).map_err(|e| format!("模型流里有读不懂的事件：{e}"))?;
                            state.event(&ev, &on_text)?;
                        }
                    }
                }
            }
            Ok(state.finish())
        })
    }
}

/// End of the next complete SSE event (including its blank-line separator), if the buffer holds one.
fn find_event_end(buf: &[u8]) -> Option<usize> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|p| p + 2);
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

// ---------- OpenAI-compatible ----------

pub struct OpenAiCompatible {
    settings: Settings,
    http: reqwest::Client,
}

impl OpenAiCompatible {
    pub fn new(settings: Settings) -> OpenAiCompatible {
        OpenAiCompatible { settings, http: http() }
    }

    pub fn body(&self, system: &str, tools: &[ToolDef], messages: &[Message]) -> Value {
        let mut out = Vec::new();
        if !system.is_empty() {
            out.push(json!({"role": "system", "content": system}));
        }
        for m in messages {
            openai_messages(m, &mut out);
        }
        let mut body = json!({"model": self.settings.model, "max_tokens": MAX_TOKENS, "messages": out});
        if !tools.is_empty() {
            body["tools"] = tools
                .iter()
                .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.input_schema}}))
                .collect();
        }
        body
    }
}

fn openai_messages(m: &Message, out: &mut Vec<Value>) {
    match m.role {
        Role::User => {
            let mut parts = Vec::new();
            for b in &m.content {
                match b {
                    Block::Text(t) => parts.push(json!({"type": "text", "text": t})),
                    Block::Image { media_type, data_base64 } => {
                        parts.push(json!({"type": "image_url", "image_url": {"url": format!("data:{media_type};base64,{data_base64}")}}))
                    }
                    Block::ToolResult { tool_use_id, content, .. } => {
                        out.push(json!({"role": "tool", "tool_call_id": tool_use_id, "content": content}))
                    }
                    Block::ToolUse { .. } | Block::Opaque(_) => {}
                }
            }
            if parts.len() == 1 && parts[0]["type"] == "text" {
                out.push(json!({"role": "user", "content": parts[0]["text"]}));
            } else if !parts.is_empty() {
                out.push(json!({"role": "user", "content": parts}));
            }
        }
        Role::Assistant => {
            let reply = Reply { content: m.content.clone(), stop: Stop::EndTurn, usage: Usage::default() };
            let text = reply.text();
            let calls: Vec<Value> = reply
                .tool_uses()
                .map(|(id, name, input)| json!({"id": id, "type": "function", "function": {"name": name, "arguments": input.to_string()}}))
                .collect();
            let mut msg = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
            if !calls.is_empty() {
                msg["tool_calls"] = Value::Array(calls);
            }
            out.push(msg);
        }
    }
}

pub fn parse_openai(v: Value) -> Result<Reply, String> {
    let choice = &v["choices"][0];
    if choice.is_null() {
        return Err(format!("模型接口的回复里没有内容：{}", error_message(&v.to_string())));
    }
    let msg = &choice["message"];
    let mut content = Vec::new();
    if let Some(t) = msg["content"].as_str().filter(|t| !t.is_empty()) {
        content.push(Block::Text(t.to_string()));
    }
    for call in msg["tool_calls"].as_array().into_iter().flatten() {
        let args = call["function"]["arguments"].as_str().unwrap_or("{}");
        let input = serde_json::from_str(args).unwrap_or_else(|_| json!({"_raw": args}));
        content.push(Block::ToolUse {
            id: call["id"].as_str().unwrap_or("").into(),
            name: call["function"]["name"].as_str().unwrap_or("").into(),
            input,
        });
    }
    let stop = match choice["finish_reason"].as_str().unwrap_or("") {
        "stop" => Stop::EndTurn,
        "tool_calls" | "function_call" => Stop::ToolUse,
        "length" => Stop::MaxTokens,
        "content_filter" => Stop::Refusal(None),
        other => Stop::Other(other.to_string()),
    };
    let u = &v["usage"];
    Ok(Reply {
        content,
        stop,
        usage: Usage { input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0), output_tokens: u["completion_tokens"].as_u64().unwrap_or(0) },
    })
}

impl Model for OpenAiCompatible {
    fn send<'a>(&'a self, system: &'a str, tools: &'a [ToolDef], messages: &'a [Message]) -> BoxFut<'a, Result<Reply, String>> {
        Box::pin(async move {
            let url = format!("{}/chat/completions", self.settings.base_url.trim_end_matches('/'));
            let body = self.body(system, tools, messages);
            let resp = send_with_retries(|| {
                let req = self.http.post(&url).header("content-type", "application/json").json(&body);
                match &self.settings.api_key {
                    Some(k) => req.bearer_auth(k),
                    None => req,
                }
            })
            .await?;
            let v: Value = resp.json().await.map_err(|e| format!("模型接口的回复读不懂：{e}"))?;
            parse_openai(v)
        })
    }
}

// ---------- Providers and model discovery ----------

/// A known service. Everything except Anthropic speaks the OpenAI-compatible chat API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub provider: Provider,
    pub base_url: &'static str,
    pub needs_key: bool,
}

const fn openai(id: &'static str, name: &'static str, base_url: &'static str, needs_key: bool) -> Preset {
    Preset { id, name, provider: Provider::OpenAi, base_url, needs_key }
}

pub const PRESETS: &[Preset] = &[
    Preset { id: "anthropic", name: "Anthropic Claude", provider: Provider::Anthropic, base_url: DEFAULT_ANTHROPIC_URL, needs_key: true },
    openai("openai", "OpenAI", "https://api.openai.com/v1", true),
    openai("gemini", "Google Gemini", "https://generativelanguage.googleapis.com/v1beta/openai", true),
    openai("deepseek", "DeepSeek 深度求索", "https://api.deepseek.com/v1", true),
    openai("qwen", "通义千问（阿里云百炼）", "https://dashscope.aliyuncs.com/compatible-mode/v1", true),
    openai("moonshot", "Kimi（月之暗面）", "https://api.moonshot.cn/v1", true),
    openai("zhipu", "智谱 GLM", "https://open.bigmodel.cn/api/paas/v4", true),
    openai("doubao", "豆包（火山方舟）", "https://ark.cn-beijing.volces.com/api/v3", true),
    openai("siliconflow", "硅基流动", "https://api.siliconflow.cn/v1", true),
    openai("mistral", "Mistral", "https://api.mistral.ai/v1", true),
    openai("xai", "xAI Grok", "https://api.x.ai/v1", true),
    openai("groq", "Groq", "https://api.groq.com/openai/v1", true),
    openai("openrouter", "OpenRouter（几百个模型）", "https://openrouter.ai/api/v1", true),
    openai("ollama", "Ollama（本机）", "http://localhost:11434/v1", false),
    openai("lmstudio", "LM Studio（本机）", "http://localhost:1234/v1", false),
    openai("custom", "其他兼容 OpenAI 的接口", "", false),
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    /// What the service calls it, when it says.
    pub name: String,
}

/// Models that cannot chat (embeddings, speech, images, moderation) are left out of the list.
fn chat_capable(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    !["embed", "whisper", "tts", "dall-e", "moderation", "transcribe", "rerank", "audio"].iter().any(|w| id.contains(w))
}

/// Asks the service which models this key can use. Needs the base URL and, for most services, the key;
/// the model in `settings` is ignored.
pub async fn list_models(settings: &Settings) -> Result<Vec<ModelInfo>, String> {
    let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
    let base = settings.base_url.trim_end_matches('/');
    if base.is_empty() {
        return Err("请先填接口地址。".into());
    }
    let key = settings.api_key.clone().unwrap_or_default();
    let mut out = vec![];
    match settings.provider {
        Provider::Anthropic => {
            let mut after: Option<String> = None;
            loop {
                let mut url = format!("{base}/v1/models?limit=100");
                if let Some(a) = &after {
                    url.push_str(&format!("&after_id={a}"));
                }
                let resp = send_with_retries(|| http.get(&url).header("x-api-key", &key).header("anthropic-version", ANTHROPIC_VERSION)).await?;
                let v: Value = resp.json().await.map_err(|e| format!("模型列表读不懂：{e}"))?;
                for m in v["data"].as_array().into_iter().flatten() {
                    let id = m["id"].as_str().unwrap_or("").to_string();
                    out.push(ModelInfo { name: m["display_name"].as_str().unwrap_or(&id).to_string(), id });
                }
                match (v["has_more"].as_bool(), v["last_id"].as_str()) {
                    (Some(true), Some(last)) if out.len() < 1000 => after = Some(last.to_string()),
                    _ => break,
                }
            }
        }
        Provider::OpenAi => {
            let url = format!("{base}/models");
            let resp = send_with_retries(|| {
                let req = http.get(&url);
                if key.is_empty() { req } else { req.bearer_auth(&key) }
            })
            .await?;
            let v: Value = resp.json().await.map_err(|e| format!("模型列表读不懂：{e}"))?;
            for m in v["data"].as_array().into_iter().flatten() {
                let id = m["id"].as_str().unwrap_or("").to_string();
                if !id.is_empty() && chat_capable(&id) {
                    out.push(ModelInfo { name: m["name"].as_str().unwrap_or(&id).to_string(), id });
                }
            }
            out.sort_by(|a, b| a.id.cmp(&b.id));
        }
    }
    if out.is_empty() {
        return Err("接口没有返回可用的模型：请检查密钥，或者直接填模型名。".into());
    }
    Ok(out)
}

/// The model picked when the admin has not chosen one: Claude's default if offered, else the first listed.
pub fn default_model(provider: Provider, models: &[ModelInfo]) -> Option<String> {
    if provider == Provider::Anthropic && models.iter().any(|m| m.id == DEFAULT_MODEL) {
        return Some(DEFAULT_MODEL.into());
    }
    models.first().map(|m| m.id.clone())
}

// ---------- Settings saved from the admin ----------

fn ensure_table(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS llm_settings (id INTEGER PRIMARY KEY CHECK (id = 1), preset TEXT NOT NULL, base_url TEXT NOT NULL,
         model TEXT NOT NULL, api_key TEXT, updated_at TEXT NOT NULL DEFAULT (datetime('now')))",
        [],
    )
    .map(|_| ())
    .map_err(|e| format!("准备模型设置表失败：{e}"))
}

/// What the admin saved, if anything: (preset id, settings).
pub fn load_saved(conn: &rusqlite::Connection) -> Result<Option<(String, Settings)>, String> {
    ensure_table(conn)?;
    use rusqlite::OptionalExtension;
    conn.query_row("SELECT preset, base_url, model, api_key FROM llm_settings WHERE id = 1", [], |r| {
        let preset_id: String = r.get(0)?;
        let provider = preset(&preset_id).map(|p| p.provider).unwrap_or(Provider::OpenAi);
        Ok((preset_id, Settings { provider, base_url: r.get(1)?, model: r.get(2)?, api_key: r.get(3)? }))
    })
    .optional()
    .map_err(|e| format!("读取模型设置失败：{e}"))
}

/// Saves the admin's choice. `api_key: None` keeps the key already saved (the form never shows it back).
pub fn save(conn: &rusqlite::Connection, preset_id: &str, base_url: &str, model: &str, api_key: Option<&str>) -> Result<Settings, String> {
    let p = preset(preset_id).ok_or("不认识这个模型服务。")?;
    let base_url = if base_url.trim().is_empty() { p.base_url } else { base_url.trim() };
    if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
        return Err("接口地址要以 https:// 或 http:// 开头。".into());
    }
    if model.trim().is_empty() {
        return Err("请选一个模型，或者直接填模型名。".into());
    }
    ensure_table(conn)?;
    let key = match api_key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) => Some(k.to_string()),
        None => load_saved(conn)?.and_then(|(_, s)| s.api_key),
    };
    if p.needs_key && key.is_none() {
        return Err(format!("{} 需要密钥。", p.name));
    }
    conn.execute(
        "INSERT INTO llm_settings (id, preset, base_url, model, api_key) VALUES (1, ?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET preset = ?1, base_url = ?2, model = ?3, api_key = ?4, updated_at = datetime('now')",
        rusqlite::params![preset_id, base_url.trim_end_matches('/'), model.trim(), key],
    )
    .map_err(|e| format!("保存模型设置失败：{e}"))?;
    Ok(Settings { provider: p.provider, base_url: base_url.trim_end_matches('/').to_string(), model: model.trim().to_string(), api_key: key })
}

/// Environment settings win (the server operator's choice); otherwise what the admin saved.
pub fn effective(conn: &rusqlite::Connection) -> Option<Settings> {
    Settings::from_env().or_else(|| load_saved(conn).ok().flatten().map(|(_, s)| s))
}
