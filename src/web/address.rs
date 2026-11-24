//! Where the admin lives. 同一个网址 (the default): site and admin on the site's address (`HC_ORIGIN`).
//! 分开网址: the site there, the admin on its own origin such as `https://admin.example.com`, routed by Host.
//! Chosen in 设置 → 网址 (saved as `site_settings.admin_origin`), overridden by `HC_ADMIN_ORIGIN`, and
//! recoverable with `cms-harness admin-origin off`. The choice is read from the database on every request, so
//! the command applies to a running server at once.

use super::content;
use super::session::AdminUser;
use crate::{App, Shared};
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Extension;
use rand::{rngs::OsRng, RngCore};
use rusqlite::{Connection, OptionalExtension};
use std::time::{Duration, Instant};

/// How long the old address keeps answering for the admin after a switch.
pub const GRACE: Duration = Duration::from_secs(600);

/// The address facts of one request, set by the front middleware for every handler.
#[derive(Clone, Debug)]
pub struct Address {
    /// The admin's own origin in 分开 mode.
    pub admin: Option<String>,
    /// The origin this request reached the admin at: the admin origin on its host, else the site's.
    pub here: String,
}

impl Address {
    /// Admin cookies get `Secure` on an https admin address.
    pub fn secure(&self) -> bool {
        self.here.starts_with("https://")
    }
}

/// `scheme://host[:port]`, http(s), lowercase, nothing after: the only shape stored or put into a header.
pub fn normalize(s: &str) -> Option<String> {
    let s = s.trim().trim_end_matches('/').to_ascii_lowercase();
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://"))?;
    let ok = !rest.is_empty() && !rest.starts_with(['.', '-', ':']) && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b".-:".contains(&b));
    ok.then_some(s)
}

/// The host of an origin or a Host header, lowercase, without a default port.
fn host_key(s: &str) -> String {
    let s = s.to_ascii_lowercase();
    let host = s.split_once("://").map_or(s.as_str(), |(_, h)| h);
    host.strip_suffix(":443").or_else(|| host.strip_suffix(":80")).unwrap_or(host).to_string()
}

/// The admin origin saved in 设置 → 网址, if any.
pub fn saved(conn: &Connection) -> Option<String> {
    conn.query_row("SELECT value FROM site_settings WHERE key = 'admin_origin'", [], |r| r.get::<_, String>(0)).optional().ok().flatten()
}

/// Saves the choice: an admin origin for 分开网址, `None` for 同一个网址.
pub fn save(conn: &Connection, origin: Option<&str>) -> Result<(), String> {
    let done = match origin {
        Some(o) => conn.execute("INSERT INTO site_settings(key, value) VALUES('admin_origin', ?1) ON CONFLICT(key) DO UPDATE SET value = ?1", [o]),
        None => conn.execute("DELETE FROM site_settings WHERE key = 'admin_origin'", []),
    };
    done.map(drop).map_err(|e| format!("保存网址设置失败：{e}"))
}

/// The admin's own origin when 分开网址 is on: the environment's, else the saved one.
pub fn current(app: &App) -> Option<String> {
    match app.env_admin_origin.get() {
        Some(env) => env.clone(),
        None => saved(&app.conn()),
    }
}

#[derive(Debug, PartialEq)]
enum Side {
    Admin,
    Site,
    Both,
}

/// Which host a path belongs to in 分开 mode. Uploads and the public content API serve both: admin pages and the
/// preview (on the admin host) use them by absolute path. The ping must answer on any address.
fn side(path: &str) -> Side {
    let under = |p: &str| path == p || path.starts_with(&format!("{p}/"));
    if under("/admin") || under("/api/v1/tools") || under("/api/v1/site") || under("/api/v1/media") || path == "/mcp" || path.starts_with("/_hc/preview") {
        Side::Admin
    } else if under("/uploads") || under("/api/v1/content") || path == "/_hc/ping" {
        Side::Both
    } else {
        Side::Site
    }
}

/// Decides where a request goes: on, or a response that sends it elsewhere (or nowhere).
/// The caller has read the mode (`admin`) off the blocking pool.
pub fn route(app: &App, admin: Option<String>, req: &Request) -> Result<Address, Box<Response>> {
    let Some(admin) = admin else { return Ok(Address { admin: None, here: app.origin.clone() }) };
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).map(str::to_string).or_else(|| req.uri().authority().map(|a| a.to_string()));
    let on_admin = host.is_some_and(|h| host_key(&h) == host_key(&admin));
    let side = side(req.uri().path());
    let target = req.uri().path_and_query().map_or("/", |p| p.as_str());
    if on_admin {
        return match side {
            Side::Site => Err(Box::new(Redirect::to(&format!("{}{target}", app.origin)).into_response())),
            _ => Ok(Address { here: admin.clone(), admin: Some(admin) }),
        };
    }
    if side == Side::Admin {
        let grace = app.old_admin_until.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|until| Instant::now() < until);
        return match (grace, matches!(*req.method(), Method::GET | Method::HEAD)) {
            (true, true) => Err(Box::new(Redirect::to(&format!("{admin}{target}")).into_response())),
            (true, false) => Ok(Address { admin: Some(admin), here: app.origin.clone() }),
            (false, _) => Err(Box::new((StatusCode::NOT_FOUND, "找不到这个页面。").into_response())),
        };
    }
    Ok(Address { admin: Some(admin), here: app.origin.clone() })
}

