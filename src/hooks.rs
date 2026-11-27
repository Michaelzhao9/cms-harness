//! 通知 (webhooks): what happens in the environment is passed on to where a builder said — a 飞书 group robot, a
//! 企业微信 group robot, or an address of their own. Which of the three an address is, is read from the address.
//!
//! `fire` only writes the message down, in the transaction of the thing it tells of: what is rolled back tells
//! nobody. `deliver_due` sends what waits, one message a round for each destination, and never to an address
//! inside the private network. Only a builder adds a destination, in 设置: no tool does.

use crate::{App, Shared};
use base64::Engine;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::time::Duration;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS hooks(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, url TEXT NOT NULL, secret TEXT, events TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')), last_at TEXT, last_ok INTEGER, last_note TEXT);
CREATE TABLE IF NOT EXISTS hook_outbox(id INTEGER PRIMARY KEY, hook_id INTEGER NOT NULL REFERENCES hooks(id) ON DELETE CASCADE,
  event TEXT NOT NULL, text TEXT NOT NULL, data TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, next_at TEXT NOT NULL DEFAULT (datetime('now')));
";

/// What a destination can ask to hear of: (event, what the settings page calls it).
pub const EVENTS: [(&str, &str); 4] = [("submit", "有人提交了内容"), ("publish", "发布了修改"), ("member", "有新会员注册"), ("notify", "函数里的 notify()")];

const MAX_HOOKS: i64 = 10;
/// How many messages may wait for one destination; more are not written down.
// ponytail: a flood of forms loses its tail instead of queueing without end; count what was dropped if anyone asks.
const MAX_WAITING: i64 = 100;
const TRIES: i64 = 3;
const MAX_TEXT: usize = 2000;
/// How often the server sends what waits. One message a round for each destination: 20 a minute, 企业微信's limit.
pub const ROUND: Duration = Duration::from_secs(3);

fn db(e: rusqlite::Error) -> String {
    format!("读写通知设置失败：{e}")
}

pub fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let inner = Sha256::new().chain_update(block.map(|b| b ^ 0x36)).chain_update(msg).finalize();
    Sha256::new().chain_update(block.map(|b| b ^ 0x5c)).chain_update(inner).finalize().into()
}

/// `feishu`, `wecom` or `custom`, by the host of the address.
// ponytail: a privately deployed 飞书 has a host of its own and is taken for `custom`; add a choice in the form then.
pub fn kind_of(url: &str) -> &'static str {
    let host = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
    let under = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    if under("feishu.cn") || under("larksuite.com") {
        "feishu"
    } else if host == "qyapi.weixin.qq.com" {
        "wecom"
    } else {
        "custom"
    }
}

fn label(kind: &str) -> &'static str {
    match kind {
        "feishu" => "飞书",
        "wecom" => "企业微信",
        _ => "自定义",
    }
}

/// Adds a destination. The address is a secret (whoever has it can write into the group) and is never shown again.
pub fn add(conn: &Connection, url: &str, secret: &str, events: &[String]) -> Result<i64, String> {
    let (url, secret) = (url.trim(), secret.trim());
    let parsed = reqwest::Url::parse(url).ok().filter(|u| u.host_str().is_some()).ok_or("这不是一个网址：把机器人的 Webhook 地址整个粘贴进来，以 https:// 开头。")?;
    if !(parsed.scheme() == "https" || parsed.scheme() == "http" && crate::websearch::private_allowed()) {
        return Err("Webhook 地址要以 https:// 开头。".into());
    }
    if url.len() > 500 || secret.chars().count() > 200 {
        return Err("地址或签名密钥太长了。".into());
    }
    if events.is_empty() {
        return Err("请至少勾一样：什么时候发消息。".into());
    }
    if let Some(bad) = events.iter().find(|e| !EVENTS.iter().any(|(k, _)| k == e)) {
        return Err(format!("不认识的事件“{bad}”：可选 {}。", EVENTS.map(|(k, _)| k).join("、")));
    }
    if conn.query_row("SELECT COUNT(*) FROM hooks", [], |r| r.get::<_, i64>(0)).map_err(db)? >= MAX_HOOKS {
        return Err(format!("最多 {MAX_HOOKS} 个去处：先删掉不用的。"));
    }
    conn.execute(
        "INSERT INTO hooks(kind, url, secret, events) VALUES (?1, ?2, ?3, ?4)",
        params![kind_of(url), url, Some(secret).filter(|s| !s.is_empty()), json!(events).to_string()],
    )
    .map_err(db)?;
    Ok(conn.last_insert_rowid())
}

