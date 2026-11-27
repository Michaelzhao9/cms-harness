//! 通知: what happens in the environment is passed on to where a builder said: a 飞书 group, a 企业微信 group, or
//! an address of their own. A message first waits in the database, in the same transaction as what it tells of, and
//! is sent from there.

mod common;

use axum::http::StatusCode;
use cms_harness::access::{self, Submitter};
use cms_harness::{hooks, Shared};
use common::*;
use serde_json::{json, Value};

fn add(app: &Shared, url: &str, secret: &str, events: &[&str]) -> i64 {
    let events: Vec<String> = events.iter().map(|e| e.to_string()).collect();
    hooks::add(&app.conn(), url, secret, &events).unwrap_or_else(|e| panic!("{url}: {e}"))
}

/// What waits to be sent: (destination, event, text), oldest first.
fn waiting(app: &Shared) -> Vec<(i64, String, String)> {
    let conn = app.conn();
    let mut st = conn.prepare("SELECT hook_id, event, text FROM hook_outbox ORDER BY id").unwrap();
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    rows.collect::<Result<_, _>>().unwrap()
}

fn guestbook(app: &Shared) {
    ok(app, &builder(), "create_collection", json!({"name": "messages", "title": "留言", "read": "none", "create": "public", "note": "建留言", "fields": [
        {"name": "name", "title": "称呼", "type": "text", "required": true}, {"name": "text", "title": "内容", "type": "text"}]}));
}

