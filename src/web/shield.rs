//! 防护 in the admin: the settings (a builder changes them, at once, never a draft) and what was stopped.

use super::content::{self, Out};
use super::session::AdminUser;
use crate::shield::{self, Config, Mode};
use crate::{App, Shared};
use axum::extract::{Query, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use serde_json::{json, Value};

/// What an event kind means, for people.
fn label(kind: &str) -> &'static str {
    match kind {
        "probe" => "探测漏洞的路径",
        "flood" => "一再超过上限，暂时关在门外",
        "limited" => "超过每分钟的请求上限",
        "ai" => "AI 爬虫",
        "tool" => "脚本工具",
        "listed" => "在封禁名单里",
        "challenge" => "翻页太快，要求验证",
        _ => "其他",
    }
}

fn shield_page(app: &App, user: &AdminUser, saved: bool, error: Option<String>) -> Result<Out, Out> {
    let (events, counts) = {
        let conn = app.conn();
        (shield::events(&conn, 100).map_err(content::internal)?, shield::counts(&conn).map_err(content::internal)?)
    };
    let events: Vec<Value> = events
        .into_iter()
        .map(|mut e| {
            e["label"] = json!(label(e["kind"].as_str().unwrap_or_default()));
            e
        })
        .collect();
    let stopped: i64 = counts.as_object().map_or(0, |o| o.values().filter_map(Value::as_i64).sum());
    let c = app.shield.config();
    let status = if error.is_some() { StatusCode::BAD_REQUEST } else { StatusCode::OK };
    let ctx = json!({
        "config": c, "blocked_text": c.blocked.join("\n"), "events": events, "stopped": stopped, "builder": user.role == "builder", "saved": saved, "error": error,
    });
    Ok(Out::Page(status, "shield.html", content::shell(app, user, "shield", ctx)))
}

pub async fn page(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Query(q): Query<Vec<(String, String)>>) -> Response {
    let saved = q.iter().any(|(k, v)| k == "saved" && v == "1");
    content::blocking(move || shield_page(&app, &user, saved, None)).await
}

pub async fn save(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    let posted = match content::checked(&user, req).await {
        Ok(p) => p,
        Err(o) => return o.into_response(),
    };
    if user.role != "builder" {
        return (StatusCode::FORBIDDEN, format!("“{}”是编辑：只有搭建者（builder）可以改防护设置。", user.email)).into_response();
    }
    content::blocking(move || {
        let refuse = |e: String| shield_page(&app, &user, false, Some(e));
        let mode = match posted.one("mode") {
            "off" => Mode::Off,
            "standard" => Mode::Standard,
            "strict" => Mode::Strict,
            other => return refuse(format!("防护档位“{other}”不对：只能是关闭、标准或强化。")),
        };
        let old = app.shield.config();
        let number = |name: &str, now: u32, lo: u32, hi: u32| match posted.one(name).trim() {
            "" => Ok(now),
            s => s.parse::<u32>().ok().filter(|n| (lo..=hi).contains(n)).ok_or_else(|| format!("“{s}”不对：要是 {lo} 到 {hi} 之间的整数。")),
        };
        let (per_minute, pages_per_minute) = match (number("per_minute", old.per_minute, 10, 100_000), number("pages_per_minute", old.pages_per_minute, 5, 10_000)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => return refuse(e),
        };
        let blocked: Vec<String> = posted.one("blocked").lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
        if blocked.len() > 500 || blocked.iter().any(|b| b.len() > 64 || !b.bytes().all(|c| c.is_ascii_hexdigit() || c == b'.' || c == b':')) {
            return refuse("封禁名单里每行写一个地址（比如 203.0.113.7），最多 500 个。".into());
        }
        let config = Config { mode, per_minute, pages_per_minute, block_ai: posted.one("block_ai") == "on", block_tools: posted.one("block_tools") == "on", blocked };
        shield::save(&app.conn(), &config).map_err(content::internal)?;
        app.shield.set_config(config);
        Ok(Out::Go("/admin/shield?saved=1".into()))
    })
    .await
}

/// `GET /_hc/shield.js`: the challenge page's script.
pub async fn script() -> Response {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")], shield::SCRIPT).into_response()
}
