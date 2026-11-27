//! 同一个网址 / 分开网址: the admin on the site's address or on its own, routing by Host, the switch and the recovery.
mod common;
use axum::http::{header, HeaderMap, Request, StatusCode};
use cms_harness::web::address;
use cms_harness::Shared;
use common::*;
use http_body_util::BodyExt;
use serde_json::json;
use std::time::{Duration, Instant};
use tower::ServiceExt;

const SITE: &str = "http://127.0.0.1:4200";
const SITE_HOST: &str = "127.0.0.1:4200";
const ADMIN: &str = "http://admin.test:8080";
const ADMIN_HOST: &str = "admin.test:8080";
const WARNING: &str = "安全提示";

/// A request with a Host header; `form` makes it a urlencoded POST. (status, headers, body)
async fn on(app: &Shared, host: &str, path: &str, cookie: Option<&str>, form: Option<&[(&str, &str)]>, extra: &[(&str, &str)]) -> (StatusCode, HeaderMap, String) {
    let mut req = match form {
        Some(_) => Request::post(path).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded"),
        None => Request::get(path),
    };
    if !host.is_empty() {
        req = req.header(header::HOST, host);
    }
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    let enc = |s: &str| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
    let body = form.map(|f| f.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")).unwrap_or_default();
    let resp = cms_harness::api::router(app.clone()).oneshot(req.body(axum::body::Body::from(body)).unwrap()).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    (status, headers, String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned())
}

fn location(h: &HeaderMap) -> String {
    h[header::LOCATION].to_str().unwrap().to_string()
}

fn site_with_a_page(app: &Shared) {
    ok(app, &builder(), "write_site_file", json!({ "path": "index.html", "content": "<h1>线上</h1>", "note": "页" }));
    news(app);
    ok(app, &builder(), "update_collection", json!({ "name": "news", "public_read": true, "note": "公开" }));
    publish_all(app);
}

#[tokio::test]
async fn same_address_is_the_default_and_settings_warn_about_it() {
    let app = app();
    assert_eq!(address::current(&app), None);
    let s = admin_session(&app, "builder").await;
    let (status, _, page) = get_with(&app, "/admin/settings", Some(&s.0)).await;
    let page = page.replace("&#x2f;", "/");
    assert_eq!(status, StatusCode::OK);
    // The choice is offered plainly; the page carries no security notices.
    assert!(!page.contains(WARNING) && !page.contains("冒充你") && page.contains("分开网址"), "{page}");
    assert!(page.contains("action=\"/admin/settings/address\""), "{page}");
    assert!(page.contains(&format!("value=\"{SITE}/mcp\"")) && page.contains(&format!("value=\"{SITE}/api/v1/content/&lt;")), "{page}");
    // Editors cannot switch, so the row is not on their page.
    let e = admin_session(&app, "editor").await;
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&e.0)).await;
    assert!(!page.contains(WARNING) && !page.contains("action=\"/admin/settings/address\"") && !page.contains("id=\"s-address\""), "{page}");
    // Admin and site answer on any host.
    for host in ["", SITE_HOST, ADMIN_HOST] {
        assert_eq!(on(&app, host, "/admin/login", None, None, &[]).await.0, StatusCode::OK, "{host}");
        assert_eq!(on(&app, host, "/", None, None, &[]).await.0, StatusCode::OK, "{host}");
    }
    // Nothing is waiting to be checked, so the ping has nothing to say.
    assert_eq!(on(&app, "", "/_hc/ping", None, None, &[]).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn separate_addresses_route_by_host() {
    let app = app();
    site_with_a_page(&app);
    let s = admin_session(&app, "builder").await;
    address::save(&app.conn(), Some(ADMIN)).unwrap();
    assert_eq!(address::current(&app).as_deref(), Some(ADMIN));

    // Admin paths on the site host do not exist, whatever the request carries. No Host counts as the site's.
    for host in ["", SITE_HOST, "www.example.com"] {
        for path in ["/admin", "/admin/login", "/admin/settings", "/admin/static/admin.css", "/api/v1/tools", "/_hc/preview.js", "/_hc/preview/abc/"] {
            let (status, _, body) = on(&app, host, path, Some(&s.0), None, &[]).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{host} {path}: {body}");
        }
        let (status, _, _) = on(&app, host, "/mcp", None, Some(&[]), &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{host} /mcp");
        let (status, _, _) = on(&app, host, "/admin/login", None, Some(&[("email", "builder@test"), ("password", "secret-pass")]), &[("origin", SITE)]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{host} login");
        let (status, h, body) = on(&app, host, "/", Some(&s.0), None, &[]).await;
        assert_eq!(status, StatusCode::OK, "{host}");
        assert!(body.contains("线上"), "{body}");
        // The site may be framed by the admin's own origin (the 线上 view), and says so in its CSP.
        assert!(h[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains(&format!("frame-ancestors 'self' {ADMIN};")), "{h:?}");
        assert!(h.get(header::X_FRAME_OPTIONS).is_none(), "{h:?}");
    }
    // Site paths on the admin host go to the site.
    for (path, to) in [("/", format!("{SITE}/")), ("/about?x=1", format!("{SITE}/about?x=1")), ("/news/3", format!("{SITE}/news/3"))] {
        let (status, h, _) = on(&app, ADMIN_HOST, path, Some(&s.0), None, &[]).await;
        assert_eq!((status, location(&h)), (StatusCode::SEE_OTHER, to), "{path}");
    }
    // Public data and uploads answer on both.
    for host in [SITE_HOST, ADMIN_HOST] {
        assert_eq!(on(&app, host, "/api/v1/content/news", None, None, &[]).await.0, StatusCode::OK, "{host}");
        let (status, _, body) = on(&app, host, "/uploads/none.png", None, None, &[]).await;
        assert!(status == StatusCode::NOT_FOUND && body.contains("文件不存在"), "{host}: {body}");
    }

    // The admin lives on its host: login there, with its own origin; the cookie stays on that host.
    let (status, _, _) = on(&app, ADMIN_HOST, "/admin/login", None, Some(&[("email", "builder@test"), ("password", "secret-pass")]), &[("origin", SITE)]).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the site's origin is not the admin's");
    let (status, h, _) = on(&app, ADMIN_HOST, "/admin/login", None, Some(&[("email", "builder@test"), ("password", "secret-pass")]), &[("origin", ADMIN)]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let set = h[header::SET_COOKIE].to_str().unwrap().to_string();
    assert!(set.starts_with("hc_admin=") && set.contains("Path=/") && set.contains("SameSite=Strict") && !set.to_ascii_lowercase().contains("domain"), "{set}");
    let cookie = set.split(';').next().unwrap().to_string();
    let (status, h, page) = on(&app, ADMIN_HOST, "/admin?v=split&p=/", Some(&cookie), None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    // The admin may frame the live site's origin; 线上 is the site itself.
    let policy = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(policy.contains(&format!("frame-src 'self' {SITE};")) && policy.contains("frame-ancestors 'self'"), "{policy}");
    assert_eq!(h[header::X_FRAME_OPTIONS], "SAMEORIGIN");
    assert_eq!(frame_src(&page, "live"), format!("{SITE}/"));
    // Pressed again, the side-by-side button closes it: it leads back to the draft alone.
    let button = page.split("aria-label=\"关闭并排对比\"").next().unwrap().rsplit("href=\"").next().unwrap();
    assert!(button.starts_with("/admin?") && !button.contains("v=split"), "{button}");
    let draft = frame_src(&page, "draft");
    assert!(draft.starts_with("/_hc/preview/") && draft.ends_with("/"), "{draft}");
    // The preview is the admin's: on its host only.
    assert_eq!(on(&app, ADMIN_HOST, &draft, None, None, &[]).await.0, StatusCode::OK);
    assert_eq!(on(&app, SITE_HOST, &draft, None, None, &[]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(on(&app, ADMIN_HOST, "/_hc/preview.js", None, None, &[]).await.0, StatusCode::OK);
    // Admin posts carry the admin's origin.
    let csrf = { let mut m = HeaderMap::new(); m.insert(header::COOKIE, cookie.parse().unwrap()); cms_harness::web::session::from_cookie(&app.conn(), &m).unwrap().csrf };
    let (status, _, _) = on(&app, ADMIN_HOST, "/admin/prefs", Some(&cookie), Some(&[("nav", "collapsed"), ("csrf", &csrf)]), &[("origin", ADMIN)]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    // Settings name each address for what it is, and say nothing about the shared-address risk.
    let (_, _, page) = on(&app, ADMIN_HOST, "/admin/settings", Some(&cookie), None, &[]).await;
    let page = page.replace("&#x2f;", "/");
    assert!(page.contains(&format!("value=\"{ADMIN}/mcp\"")) && page.contains(&format!("value=\"{SITE}/api/v1/content/&lt;")), "{page}");
    assert!(!page.contains(WARNING), "{page}");
    // The tools API is the admin's too.
    assert_eq!(on(&app, ADMIN_HOST, "/api/v1/tools", None, None, &[]).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switching_checks_the_new_address_first_and_the_old_one_redirects_for_a_while() {
    let app = app();
    let s = admin_session(&app, "builder").await;
    let form = |mode: &'static str, url: String| move |csrf: &str| vec![("mode", mode.to_string()), ("admin_origin", url.clone()), ("csrf", csrf.to_string())];
    let post = |fields: Vec<(&'static str, String)>, host: &'static str, origin: &'static str| {
        let app = app.clone();
        let cookie = s.0.clone();
        async move {
            let f: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let (status, h, body) = on(&app, host, "/admin/settings/address", Some(&cookie), Some(&f), &[("origin", origin)]).await;
            (status, h, body.replace("&#x2f;", "/"))
        }
    };

    // Nothing answers there: refused, nothing saved, and the page says what to add.
    let (status, _, body) = post(form("separate", "http://127.0.0.1:9".into())(&s.1), "", SITE).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("没能") && body.contains("127.0.0.1:9") && body.contains("域名解析") && body.contains("Host"), "{body}");
    assert_eq!(address::current(&app), None);
    for bad in ["ftp://admin.test", "https://admin.test/path", "admin.test", SITE, "https://adm in.test"] {
        let (status, _, body) = post(form("separate", bad.into())(&s.1), "", SITE).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert_eq!(address::current(&app), None, "{bad}");
    }
    let (status, _, _) = post(form("separate", ADMIN.into())("wrong"), "", SITE).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let e = admin_session(&app, "editor").await;
    let f = [("mode", "separate"), ("admin_origin", ADMIN), ("csrf", e.1.as_str())];
    assert_eq!(on(&app, "", "/admin/settings/address", Some(&e.0), Some(&f), &[]).await.0, StatusCode::FORBIDDEN);

    // A real listener on another port: this server, reached by another address.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let served = app.clone();
    tokio::spawn(async move { axum::serve(listener, cms_harness::api::router(served)).await.unwrap() });
    let new = format!("http://127.0.0.1:{port}");
    let new_host: &'static str = Box::leak(format!("127.0.0.1:{port}").into_boxed_str());
    let new_origin: &'static str = Box::leak(new.clone().into_boxed_str());
    let (status, _, body) = post(form("separate", format!("{new}/"))(&s.1), "", SITE).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("已切换到分开网址") && body.contains(&format!("href=\"{new}/admin/login\"")) && body.contains("10 分钟"), "{body}");
    assert_eq!(address::current(&app).as_deref(), Some(new.as_str()));
    // The ping's nonce was good once.
    assert_eq!(on(&app, new_host, "/_hc/ping", None, None, &[]).await.0, StatusCode::NOT_FOUND);

    // For a while the old address sends the admin to the new one; it takes no admin POSTs (a site script could
    // hold an old CSRF token).
    let (status, h, _) = on(&app, "", "/admin/settings?x=1", Some(&s.0), None, &[]).await;
    assert_eq!((status, location(&h)), (StatusCode::SEE_OTHER, format!("{new}/admin/settings?x=1")));
    let (status, _, body) = on(&app, "", "/admin/prefs", Some(&s.0), Some(&[("nav", "open"), ("csrf", &s.1)]), &[("origin", SITE)]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains(&new), "{body}");
    assert_eq!(on(&app, new_host, "/admin/login", None, None, &[]).await.0, StatusCode::OK);
    // Then it is gone.
    *app.old_admin_until.lock().unwrap() = Some(Instant::now() - Duration::from_secs(1));
    assert_eq!(on(&app, "", "/admin/settings", Some(&s.0), None, &[]).await.0, StatusCode::NOT_FOUND);

    // Back to one address, from the admin's host; no check is needed for the site's own address.
    let (status, _, body) = post(form("same", String::new())(&s.1), new_host, new_origin).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("已切回同一个网址") && body.contains(&format!("href=\"{SITE}/admin/login\"")), "{body}");
    assert_eq!(address::current(&app), None);
    assert_eq!(on(&app, "", "/admin/settings", Some(&s.0), None, &[]).await.0, StatusCode::OK);
}

#[tokio::test]
async fn the_environment_overrides_the_saved_choice() {
    let app = app();
    let s = admin_session(&app, "builder").await;
    app.env_admin_origin.set(Some(ADMIN.into())).unwrap();
    assert_eq!(address::current(&app).as_deref(), Some(ADMIN));
    assert_eq!(on(&app, SITE_HOST, "/admin/login", None, None, &[]).await.0, StatusCode::NOT_FOUND);
    let (_, _, page) = on(&app, ADMIN_HOST, "/admin/settings", Some(&s.0), None, &[]).await;
    assert!(page.contains("HC_ADMIN_ORIGIN") && !page.contains("action=\"/admin/settings/address\""), "{page}");
    let f = [("mode", "same"), ("admin_origin", ""), ("csrf", s.1.as_str())];
    let (status, _, body) = on(&app, ADMIN_HOST, "/admin/settings/address", Some(&s.0), Some(&f), &[("origin", ADMIN)]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("HC_ADMIN_ORIGIN"), "{body}");
    assert_eq!(address::current(&app).as_deref(), Some(ADMIN));

    // `off` forces one address over a saved 分开.
    let other = common::app();
    address::save(&other.conn(), Some(ADMIN)).unwrap();
    other.env_admin_origin.set(None).unwrap();
    assert_eq!(address::current(&other), None);
    assert_eq!(on(&other, SITE_HOST, "/admin/login", None, None, &[]).await.0, StatusCode::OK);
}

#[tokio::test]
async fn the_cli_recovers_a_locked_out_admin_without_a_restart() {
    let dir = std::env::temp_dir().join(format!("cms-harness-test-cli-{:016x}", rand::Rng::gen::<u64>(&mut rand::thread_rng())));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_cms-harness")).args(args).env("HC_DATA", &dir).env_remove("HC_DB").env_remove("HC_ORIGIN").output().unwrap()
    };
    let out = cli(&["admin-origin", "https://admin.example.com/"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("https://admin.example.com/admin"), "{}", String::from_utf8_lossy(&out.stdout));
    // The running server reads the choice from the database, so the CLI applies at once.
    let db = dir.join("harness.db");
    let app = cms_harness::App::new(cms_harness::store::open(db.to_str().unwrap()).unwrap(), dir.clone(), SITE.into(), "站".into());
    assert_eq!(address::current(&app).as_deref(), Some("https://admin.example.com"));
    assert_eq!(on(&app, SITE_HOST, "/admin/login", None, None, &[]).await.0, StatusCode::NOT_FOUND);

    let out = cli(&["admin-origin", "off"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("同一个网址"), "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(address::current(&app), None);
    assert_eq!(on(&app, SITE_HOST, "/admin/login", None, None, &[]).await.0, StatusCode::OK);

    for bad in ["admin.example.com", "https://admin.example.com/x"] {
        let out = cli(&["admin-origin", bad]);
        assert!(!out.status.success(), "{bad}");
    }
    assert!(!cli(&["admin-origin"]).status.success());
    assert_eq!(address::current(&app), None);
}