#[test]
fn hmac_is_the_standard_one() {
    assert_eq!(hex::encode(hooks::hmac(b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    assert_eq!(hex::encode(hooks::hmac(&[b'k'; 100], b"msg")), "bd56a1782c2830e8abc6ed866a57a1230661e650b84c62f7ee3accc5fa5af491", "a key longer than a block");
}

#[test]
fn what_a_destination_is_is_read_from_its_address() {
    for (url, kind) in [
        ("https://open.feishu.cn/open-apis/bot/v2/hook/abc", "feishu"),
        ("https://open.larksuite.com/open-apis/bot/v2/hook/abc", "feishu"),
        ("https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=abc", "wecom"),
        ("https://example.com/hook", "custom"),
        ("https://feishu.cn.example.com/hook", "custom"),
    ] {
        assert_eq!(hooks::kind_of(url), kind, "{url}");
    }
}

#[test]
fn each_service_gets_the_message_in_its_own_shape() {
    let data = json!({"collection": "messages", "id": 7});
    let body = |kind: &str, secret: Option<&str>| -> Value {
        serde_json::from_str(&hooks::message(kind, secret, "青山中学", "submit", "「留言」有一条新提交", &data, 1_700_000_000).body).unwrap()
    };
    assert_eq!(body("feishu", None), json!({"msg_type": "text", "content": {"text": "【青山中学】「留言」有一条新提交"}}));
    assert_eq!(
        body("feishu", Some("sec-123")),
        json!({"timestamp": "1700000000", "sign": "4hGGoek7zn9v4Z6/8FjLE4ATil0aCPtSV6vo90TkRQ4=", "msg_type": "text", "content": {"text": "【青山中学】「留言」有一条新提交"}}),
        "飞书's 签名校验"
    );
    assert_eq!(body("wecom", Some("unused")), json!({"msgtype": "text", "text": {"content": "【青山中学】「留言」有一条新提交"}}));
    assert_eq!(body("custom", None), json!({"event": "submit", "text": "「留言」有一条新提交", "data": data, "site": "青山中学", "at": 1_700_000_000}));

    let plain = hooks::message("custom", None, "青山中学", "submit", "x", &data, 1_700_000_000);
    assert!(!plain.headers.iter().any(|(k, _)| k.contains("Signature")), "no secret, no signature");
    let signed = hooks::message("custom", Some("sec-123"), "青山中学", "submit", "x", &data, 1_700_000_000);
    let header = |name: &str| signed.headers.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone()).unwrap_or_default();
    assert_eq!((header("X-Harness-Event"), header("X-Harness-Timestamp")), ("submit".to_string(), "1700000000".to_string()));
    let expected = hex::encode(hooks::hmac(b"sec-123", format!("1700000000.{}", signed.body).as_bytes()));
    assert_eq!(header("X-Harness-Signature"), format!("sha256={expected}"), "the time and the body, signed together");
}

#[test]
fn what_happens_waits_for_those_who_asked_to_hear_of_it() {
    let app = app();
    let forms = add(&app, "https://example.com/forms", "", &["submit"]);
    let rest = add(&app, "https://example.com/rest", "", &["publish", "member"]);
    ok(&app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    guestbook(&app);
    ok(&app, &builder(), "publish", json!({"note": "上线留言板"}));
    let heard = waiting(&app);
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert_eq!((heard[0].0, heard[0].1.as_str()), (rest, "publish"));
    assert!(heard[0].2.contains("上线留言板") && heard[0].2.contains("builder@test"), "what was published, and by whom: {}", heard[0].2);

    let sly = json!({"name": "小王", "text": "<at user_id=\"all\">所有人</at> 你好"});
    access::submit(&app.conn(), "messages", &sly, &Submitter::Public("1.2.3.4".into())).unwrap();
    let (to, event, text) = waiting(&app).pop().unwrap();
    assert_eq!((to, event.as_str()), (forms, "submit"));
    assert!(text.contains("「留言」") && text.contains("称呼：小王") && text.contains("你好"), "{text}");
    assert!(!text.contains('<'), "a visitor cannot write a mention into the group: {text}");

    cms_harness::members::signup(&app.conn(), "new@example.com", "long-enough-1", "", "1.2.3.4").unwrap();
    let (to, event, text) = waiting(&app).pop().unwrap();
    assert_eq!((to, event.as_str()), (rest, "member"));
    assert!(text.contains("new@example.com"), "{text}");

    assert!(access::submit(&app.conn(), "messages", &json!({"text": "没写称呼"}), &Submitter::Public("1.2.3.4".into())).is_err());
    assert_eq!(waiting(&app).len(), 3, "a refused submission tells nobody");
}

#[tokio::test]
async fn a_function_sends_word_only_when_its_run_counts() {
    let app = app();
    let to = add(&app, "https://example.com/stock", "", &["notify"]);
    ok(&app, &builder(), "write_site_file", json!({"path": "_fn/tell.js", "note": "x",
        "content": "export default (req) => { notify('库存不足：' + req.query.item); if (req.query.fail) throw new Error('坏了'); return { ok: true } }"}));
    let tried = ok(&app, &builder(), "run_function", json!({"path": "/_fn/tell", "query": {"item": "笔"}}));
    assert!(tried["logs"].to_string().contains("库存不足：笔"), "a trial shows what it would have sent: {tried}");
    assert!(waiting(&app).is_empty(), "and sends nothing");

    publish_all(&app);
    let (s, _, v) = get(&app, "/_fn/tell?item=%E7%AC%94", None).await;
    assert_eq!((s, &v), (StatusCode::OK, &json!({"ok": true})));
    assert_eq!(waiting(&app), vec![(to, "notify".to_string(), "库存不足：笔".to_string())]);
    assert_eq!(get(&app, "/_fn/tell?item=x&fail=1", None).await.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(waiting(&app).len(), 1, "a run that failed told nobody");
}

#[tokio::test]
async fn a_builder_sets_destinations_in_settings_and_nobody_else() {
    let app = app();
    let session = admin_session(&app, "builder").await;
    let url = "https://open.feishu.cn/open-apis/bot/v2/hook/0123456789abcdef-secret-part";
    let (s, h, body) = post_form(&app, "/admin/settings/hooks", Some(&session), &[("url", url), ("secret", "sec-123"), ("event", "submit"), ("event", "publish")]).await;
    assert_eq!((s, h.get("location").and_then(|v| v.to_str().ok())), (StatusCode::SEE_OTHER, Some("/admin/settings?saved=hook#s-hooks")), "{body}");
    let (_, _, page) = get_with(&app, "/admin/settings?saved=hook", Some(&session.0)).await;
    assert!(page.contains("飞书") && page.contains("open.feishu.cn"), "{page}");
    assert!(!page.contains("secret-part") && !page.contains("sec-123"), "the address is itself a secret: only its host is shown");
    assert!(page.contains("id=\"s-hooks\" name=\"set\" open"), "the row it was saved in is the one open");

    for (fields, names) in [
        (vec![("url", "not a url"), ("event", "submit")], "网址"),
        (vec![("url", "ftp://example.com/x"), ("event", "submit")], "https"),
        (vec![("url", "https://example.com/x")], "什么时候"),
        (vec![("url", "https://example.com/x"), ("event", "everything")], "everything"),
    ] {
        let (s, _, page) = post_form(&app, "/admin/settings/hooks", Some(&session), &fields).await;
        assert!(s == StatusCode::BAD_REQUEST && page.contains(names), "{fields:?}: {s}");
    }

    let editor = admin_session(&app, "editor").await;
    assert_eq!(post_form(&app, "/admin/settings/hooks", Some(&editor), &[("url", "https://example.com/x"), ("event", "submit")]).await.0, StatusCode::FORBIDDEN);
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&editor.0)).await;
    assert!(!page.contains("s-hooks") && !page.contains("open.feishu.cn"), "an editor does not see where messages go");
    let tools = cms_harness::tools::specs().iter().map(|t| t.name.to_string()).collect::<Vec<_>>().join(" ");
    assert!(!tools.contains("hook"), "no tool adds a destination: an AI cannot point messages somewhere of its own: {tools}");

    let id: i64 = app.conn().query_row("SELECT id FROM hooks", [], |r| r.get(0)).unwrap();
    hooks::fire(&app.conn(), "submit", "x", json!({}));
    assert_eq!(post_form(&app, &format!("/admin/settings/hooks/{id}/delete"), Some(&editor), &[]).await.0, StatusCode::FORBIDDEN);
    assert_eq!(post_form(&app, &format!("/admin/settings/hooks/{id}/delete"), Some(&session), &[]).await.0, StatusCode::SEE_OTHER);
    assert!(hooks::list(&app.conn()).is_empty() && waiting(&app).is_empty(), "gone, with what waited for it");
}

/// A server that keeps what it is sent: (path, headers, body). `/fail` answers 500.
async fn listener() -> (String, tokio::sync::mpsc::UnboundedReceiver<(String, axum::http::HeaderMap, Value)>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = move |req: axum::extract::Request| {
        let tx = tx.clone();
        async move {
            let (parts, body) = req.into_parts();
            let bytes = http_body_util::BodyExt::collect(body).await.unwrap().to_bytes();
            let path = parts.uri.path().to_string();
            let _ = tx.send((path.clone(), parts.headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null)));
            if path == "/fail" { StatusCode::INTERNAL_SERVER_ERROR } else { StatusCode::OK }
        }
    };
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", tcp.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(tcp, axum::Router::new().fallback(handle)).await.unwrap() });
    (base, rx)
}

fn state(app: &Shared, id: i64) -> (Option<bool>, String) {
    let h = hooks::list(&app.conn()).into_iter().find(|h| h["id"] == id).unwrap();
    (h["last_ok"].as_bool(), h["last_note"].as_str().unwrap_or_default().to_string())
}

// The one test that sends: it changes the process's environment half way, so nothing else here may depend on it.
#[tokio::test]
async fn messages_are_sent_one_at_a_time_and_never_into_the_private_network() {
    let app = app();
    let (base, mut got) = listener().await;
    guestbook(&app);
    publish_all(&app);

    // Before anything is allowed: this machine is not a place a message may go.
    assert!(hooks::add(&app.conn(), &format!("{base}/hook"), "", &["submit".to_string()]).unwrap_err().contains("https"));
    let inside = add(&app, &format!("{}/hook", base.replace("http://", "https://")), "", &["submit"]);
    hooks::fire(&app.conn(), "submit", "x", json!({}));
    assert_eq!(hooks::deliver_due(&app).await, 1);
    let (ok, note) = state(&app, inside);
    assert!(ok == Some(false) && note.contains("内网"), "{note}");
    assert!(got.try_recv().is_err(), "nothing was connected to");
    hooks::remove(&app.conn(), inside).unwrap();

    std::env::set_var("HC_FETCH_ALLOW_PRIVATE", "1");
    let good = add(&app, &format!("{base}/hook"), "sec-123", &["submit"]);
    let (s, v) = post_json(&app, "/api/v1/content/messages", None, json!({"name": "小王", "text": "你好"})).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    for n in ["第二条", "第三条"] {
        hooks::fire(&app.conn(), "submit", n, json!({}));
    }
    assert_eq!(hooks::deliver_due(&app).await, 1, "one message a round for each destination: a flood of forms is a trickle in the group");
    let (path, headers, body) = got.recv().await.unwrap();
    assert_eq!((path.as_str(), body["event"].as_str(), body["site"].as_str()), ("/hook", Some("submit"), Some("测试站")));
    assert_eq!((body["data"]["collection"].as_str(), body["data"]["record"]["name"].as_str(), body["data"]["id"].as_i64()), (Some("messages"), Some("小王"), v["id"].as_i64()), "{body}");
    assert!(body["text"].as_str().unwrap().contains("称呼：小王"));
    let stamp = headers["x-harness-timestamp"].to_str().unwrap();
    let signature = hex::encode(hooks::hmac(b"sec-123", format!("{stamp}.{body}").as_bytes()));
    assert_eq!(headers["x-harness-signature"].to_str().unwrap(), format!("sha256={signature}"));
    assert_eq!((state(&app, good).0, waiting(&app).len()), (Some(true), 2), "sent and no longer waiting; the rest in their turn");
    assert_eq!(hooks::deliver_due(&app).await, 1);
    assert_eq!(got.recv().await.unwrap().2["text"], "第二条");

    // One that fails is tried again later, a few times, and then given up on.
    let bad = add(&app, &format!("{base}/fail"), "", &["publish"]);
    hooks::fire(&app.conn(), "publish", "发布了", json!({}));
    hooks::deliver_due(&app).await;
    let (ok, note) = state(&app, bad);
    assert!(ok == Some(false) && note.contains("500"), "{note}");
    let left: (i64, bool) = app.conn().query_row("SELECT attempts, next_at > datetime('now') FROM hook_outbox WHERE hook_id = ?1", [bad], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(left, (1, true), "kept, for later");
    app.conn().execute("UPDATE hook_outbox SET attempts = 2, next_at = datetime('now') WHERE hook_id = ?1", [bad]).unwrap();
    hooks::deliver_due(&app).await;
    assert_eq!(app.conn().query_row("SELECT COUNT(*) FROM hook_outbox WHERE hook_id = ?1", [bad], |r| r.get::<_, i64>(0)).unwrap(), 0, "three tries, then dropped");

    // 发送测试 sends now and says how it went.
    while got.try_recv().is_ok() {}
    let session = admin_session(&app, "builder").await;
    let (s, h, _) = post_form(&app, &format!("/admin/settings/hooks/{good}/test"), Some(&session), &[]).await;
    assert_eq!((s, h["location"].to_str().unwrap()), (StatusCode::SEE_OTHER, "/admin/settings?saved=hooksent#s-hooks"));
    assert_eq!(got.recv().await.unwrap().2["event"], "test");
    let (s, _, page) = post_form(&app, &format!("/admin/settings/hooks/{bad}/test"), Some(&session), &[]).await;
    assert!(s == StatusCode::BAD_REQUEST && page.contains("500"), "a test that failed says why: {s}");
}
