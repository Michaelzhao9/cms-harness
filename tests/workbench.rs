mod common;

use axum::http::{header, HeaderMap, Request, StatusCode};
use common::*;
use cms_harness::chat;
use cms_harness::llm::{self, Block, BoxFut, Message, Model, Reply, Stop, ToolDef, Usage};
use cms_harness::Shared;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tower::ServiceExt;

/// A model that answers from a script, remembers what it was sent and, with a gate, waits before each answer.
#[derive(Default)]
struct Scripted {
    replies: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<(String, Vec<Message>)>>,
    gate: Option<Arc<Notify>>,
}

impl Model for Scripted {
    fn send<'a>(&'a self, system: &'a str, _tools: &'a [ToolDef], messages: &'a [Message]) -> BoxFut<'a, Result<Reply, String>> {
        Box::pin(async move {
            self.seen.lock().unwrap().push((system.to_string(), messages.to_vec()));
            if let Some(g) = &self.gate {
                g.notified().await;
            }
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

fn with_model(app: &Shared, replies: Vec<Reply>, gate: Option<Arc<Notify>>) -> Arc<Scripted> {
    let m = Arc::new(Scripted { replies: Mutex::new(replies.into()), gate, ..Default::default() });
    app.set_model(m.clone());
    m
}

fn token_hash(app: &Shared, cookie: &str) -> String {
    let mut h = HeaderMap::new();
    h.insert(header::COOKIE, cookie.parse().unwrap());
    cms_harness::web::session::from_cookie(&app.conn(), &h).unwrap().token_hash
}

async fn idle(app: &Shared, token: &str) -> chat::Conversation {
    for _ in 0..500 {
        let c = app.chat.get(token);
        if !c.running {
            return c;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the turn did not finish");
}

/// Every `<script` on an admin page is the one external `admin.js`, with nothing inside; no inline styles.
fn assert_clean(page: &str) {
    for (i, _) in page.match_indices("<script") {
        let rest = &page[i..];
        assert!(rest.starts_with("<script type=\"module\" src=\"/admin/static/admin.js?v="), "unexpected script: {}", &rest[..rest.len().min(120)]);
        let end = rest.find('>').unwrap();
        assert!(rest[end + 1..].starts_with("</script>"), "script with content: {}", &rest[..rest.len().min(120)]);
    }
    for bad in ["<style", "style=", " onclick=", "javascript:"] {
        assert!(!page.contains(bad), "page contains {bad}: {page}");
    }
}

fn seed(app: &Shared) -> i64 {
    news(app);
    let r = ok(app, &builder(), "create_record", json!({"collection": "news", "data": {"title": "旧标题"}, "note": "种子"}));
    publish_all(app);
    r["id"].as_i64().unwrap()
}

/// A multipart POST with text fields and files `(input, filename, bytes)`.
async fn post_multipart(app: &Shared, path: &str, s: &(String, String), fields: &[(&str, &str)], files: &[(&str, &str, &[u8])]) -> (StatusCode, HeaderMap, String) {
    let boundary = "hcboundary7MA4YWxkTrZu0gW";
    let mut body: Vec<u8> = vec![];
    let csrf = [("csrf", s.1.as_str())];
    for (k, v) in fields.iter().chain(csrf.iter()) {
        write!(body, "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").unwrap();
    }
    for (k, name, bytes) in files {
        write!(body, "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").unwrap();
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    write!(body, "--{boundary}--\r\n").unwrap();
    let req = Request::post(path)
        .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
        .header(header::COOKIE, s.0.as_str())
        .body(axum::body::Body::from(body))
        .unwrap();
    let resp = cms_harness::api::router(app.clone()).oneshot(req).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}

/// One answer of the event stream: `(event id, view)`.
async fn event(app: &Shared, cookie: &str, last: Option<&str>) -> (String, Value) {
    let mut req = Request::get("/admin/chat/events").header(header::COOKIE, cookie);
    if let Some(l) = last {
        req = req.header("last-event-id", l);
    }
    let resp = cms_harness::api::router(app.clone()).oneshot(req.body(axum::body::Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "text/event-stream");
    assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
    let text = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    let id = text.lines().find_map(|l| l.strip_prefix("id: ")).unwrap_or_else(|| panic!("no id: {text}")).to_string();
    let data = text.lines().find_map(|l| l.strip_prefix("data: ")).unwrap_or_else(|| panic!("no data: {text}"));
    assert!(text.starts_with("retry: "), "{text}");
    (id, serde_json::from_str(data).unwrap())
}

/// Keeps opening a gated model until the test ends.
fn open_gate(gate: Arc<Notify>) {
    tokio::spawn(async move {
        for _ in 0..400 {
            gate.notify_one();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
}

#[tokio::test]
async fn without_a_model_the_page_says_how_to_configure_one_and_the_rest_works() {
    let app = app();
    seed(&app);
    let s = admin_session(&app, "builder").await;
    let (status, h, page) = get_with(&app, "/admin", Some(&s.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&page);
    assert!(page.contains("还没有配置对话用的模型") && page.contains("href=\"/admin/settings#s-model\"") && page.contains("LLM_API_KEY"), "{page}");
    assert!(h[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("script-src 'self'"));
    // The preview and the rest of the admin are there.
    assert!(page.contains("sandbox=\"allow-scripts allow-forms allow-popups\"") && page.contains("src=\"/?preview=1\""), "{page}");
    for path in ["/admin/c/news", "/admin/settings", "/admin/publish"] {
        let (status, _, page) = get_with(&app, path, Some(&s.0)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_clean(&page);
    }
    let (status, _, body) = post_form(&app, "/admin/chat", Some(&s), &[("text", "改个标题")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("还没有配置对话用的模型"), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_posted_message_runs_a_turn_and_the_event_stream_sends_steps_and_changes() {
    let app = app();
    let id = seed(&app);
    let gate = Arc::new(Notify::new());
    with_model(&app, vec![tool("t1", "update_record", json!({"id": id, "data": {"title": "新标题"}, "note": "对话改标题"})), done("改好了，标题是草稿。")], Some(gate.clone()));
    let s = admin_session(&app, "builder").await;
    let token = token_hash(&app, &s.0);
    let (status, h, _) = post_multipart(&app, "/admin/chat", &s, &[("text", "把标题改成新标题"), ("ctx", "page:/")], &[]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(h[header::LOCATION], "/admin");
    // While the model works: the page refreshes itself without the script, and the input waits.
    let (_, _, page) = get_with(&app, "/admin", Some(&s.0)).await;
    assert_clean(&page);
    assert!(page.contains("<noscript><meta http-equiv=\"refresh\" content=\"2\"></noscript>"), "{page}");
    assert!(page.contains("正在处理…") && page.contains("把标题改成新标题"), "{page}");
    // Other pages carry the chat too, but never the refresh (it would wipe a half-filled form).
    let (_, _, other) = get_with(&app, "/admin/settings", Some(&s.0)).await;
    assert!(!other.contains("http-equiv=\"refresh\"") && other.contains("把标题改成新标题"), "{other}");
    let (first, view) = event(&app, &s.0, None).await;
    assert_eq!(view["running"], true, "{view}");
    // The stream answers when the conversation moves; follow it to the end of the turn.
    open_gate(gate);
    let mut last = first;
    let mut view = view;
    for _ in 0..50 {
        if view["running"] == false {
            break;
        }
        (last, view) = event(&app, &s.0, Some(&last)).await;
    }
    assert_eq!(view["running"], false, "{view}");
    let html = view["html"].as_str().unwrap();
    assert!(html.contains(&format!("修改记录 #{id}")) && html.contains("新标题") && html.contains("旧标题") && html.contains("改好了，标题是草稿。"), "{html}");
    // Change number 1 is the record's place in the pending list; it links to the record.
    assert!(html.contains("<span class=\"pin\">1</span>") && html.contains(&format!("href=\"/admin/r/{id}\"")), "{html}");
    assert_eq!(view["pending"], 1);
    assert_eq!(view["summary"]["changes"], 1, "{view}");
    assert!(view["error"].is_null());
    assert_eq!(idle(&app, &token).await.changes.len(), 1);
    // The finished page: no refresh, the check line and the change are there, and the publish button counts it.
    let (_, _, page) = get_with(&app, "/admin", Some(&s.0)).await;
    assert!(!page.contains("http-equiv=\"refresh\"") && page.contains("class=\"ok\"") && page.contains(&format!("修改记录 #{id}")), "{page}");
    assert!(page.contains("<span class=\"count\">1</span>"), "{page}");
    // An answer that has nothing new waits, then says so with a comment line and no event.
    let req = Request::get("/admin/chat/events?wait=1").header(header::COOKIE, s.0.as_str()).header("last-event-id", last.as_str());
    let resp = cms_harness::api::router(app.clone()).oneshot(req.body(axum::body::Body::empty()).unwrap()).await.unwrap();
    let text = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(text.starts_with("retry: ") && text.contains("\n: ") && !text.contains("data: "), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plus_button_and_logout_start_over() {
    let app = app();
    with_model(&app, vec![done("你好"), done("再见")], None);
    let s = admin_session(&app, "builder").await;
    let token = token_hash(&app, &s.0);
    assert_eq!(post_form(&app, "/admin/chat", Some(&s), &[("text", "第一句")]).await.0, StatusCode::SEE_OTHER);
    idle(&app, &token).await;
    let (_, _, page) = get_with(&app, "/admin", Some(&s.0)).await;
    assert!(page.contains("第一句") && page.contains("你好"), "{page}");
    let (status, h, _) = post_form_with(&app, "/admin/chat/new", Some(&s), &[], &[("referer", "http://127.0.0.1:4200/admin/settings")]).await;
    assert_eq!((status, h[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/admin/settings"));
    assert!(app.chat.get(&token).messages.is_empty());
    let (_, _, page) = get_with(&app, "/admin", Some(&s.0)).await;
    assert!(!page.contains("第一句") && page.contains("class=\"tip\""), "{page}");
    // Logout clears it too.
    assert_eq!(post_form(&app, "/admin/chat", Some(&s), &[("text", "第二句")]).await.0, StatusCode::SEE_OTHER);
    idle(&app, &token).await;
    assert_eq!(post_form(&app, "/admin/logout", Some(&s), &[]).await.0, StatusCode::SEE_OTHER);
    assert!(app.chat.get(&token).messages.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_user_options_are_buttons_that_answer() {
    let app = app();
    let m = with_model(&app, vec![tool("q1", "ask_user", json!({"question": "删掉哪一条？", "options": ["第一条", "第二条"]})), done("好的")], None);
    let s = admin_session(&app, "builder").await;
    let token = token_hash(&app, &s.0);
    post_form(&app, "/admin/chat", Some(&s), &[("text", "删一条新闻")]).await;
    idle(&app, &token).await;
    let (_, _, page) = get_with(&app, "/admin", Some(&s.0)).await;
    assert_clean(&page);
    assert!(page.contains("删掉哪一条？"), "{page}");
    assert!(page.contains("<button type=\"submit\" class=\"opt\" name=\"text\" value=\"第二条\">第二条</button>"), "{page}");
    assert_eq!(post_form(&app, "/admin/chat", Some(&s), &[("text", "第二条")]).await.0, StatusCode::SEE_OTHER);
    let c = idle(&app, &token).await;
    assert!(c.question.is_none() && c.error.is_none());
    let seen = m.seen.lock().unwrap();
    let last = seen.last().unwrap().1.last().unwrap();
    assert!(last.content.iter().any(|b| matches!(b, Block::ToolResult { tool_use_id, content, .. } if tool_use_id == "q1" && content.contains("第二条"))), "{last:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_ends_a_running_turn() {
    let app = app();
    let gate = Arc::new(Notify::new());
    let m = with_model(&app, vec![done("不会到这里")], Some(gate));
    let s = admin_session(&app, "builder").await;
    let token = token_hash(&app, &s.0);
    post_form(&app, "/admin/chat", Some(&s), &[("text", "做点慢事")]).await;
    for _ in 0..200 {
        if !m.seen.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (_, _, page) = get_with(&app, "/admin", Some(&s.0)).await;
    assert!(page.contains("formaction=\"/admin/chat/stop\""), "{page}");
    let (status, _, _) = post_form(&app, "/admin/chat/stop", Some(&s), &[]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let c = idle(&app, &token).await;
    assert!(c.stopped);
    let (_, _, page) = get_with(&app, "/admin", Some(&s.0)).await;
    assert!(page.contains("已停下") && !page.contains("http-equiv=\"refresh\""), "{page}");
    // Stopping needs the form token like every other post.
    assert_eq!(post_form(&app, "/admin/chat/stop", Some(&(s.0.clone(), "bad".into())), &[]).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn model_settings_save_without_echoing_the_key_and_only_builders_may() {
    let app = app();
    let b = admin_session(&app, "builder").await;
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&b.0)).await;
    assert_clean(&page);
    for p in llm::PRESETS {
        assert!(page.contains(&format!("<option value=\"{}\"", p.id)), "{}", p.id);
    }
    let fields = [("preset", "openai"), ("base_url", ""), ("model", ""), ("model_name", "gpt-test-1"), ("key", "sk-very-secret-123")];
    let (status, h, body) = post_form(&app, "/admin/settings/model", Some(&b), &fields).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(h[header::LOCATION], "/admin/settings?saved=model#s-model");
    assert!(app.model().is_some(), "the chat uses the saved model at once");
    let (_, saved) = llm::load_saved(&app.conn()).unwrap().unwrap();
    assert_eq!((saved.model.as_str(), saved.api_key.as_deref()), ("gpt-test-1", Some("sk-very-secret-123")));
    let (_, _, page) = get_with(&app, "/admin/settings?saved=model", Some(&b.0)).await;
    assert!(!page.contains("sk-very-secret-123") && page.contains("gpt-test-1") && page.contains("已保存，不显示"), "{page}");
    // Saving again without a key keeps the saved one.
    post_form(&app, "/admin/settings/model", Some(&b), &[("preset", "openai"), ("model", "gpt-test-2")]).await;
    assert_eq!(llm::load_saved(&app.conn()).unwrap().unwrap().1.api_key.as_deref(), Some("sk-very-secret-123"));
    // Detect against a server that is not there: an error, the key never in the page.
    let (status, _, page) = post_form(&app, "/admin/settings/model/detect", Some(&b), &[("preset", "custom"), ("base_url", "http://127.0.0.1:9/v1"), ("key", "sk-typed-456")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!page.contains("sk-typed-456") && !page.contains("sk-very-secret-123") && page.contains("127.0.0.1:9"), "{page}");
    // A bad choice is refused with the page.
    let (status, _, page) = post_form(&app, "/admin/settings/model", Some(&b), &[("preset", "nope"), ("model", "x")]).await;
    assert!(status == StatusCode::BAD_REQUEST && page.contains("不认识这个模型服务"), "{page}");
    // Editors may look but not change.
    let e = admin_session(&app, "editor").await;
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&e.0)).await;
    assert!(!page.contains("action=\"/admin/settings/model\"") && !page.contains("action=\"/admin/settings/skills/install\""), "{page}");
    for path in ["/admin/settings/model", "/admin/settings/model/detect", "/admin/settings/skills/install", "/admin/settings/skills/x/disable", "/admin/settings/skills/x/delete"] {
        assert_eq!(post_form(&app, path, Some(&e), &[("preset", "openai"), ("model", "m"), ("url", "https://example.com/x")]).await.0, StatusCode::FORBIDDEN, "{path}");
    }
    // A model set by the server's environment is shown read-only and cannot be changed here.
    let _ = app.env_model.set(llm::Settings { provider: llm::Provider::Anthropic, api_key: Some("env-key-789".into()), model: "claude-env".into(), base_url: llm::DEFAULT_ANTHROPIC_URL.into() });
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&b.0)).await;
    assert!(page.contains("claude-env") && page.contains("环境变量") && !page.contains("env-key-789") && !page.contains("action=\"/admin/settings/model\""), "{page}");
    assert_eq!(post_form(&app, "/admin/settings/model", Some(&b), &[("preset", "openai"), ("model", "m")]).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(post_form(&app, "/admin/settings/model/detect", Some(&b), &[("preset", "openai")]).await.0, StatusCode::BAD_REQUEST);
}

fn skill_zip() -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let opts = zip::write::SimpleFileOptions::default();
    z.start_file("brand/SKILL.md", opts).unwrap();
    z.write_all("---\nname: brand-voice\ndescription: 用品牌语气写文案\n---\n先读 tone.md。\n".as_bytes()).unwrap();
    z.start_file("brand/tone.md", opts).unwrap();
    z.write_all("短句，不用感叹号。".as_bytes()).unwrap();
    z.finish().unwrap();
    buf.into_inner()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_skill_installed_by_upload_is_listed_and_the_chat_sees_it() {
    let app = app();
    let m = with_model(&app, vec![done("好")], None);
    let s = admin_session(&app, "builder").await;
    let token = token_hash(&app, &s.0);
    let zip = skill_zip();
    let (status, h, body) = post_multipart(&app, "/admin/settings/skills/install", &s, &[("url", "")], &[("zip", "brand.zip", &zip)]).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(h[header::LOCATION], "/admin/settings?saved=skill#s-skills");
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&s.0)).await;
    assert_clean(&page);
    assert!(page.contains("brand-voice") && page.contains("用品牌语气写文案") && page.contains("启用中") && page.contains("上传的压缩包"), "{page}");
    post_form(&app, "/admin/chat", Some(&s), &[("text", "写段文案")]).await;
    idle(&app, &token).await;
    assert!(m.seen.lock().unwrap()[0].0.contains("brand-voice"));
    // The chat itself has no way to install one.
    let actor = cms_harness::web::session::from_cookie(&app.conn(), &{
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, s.0.parse().unwrap());
        h
    })
    .unwrap()
    .actor();
    assert!(!chat::chat_tools(&actor).iter().any(|t| t.name.contains("skill")), "chat tools offer a skill tool");
    // Disable, then delete after a confirmation.
    assert_eq!(post_form(&app, "/admin/settings/skills/brand-voice/disable", Some(&s), &[]).await.0, StatusCode::SEE_OTHER);
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&s.0)).await;
    assert!(page.contains("已停用") && page.contains("action=\"/admin/settings/skills/brand-voice/enable\""), "{page}");
    let (status, _, page) = post_form(&app, "/admin/settings/skills/brand-voice/delete", Some(&s), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&page);
    assert!(page.contains("删除技能“brand-voice”？") && page.contains("name=\"confirm\" value=\"1\""), "{page}");
    assert_eq!(cms_harness::skills::list(&app.conn()).unwrap().len(), 1);
    assert_eq!(post_form(&app, "/admin/settings/skills/brand-voice/delete", Some(&s), &[("confirm", "1")]).await.0, StatusCode::SEE_OTHER);
    assert!(cms_harness::skills::list(&app.conn()).unwrap().is_empty());
    // A broken upload is refused with the page.
    let (status, _, page) = post_multipart(&app, "/admin/settings/skills/install", &s, &[], &[("zip", "x.zip", b"not a zip")]).await;
    assert!(status == StatusCode::BAD_REQUEST && page.contains("压缩包打不开"), "{page}");
}

#[tokio::test]
async fn admin_js_and_the_logo_are_served_and_pages_carry_only_the_external_script() {
    let app = app();
    let (status, h, js) = get_with(&app, "/admin/static/admin.js", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "text/javascript; charset=utf-8");
    assert!(js.len() > 1000);
    cms_harness::site::check_file("admin.js", &js).unwrap();
    let (status, h, svg) = get_with(&app, "/admin/static/logo.svg", None).await;
    assert_eq!((status, h[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "image/svg+xml"));
    assert!(svg.starts_with("<svg"));
    let (_, h, login) = get_with(&app, "/admin/login", None).await;
    assert_eq!(h[header::CONTENT_SECURITY_POLICY], cms_harness::web::ADMIN_CSP);
    assert!(login.contains("rel=\"icon\" href=\"/admin/static/logo.svg\"") && login.contains("class=\"logo\""), "{login}");
    assert_clean(&login);
    let id = seed(&app);
    let s = admin_session(&app, "builder").await;
    for path in ["/admin", "/admin?v=split&w=mobile&marks=0&p=/news", format!("/admin/r/{id}").as_str(), "/admin/media", "/admin/releases"] {
        let (status, _, page) = get_with(&app, path, Some(&s.0)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_clean(&page);
        assert!(page.contains("<script type=\"module\" src=\"/admin/static/admin.js?v="), "{path}");
    }
    let (_, _, split) = get_with(&app, "/admin?v=split&w=mobile&p=/news", Some(&s.0)).await;
    assert!(split.contains("src=\"/news?preview=1\"") && split.contains("src=\"/news\"") && split.contains("w-mobile"), "{split}");
    // A preview path must stay on the site.
    let (_, _, odd) = get_with(&app, "/admin?p=https://evil.example/x", Some(&s.0)).await;
    assert!(odd.contains("src=\"/?preview=1\"") && !odd.contains("evil.example"), "{odd}");
}

#[tokio::test]
async fn the_chat_size_is_a_session_preference_over_the_admin_default() {
    let app = app();
    let s = admin_session(&app, "builder").await;
    let (_, _, page) = get_with(&app, "/admin", Some(&s.0)).await;
    assert!(page.contains("class=\"shell chat-docked"), "{page}");
    for mode in ["mini", "floating", "docked"] {
        let (status, _, _) = post_form(&app, "/admin/prefs", Some(&s), &[("chat", mode)]).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let (_, _, page) = get_with(&app, "/admin", Some(&s.0)).await;
        assert!(page.contains(&format!("class=\"shell chat-{mode}")), "{mode}");
    }
    assert_eq!(post_form(&app, "/admin/prefs", Some(&s), &[("chat", "huge")]).await.0, StatusCode::BAD_REQUEST);
    // The 外观 default applies at the next login.
    post_form(&app, "/admin/prefs", Some(&s), &[("chat", "mini")]).await;
    post_form(&app, "/admin/settings/appearance", Some(&s), &[("chat", "floating")]).await;
    let (cookie, _) = {
        let (status, headers, _) = post_form(&app, "/admin/login", None, &[("email", "builder@test"), ("password", "secret-pass")]).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        (headers[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string(), ())
    };
    let (_, _, page) = get_with(&app, "/admin", Some(&cookie)).await;
    assert!(page.contains("class=\"shell chat-floating"), "{page}");
}
