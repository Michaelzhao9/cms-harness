//! Stage two end to end, with a scripted model standing in for the real one: the chat builds a booking form,
//! the admin publishes it, a visitor books, the admin sees it; then member login brings 用户管理.

mod common;

use axum::http::{header, HeaderMap, StatusCode};
use cms_harness::llm::{Block, BoxFut, Message, Model, Reply, Stop, ToolDef, Usage};
use cms_harness::Shared;
use common::*;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Scripted(Mutex<VecDeque<Reply>>);

impl Model for Scripted {
    fn send<'a>(&'a self, _: &'a str, _: &'a [ToolDef], _: &'a [Message]) -> BoxFut<'a, Result<Reply, String>> {
        Box::pin(async move { self.0.lock().unwrap().pop_front().ok_or_else(|| "script ran out".to_string()) })
    }
}

fn tools(calls: Vec<(&str, Value)>) -> Reply {
    let content = calls.into_iter().enumerate().map(|(i, (n, input))| Block::ToolUse { id: format!("t{i}"), name: n.into(), input }).collect();
    Reply { content, stop: Stop::ToolUse, usage: Usage::default() }
}

fn say(text: &str) -> Reply {
    Reply { content: vec![Block::Text(text.into())], stop: Stop::EndTurn, usage: Usage::default() }
}

async fn chat_turn(app: &Shared, session: &(String, String), text: &str) {
    let (s, _, body) = post_form(app, "/admin/chat", Some(session), &[("text", text)]).await;
    assert!(s.is_redirection() || s.is_success(), "{s} {body}");
    let mut h = HeaderMap::new();
    h.insert(header::COOKIE, session.0.parse().unwrap());
    let token = cms_harness::web::session::from_cookie(&app.conn(), &h).unwrap().token_hash;
    for _ in 0..500 {
        let c = app.chat.get(&token);
        if !c.running {
            assert_eq!(c.error, None, "the turn ended with an error");
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the chat turn did not finish");
}

async fn publish_everything(app: &Shared, session: &(String, String)) {
    let pending = ok(app, &builder(), "list_pending", json!({}));
    let targets: Vec<String> = pending.as_array().unwrap().iter().map(|p| p["target"].as_str().unwrap().to_string()).collect();
    let mut fields: Vec<(&str, &str)> = targets.iter().map(|t| ("target", t.as_str())).collect();
    fields.push(("note", "上线"));
    let (s, h, body) = post_form(app, "/admin/publish", Some(session), &fields).await;
    assert_eq!(s, StatusCode::SEE_OTHER, "{body}");
    assert!(h[header::LOCATION].to_str().unwrap().starts_with("/admin/publish?done="));
    assert!(ok(app, &builder(), "list_pending", json!({})).as_array().unwrap().is_empty());
}

const BOOK_PAGE: &str = r#"{% extends "_hc/layout.html" %}{% block main %}
<h1>预约到店</h1>
{% if request.query.sent %}<p class="thanks">收到了，我们会尽快联系你。</p>{% endif %}
<form method="post" action="/bookings/new">
<input type="hidden" name="next" value="/book">
<label>名字 <input name="name" required></label>
<label>电话 <input name="phone"></label>
<label>日期 <input type="date" name="day"></label>
<button>预约</button>
</form>{% endblock %}"#;

#[tokio::test(flavor = "multi_thread")]
async fn the_chat_builds_a_booking_form_a_visitor_books_and_members_get_a_screen() {
    let app = app();
    let session = admin_session(&app, "builder").await;

    // 1. The chat builds it: a collection visitors may add to, the page, and the admin screen for it.
    app.set_model(Arc::new(Scripted(Mutex::new(VecDeque::from([
        tools(vec![("create_collection", json!({"name": "bookings", "title": "预约", "read": "private", "create": "public", "note": "预约表单",
            "fields": [{"name": "name", "title": "名字", "type": "text", "required": true}, {"name": "phone", "title": "电话", "type": "text"},
                       {"name": "day", "title": "日期", "type": "date", "index": true}]}))]),
        tools(vec![
            ("write_site_file", json!({"path": "book.html", "content": BOOK_PAGE, "note": "预约页"})),
            ("set_view", json!({"name": "booking_log", "title": "预约记录", "collection": "bookings", "columns": ["name", "phone", "day"], "sort": "-day", "note": "后台看预约"})),
        ]),
        say("做好了：网站多了 /book 预约页，左边多了「预约记录」。都是草稿，点右上角「发布」上线。"),
    ])))));
    chat_turn(&app, &session, "加一个预约到店的表单").await;
    let (_, _, admin) = get_with(&app, "/admin", Some(&session.0)).await;
    assert!(admin.contains("预约记录"), "the admin screen arrives with the feature");
    assert_eq!(get_with(&app, "/book", None).await.0, StatusCode::NOT_FOUND, "nothing is live before publishing");

    // 2. The admin publishes from the review page.
    publish_everything(&app, &session).await;

    // 3. A visitor books on the built-in site.
    let (s, _, page) = get_with(&app, "/book", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(page.contains("action=\"/bookings/new\""));
    let (s, h, _) = post_form(&app, "/bookings/new", None, &[("name", "张三"), ("phone", "13800000000"), ("day", "2026-12-06"), ("next", "/book")]).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(h[header::LOCATION], "/book?sent=1");
    assert!(get_with(&app, "/book?sent=1", None).await.2.contains("收到了"));
    assert_eq!(get_with(&app, "/api/v1/content/bookings", None).await.0, StatusCode::UNAUTHORIZED, "bookings stay private");

    // 4. The admin sees it: a badge, then the record.
    let (_, _, admin) = get_with(&app, "/admin", Some(&session.0)).await;
    assert!(admin.contains("1 条新"));
    let (_, _, log) = get_with(&app, "/admin/v/booking_log", Some(&session.0)).await;
    assert!(log.contains("张三") && log.contains("13800000000"));

    // 5. Member login through the chat brings 用户管理; a visitor signs up; the admin disables them.
    app.set_model(Arc::new(Scripted(Mutex::new(VecDeque::from([
        tools(vec![("set_member_login", json!({"enabled": true, "note": "网站加登录"}))]),
        say("会员登录打开了（草稿），左边多了「用户管理」。"),
    ])))));
    chat_turn(&app, &session, "网站加个会员登录").await;
    let (_, _, admin) = get_with(&app, "/admin", Some(&session.0)).await;
    assert!(admin.contains("/admin/members"));
    publish_everything(&app, &session).await;
    let (s, _, _) = post_form(&app, "/account/signup", None, &[("email", "visitor@example.com"), ("password", "long-enough-1")]).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    let (_, _, members) = get_with(&app, "/admin/members", Some(&session.0)).await;
    assert!(members.contains("visitor@example.com"));
    let id = ok(&app, &builder(), "list_members", json!({}))["items"][0]["id"].to_string();
    let (s, _, _) = post_form(&app, &format!("/admin/members/{id}/status"), Some(&session), &[("status", "disabled")]).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    let (s, _, page) = post_form(&app, "/account/login", None, &[("email", "visitor@example.com"), ("password", "long-enough-1")]).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{page}");
}