/// `GET /_hc/ping`: the nonce a switch is waiting for, once.
pub async fn ping(State(app): State<Shared>) -> Response {
    match app.ping_nonce.lock().unwrap_or_else(|e| e.into_inner()).take() {
        Some(n) => ([(header::CACHE_CONTROL, "no-store")], n).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// What to set up before switching: the DNS record and the proxy rule for the new host.
pub fn setup_line(app: &App, admin: &str) -> String {
    let site = host_key(&app.origin);
    let host = admin.split_once("://").map_or(admin, |(_, h)| h);
    format!("在域名解析里加一条，让 {host} 指向这台服务器（和 {site} 一样）；在反向代理里把 {host} 也转给 CMS Harness（和网站同一个端口），并原样带上 Host 头。")
}

/// Asks the new address for the ping and checks the answer is ours: the address really reaches this server.
async fn reaches_us(app: &App, origin: &str) -> Result<(), String> {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let nonce = hex::encode(bytes);
    *app.ping_nonce.lock().unwrap_or_else(|e| e.into_inner()) = Some(nonce.clone());
    let client = reqwest::Client::builder().timeout(Duration::from_secs(5)).redirect(reqwest::redirect::Policy::none()).build().map_err(|e| e.to_string())?;
    let answer = match client.get(format!("{origin}/_hc/ping")).send().await {
        Ok(r) if r.status().is_success() => r.text().await.ok(),
        _ => None,
    };
    app.ping_nonce.lock().unwrap_or_else(|e| e.into_inner()).take();
    if answer.is_some_and(|a| a.trim() == nonce) {
        Ok(())
    } else {
        Err(format!(
            "没能从 {origin} 访问到这台服务器，所以没有切换。{} 做好后再点一次。有些网络不让服务器从内部访问自己的公网地址；这时可以用环境变量 HC_ADMIN_ORIGIN 或命令 cms-harness admin-origin {origin} 来设。",
            setup_line(app, origin)
        ))
    }
}

/// `POST /admin/settings/address`: `mode=same`, or `mode=separate` with `admin_origin`. Builders only.
/// 分开 is saved only after the new address answers the ping. The old address keeps answering for `GRACE`.
pub async fn switch(State(app): State<Shared>, Extension(user): Extension<AdminUser>, req: Request) -> Response {
    let posted = match content::checked(&user, req).await {
        Ok(p) => p,
        Err(o) => return o.into_response(),
    };
    if user.role != "builder" {
        return (StatusCode::FORBIDDEN, format!("“{}”是编辑：只有搭建者（builder）可以改网址。", user.email)).into_response();
    }
    let typed = posted.one("admin_origin").trim().to_string();
    let chosen: Result<Option<String>, String> = if app.env_admin_origin.get().is_some() {
        Err(ENV_SET.into())
    } else if posted.one("mode") == "same" {
        Ok(None)
    } else if posted.one("mode") != "separate" {
        Err("请选“同一个网址”或“分开网址”。".into())
    } else {
        match normalize(&typed) {
            None => Err(format!("“{typed}”不是一个网址：要形如 https://admin.example.com，不带路径。")),
            Some(o) if host_key(&o) == host_key(&app.origin) => Err(format!("后台的网址不能和网站一样（{}）：要分开就换一个，比如 admin. 开头的子域名。", app.origin)),
            Some(o) => reaches_us(&app, &o).await.map(|()| Some(o)),
        }
    };
    let mut resp = content::blocking(move || {
        let chosen = match chosen {
            Ok(c) => c,
            Err(e) => return super::settings::address_refused(&app, &user, e, typed),
        };
        save(&app.conn(), chosen.as_deref()).map_err(content::internal)?;
        let mut until = app.old_admin_until.lock().unwrap_or_else(|e| e.into_inner());
        *until = chosen.as_ref().map(|_| Instant::now() + GRACE);
        drop(until);
        super::settings::address_saved(&app, &user)
    })
    .await;
    resp.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    resp
}

pub const ENV_SET: &str = "网址由服务器的环境变量 HC_ADMIN_ORIGIN 设置，这里不能改：请改环境变量后重启。";

/// The 网址 section of the settings page.
pub fn section(app: &App, typed: Option<String>) -> serde_json::Value {
    let admin = current(app);
    let example = typed.clone().filter(|t| normalize(t).is_some()).or_else(|| admin.clone()).unwrap_or_else(|| {
        let site = host_key(&app.origin);
        let scheme = app.origin.split_once("://").map_or("https", |(s, _)| s);
        format!("{scheme}://admin.{}", site.strip_prefix("www.").unwrap_or(&site))
    });
    serde_json::json!({
        "site": app.origin,
        "admin": admin,
        "env": app.env_admin_origin.get().is_some(),
        "typed": typed,
        "setup": setup_line(app, &example),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_and_hosts() {
        assert_eq!(normalize(" HTTPS://Admin.Example.com/ ").as_deref(), Some("https://admin.example.com"));
        assert_eq!(normalize("http://127.0.0.1:4300").as_deref(), Some("http://127.0.0.1:4300"));
        for bad in ["admin.example.com", "ftp://a", "https://", "https://a/b", "https://a b", "https://a?x", "https://.a"] {
            assert_eq!(normalize(bad), None, "{bad}");
        }
        assert_eq!(host_key("https://admin.example.com"), host_key("admin.example.com:443"));
        assert_eq!(host_key("http://admin.example.com"), host_key("ADMIN.example.com:80"));
        assert_eq!(host_key("https://admin.example.com"), "admin.example.com");
        assert_ne!(host_key("http://127.0.0.1:4200"), host_key("127.0.0.1:4300"));
        assert_eq!(side("/admin"), Side::Admin);
        assert_eq!(side("/administrator"), Side::Site);
        assert_eq!(side("/_hc/preview/x/y.css"), Side::Admin);
        assert_eq!(side("/uploads/a.png"), Side::Both);
        assert_eq!(side("/api/v1/content/news"), Side::Both);
        assert_eq!(side("/about"), Side::Site);
    }
}