pub fn remove(conn: &Connection, id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM hook_outbox WHERE hook_id = ?1", [id]).map_err(db)?;
    conn.execute("DELETE FROM hooks WHERE id = ?1", [id]).map(drop).map_err(db)
}

/// Every destination, for the settings page: what it is, the host of its address, what it hears of, how the last
/// message went.
pub fn list(conn: &Connection) -> Vec<Value> {
    let Ok(mut st) = conn.prepare("SELECT id, kind, url, events, last_at, last_ok, last_note, secret IS NOT NULL FROM hooks ORDER BY id") else { return vec![] };
    let rows = st.query_map([], |r| {
        let (kind, url): (String, String) = (r.get(1)?, r.get(2)?);
        let events: Vec<String> = serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default();
        let names: Vec<&str> = EVENTS.iter().filter(|(k, _)| events.iter().any(|e| e == k)).map(|(_, name)| *name).collect();
        Ok(json!({
            "id": r.get::<_, i64>(0)?, "kind": kind, "label": label(&kind),
            "host": reqwest::Url::parse(&url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default(),
            "events": names, "last_at": r.get::<_, Option<String>>(4)?, "last_ok": r.get::<_, Option<bool>>(5)?,
            "last_note": r.get::<_, Option<String>>(6)?, "signed": r.get::<_, bool>(7)?,
        }))
    });
    rows.map(|rows| rows.filter_map(Result::ok).collect()).unwrap_or_default()
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max).collect::<String>()) }
}

/// What a group reads of a new record: its collection and its first few values.
pub fn summary(def: &crate::schema::CollectionDef, doc: &Value) -> String {
    let mut out = format!("「{}」有一条新提交", def.title);
    let said = def.fields.iter().filter_map(|f| {
        let v = match &doc[&f.name] {
            Value::Null => return None,
            Value::String(s) if f.kind == crate::schema::FieldType::Richtext => crate::websearch::page_text(s).1,
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        Some((f, v)).filter(|(_, v)| !v.trim().is_empty())
    });
    for (f, v) in said.take(8) {
        out.push_str(&format!("\n{}：{}", f.title, short(v.trim(), 120)));
    }
    out
}

/// Writes `text` down for every destination that asked to hear of `event`. It never fails what it is called from.
/// `<` is taken out of the text: what a visitor typed must not become a mention of everyone in the group.
pub fn fire(conn: &Connection, event: &str, text: &str, data: Value) {
    let text = short(&text.replace('<', "＜"), MAX_TEXT);
    let _ = conn.execute(
        "INSERT INTO hook_outbox(hook_id, event, text, data) SELECT h.id, ?1, ?2, ?3 FROM hooks h
         WHERE EXISTS (SELECT 1 FROM json_each(h.events) e WHERE e.value = ?1)
           AND (SELECT COUNT(*) FROM hook_outbox o WHERE o.hook_id = h.id) < ?4",
        params![event, text, data.to_string(), MAX_WAITING],
    );
}

/// One message as a service wants it.
pub struct Message {
    pub body: String,
    pub headers: Vec<(&'static str, String)>,
}

/// The request for one message. 飞书 and 企业微信 get a line of text headed by the site's name; an address of the
/// builder's own gets the event as JSON, signed with the secret when there is one (`sha256=` and the HMAC-SHA256 of
/// `<timestamp>.<body>`, so a copy of an old request does not pass for a new one).
pub fn message(kind: &str, secret: Option<&str>, site: &str, event: &str, text: &str, data: &Value, now: u64) -> Message {
    let headed = format!("【{site}】{text}");
    match kind {
        "feishu" => {
            let mut body = json!({ "msg_type": "text", "content": { "text": headed } });
            if let Some(secret) = secret {
                // 飞书's 签名校验: the key is the time and the secret, the message is empty.
                body["timestamp"] = json!(now.to_string());
                body["sign"] = json!(base64::engine::general_purpose::STANDARD.encode(hmac(format!("{now}\n{secret}").as_bytes(), b"")));
            }
            Message { body: body.to_string(), headers: vec![] }
        }
        "wecom" => Message { body: json!({ "msgtype": "text", "text": { "content": headed } }).to_string(), headers: vec![] },
        _ => {
            let body = json!({ "event": event, "text": text, "data": data, "site": site, "at": now }).to_string();
            let mut headers = vec![("X-Harness-Event", event.to_string()), ("X-Harness-Timestamp", now.to_string())];
            if let Some(secret) = secret {
                headers.push(("X-Harness-Signature", format!("sha256={}", hex::encode(hmac(secret.as_bytes(), format!("{now}.{body}").as_bytes())))));
            }
            Message { body, headers }
        }
    }
}

/// Whether the other side took the message. The robots answer 200 to almost anything and say no in the body.
fn judge(kind: &str, status: u16, reply: &str) -> Result<(), String> {
    if !(200..300).contains(&status) {
        return Err(format!("对方返回 {status}：{}", short(reply.trim(), 200)));
    }
    let v: Value = serde_json::from_str(reply).unwrap_or(Value::Null);
    let code = match kind {
        "feishu" => v.get("code").or_else(|| v.get("StatusCode")),
        "wecom" => v.get("errcode"),
        _ => return Ok(()),
    };
    match code.and_then(Value::as_i64) {
        Some(0) => Ok(()),
        _ => Err(format!("{}没有收下：{}", label(kind), short(reply.trim(), 200))),
    }
}

/// Posts one message. The name is resolved here, every address it has must be public, and the connection goes to
/// the address that was checked; a redirect is not followed.
async fn post(kind: &str, url: &str, m: Message) -> Result<(), String> {
    let url = reqwest::Url::parse(url).map_err(|_| "地址不对。".to_string())?;
    let host = url.host_str().ok_or("地址里没有主机名。")?.to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.trim_matches(|c| c == '[' || c == ']'), port)).await.map_err(|e| format!("找不到“{host}”：{e}"))?.collect();
    let Some(addr) = addrs.first().copied() else { return Err(format!("找不到“{host}”。")) };
    if addrs.iter().any(|a| !crate::websearch::public(a.ip())) {
        return Err(format!("“{host}”是内网或本机地址：消息不会发到那里。"));
    }
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("cms-harness")
        .resolve(&host, addr)
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = http.post(url).header(reqwest::header::CONTENT_TYPE, "application/json").body(m.body);
    for (k, v) in m.headers {
        req = req.header(k, v);
    }
    // The address is a secret: an error must not repeat it.
    let mut resp = req.send().await.map_err(|e| format!("连不上“{host}”：{}", e.without_url()))?;
    let status = resp.status().as_u16();
    let mut reply = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        reply.extend_from_slice(&chunk);
        if reply.len() > 4096 {
            break;
        }
    }
    judge(kind, status, &String::from_utf8_lossy(&reply))
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Where in the admin a message's reader would look next.
fn link(event: &str, data: &Value) -> Option<String> {
    match event {
        "submit" => data["collection"].as_str().map(|c| format!("/admin/c/{c}")),
        "member" => Some("/admin/members".into()),
        "publish" => Some("/admin/releases".into()),
        _ => None,
    }
}

fn mark(conn: &Connection, id: i64, sent: &Result<(), String>) {
    let note = sent.as_ref().err().map(|e| short(e, 300));
    let _ = conn.execute("UPDATE hooks SET last_at = datetime('now'), last_ok = ?2, last_note = ?3 WHERE id = ?1", params![id, sent.is_ok(), note]);
}

struct Due {
    id: i64,
    hook: i64,
    kind: String,
    url: String,
    secret: Option<String>,
    event: String,
    text: String,
    data: Value,
    attempts: i64,
}

/// Sends the oldest waiting message of every destination, when its time has come; returns how many were tried.
/// One that fails waits a minute, then five, and is dropped after the third try; those behind it wait their turn.
pub async fn deliver_due(app: &Shared) -> usize {
    let a = app.clone();
    let read = tokio::task::spawn_blocking(move || {
        let admin = crate::web::address::current(&a).unwrap_or_else(|| a.origin.clone());
        let conn = a.conn();
        let due: Vec<Due> = conn
            .prepare(
                "SELECT o.id, o.hook_id, h.kind, h.url, h.secret, o.event, o.text, o.data, o.attempts FROM hook_outbox o JOIN hooks h ON h.id = o.hook_id
                 WHERE o.id IN (SELECT MIN(id) FROM hook_outbox GROUP BY hook_id) AND o.next_at <= datetime('now')",
            )
            .and_then(|mut st| {
                let rows = st.query_map([], |r| {
                    Ok(Due { id: r.get(0)?, hook: r.get(1)?, kind: r.get(2)?, url: r.get(3)?, secret: r.get(4)?, event: r.get(5)?, text: r.get(6)?,
                        data: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or(Value::Null), attempts: r.get(8)? })
                })?;
                rows.collect()
            })
            .unwrap_or_default();
        (a.name(&conn, false), admin, due)
    });
    let Ok((site, admin, due)) = read.await else { return 0 };
    let sending = due.iter().map(|d| {
        let text = match link(&d.event, &d.data) {
            Some(path) => format!("{}\n{admin}{path}", d.text),
            None => d.text.clone(),
        };
        post(&d.kind, &d.url, message(&d.kind, d.secret.as_deref(), &site, &d.event, &text, &d.data, now()))
    });
    let sent = futures_util::future::join_all(sending).await;
    let (a, n) = (app.clone(), due.len());
    let _ = tokio::task::spawn_blocking(move || {
        let conn = a.conn();
        for (d, sent) in due.iter().zip(&sent) {
            let _ = match sent {
                Err(_) if d.attempts + 1 < TRIES => conn.execute(
                    "UPDATE hook_outbox SET attempts = attempts + 1, next_at = datetime('now', ?2) WHERE id = ?1",
                    params![d.id, if d.attempts == 0 { "+1 minutes" } else { "+5 minutes" }],
                ),
                _ => conn.execute("DELETE FROM hook_outbox WHERE id = ?1", [d.id]),
            };
            mark(&conn, d.hook, sent);
        }
    })
    .await;
    n
}

/// 发送测试: one message to one destination, now.
pub async fn test(app: &Shared, id: i64) -> Result<(), String> {
    let a = app.clone();
    let read = tokio::task::spawn_blocking(move || {
        let conn = a.conn();
        let row = conn.query_row("SELECT kind, url, secret FROM hooks WHERE id = ?1", [id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)));
        row.map(|r| (r, a.name(&conn, false))).map_err(|_| "没有这个去处，可能已经删掉了。".to_string())
    });
    let ((kind, url, secret), site) = read.await.map_err(|e| e.to_string())??;
    let sent = post(&kind, &url, message(&kind, secret.as_deref(), &site, "test", "这是一条测试消息：通知已经接通。", &json!({}), now())).await;
    let (a, kept) = (app.clone(), sent.clone());
    let _ = tokio::task::spawn_blocking(move || mark(&a.conn(), id, &kept)).await;
    sent
}

/// The server's own round: sends what waits, for as long as it runs.
pub async fn run(app: Shared) {
    let mut tick = tokio::time::interval(ROUND);
    loop {
        tick.tick().await;
        deliver_due(&app).await;
    }
}

/// The settings page's part: the destinations and the events to choose from.
pub fn section(app: &App) -> Value {
    json!({ "list": list(&app.conn()), "events": EVENTS.map(|(k, name)| json!({ "key": k, "name": name, "on": matches!(k, "submit" | "publish") })) })
}

#[cfg(test)]
mod tests {
    use super::judge;

    #[test]
    fn a_robot_that_answers_200_can_still_have_said_no() {
        assert!(judge("feishu", 200, r#"{"code":0,"msg":"success","data":{}}"#).is_ok());
        assert!(judge("feishu", 200, r#"{"StatusCode":0,"StatusMessage":"success"}"#).is_ok());
        assert!(judge("feishu", 200, r#"{"code":19021,"msg":"sign match fail or timestamp is not within one hour from current time"}"#).unwrap_err().contains("sign match fail"));
        assert!(judge("wecom", 200, r#"{"errcode":0,"errmsg":"ok"}"#).is_ok());
        assert!(judge("wecom", 200, r#"{"errcode":93000,"errmsg":"invalid webhook url"}"#).unwrap_err().contains("93000"));
        assert!(judge("wecom", 200, "<html>").is_err(), "not the robot's answer at all");
        assert!(judge("custom", 204, "").is_ok());
        assert!(judge("custom", 500, "boom").unwrap_err().contains("500"));
    }
}
