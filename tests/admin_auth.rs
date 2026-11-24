mod common;
use axum::http::{header, HeaderMap, StatusCode};
use common::*;
use cms_harness::{auth, store, web::session, App};

const ADMIN_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' https: data:; connect-src 'self'; frame-src 'self'; form-action 'self'; frame-ancestors 'self'; base-uri 'none'";

fn location(h: &HeaderMap) -> &str {
    h[header::LOCATION].to_str().unwrap()
}

async fn login(app: &cms_harness::Shared, email: &str, password: &str) -> (StatusCode, HeaderMap, String) {
    post_form(app, "/admin/login", None, &[("email", email), ("password", password)]).await
}

fn cookie_pair(h: &HeaderMap) -> String {
    h[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn login_sets_a_strict_http_only_cookie_and_redirects() {
    let app = app();
    auth::create_admin(&app.conn(), "a@test", "builder", Some("secret-pass")).unwrap();
    let (status, h, _) = login(&app, "a@test", "secret-pass").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&h), "/admin");
    let c = h[header::SET_COOKIE].to_str().unwrap();
    assert!(c.starts_with("hc_admin="), "{c}");
    assert!(c.contains("HttpOnly") && c.contains("SameSite=Strict") && c.contains("Path=/") && c.contains("Max-Age=2592000"), "{c}");
    assert!(!c.contains("Secure"), "{c}");
    // Only the hash of the token is stored.
    let token = cookie_pair(&h)["hc_admin=".len()..].to_string();
    assert_eq!(token.len(), 64);
    let stored: String = app.conn().query_row("SELECT token_hash FROM admin_sessions", [], |r| r.get(0)).unwrap();
    assert_ne!(stored, token);
    let (status, _, body) = get_with(&app, "/admin", Some(&cookie_pair(&h))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("aria-label=\"导航\""), "the admin shell: {body}");
}

#[tokio::test]
async fn https_origin_adds_secure_and_hsts() {
    let dir = std::env::temp_dir().join(format!("cms-harness-test-https-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let app = App::new(store::open(":memory:").unwrap(), dir, "https://cms.example".into(), "站".into());
    assert!(app.secure_cookies);
    auth::create_admin(&app.conn(), "a@test", "builder", Some("secret-pass")).unwrap();
    let (status, h, _) = login(&app, "a@test", "secret-pass").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(h[header::SET_COOKIE].to_str().unwrap().contains("; Secure"));
    assert_eq!(h[header::STRICT_TRANSPORT_SECURITY], "max-age=31536000");
    let (_, h, _) = get_with(&app, "/", None).await;
    assert_eq!(h[header::STRICT_TRANSPORT_SECURITY], "max-age=31536000");
    let (_, h, _) = get_with(&common::app(), "/", None).await;
    assert!(h.get(header::STRICT_TRANSPORT_SECURITY).is_none());
}

#[tokio::test]
async fn wrong_password_and_unknown_email_look_the_same() {
    let app = app();
    auth::create_admin(&app.conn(), "a@test", "builder", Some("secret-pass")).unwrap();
    let (s1, _, wrong) = login(&app, "a@test", "nope").await;
    let (s2, _, unknown) = login(&app, "nobody@test", "nope").await;
    assert_eq!((s1, s2), (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED));
    assert_eq!(wrong, unknown);
    assert!(wrong.contains("邮箱或密码不正确"), "{wrong}");
}

#[tokio::test]
async fn five_failures_lock_the_email_for_fifteen_minutes() {
    let app = app();
    auth::create_admin(&app.conn(), "a@test", "builder", Some("secret-pass")).unwrap();
    for _ in 0..5 {
        assert_eq!(login(&app, "a@test", "nope").await.0, StatusCode::UNAUTHORIZED);
    }
    let (status, h, body) = login(&app, "a@test", "secret-pass").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(h.get(header::SET_COOKIE).is_none());
    assert!(body.contains("15 分钟"), "{body}");
    // Attempts older than 15 minutes no longer count.
    app.conn().execute("UPDATE login_attempts SET at = datetime('now', '-16 minutes')", []).unwrap();
    assert_eq!(login(&app, "a@test", "secret-pass").await.0, StatusCode::SEE_OTHER);
}

#[test]
fn the_client_key_is_locked_too_and_success_clears_only_the_email() {
    let conn = store::open(":memory:").unwrap();
    auth::create_admin(&conn, "a@test", "builder", Some("secret-pass")).unwrap();
    auth::create_admin(&conn, "b@test", "builder", Some("secret-pass")).unwrap();
    for _ in 0..4 {
        session::login(&conn, "a@test", "nope", "1.2.3.4").unwrap_err();
    }
    session::login(&conn, "a@test", "secret-pass", "1.2.3.4").unwrap();
    let email_rows: i64 = conn.query_row("SELECT count(*) FROM login_attempts WHERE key = 'email:a@test'", [], |r| r.get(0)).unwrap();
    assert_eq!(email_rows, 0);
    session::login(&conn, "b@test", "nope", "1.2.3.4").unwrap_err();
    // Five failures from one client lock every account for that client, not for others.
    assert_eq!(session::login(&conn, "b@test", "secret-pass", "1.2.3.4").unwrap_err(), session::LOCKED);
    session::login(&conn, "b@test", "secret-pass", "5.6.7.8").unwrap();
    // Five failures for one email from five clients lock that email for a sixth client.
    for i in 0..5 {
        session::login(&conn, "b@test", "nope", &format!("10.0.0.{i}")).unwrap_err();
    }
    assert_eq!(session::login(&conn, "b@test", "secret-pass", "10.0.0.9").unwrap_err(), session::LOCKED);
    session::login(&conn, "a@test", "secret-pass", "10.0.0.9").unwrap();
}

#[tokio::test]
async fn admin_pages_need_a_session() {
    let app = app();
    let (status, h, _) = get_with(&app, "/admin", None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&h), "/admin/login");
    let (status, _, _) = get_with(&app, "/admin", Some("hc_admin=0000")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (status, _, _) = post_form(&app, "/admin/logout", None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Paths no route handles yet are gated too.
    assert_eq!(get_with(&app, "/admin/anything", None).await.0, StatusCode::SEE_OTHER);
    assert_eq!(post_form(&app, "/admin/anything", None, &[]).await.0, StatusCode::UNAUTHORIZED);
    // The login page and the stylesheet are open.
    assert_eq!(get_with(&app, "/admin/login", None).await.0, StatusCode::OK);
    let (status, h, css) = get_with(&app, "/admin/static/admin.css", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(h[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/css"));
    assert!(css.contains(".login"));
}

#[tokio::test]
async fn expired_sessions_are_absent_and_deleted() {
    let app = app();
    let s = admin_session(&app, "builder").await;
    app.conn().execute("UPDATE admin_sessions SET expires_at = datetime('now', '-1 minute')", []).unwrap();
    assert_eq!(get_with(&app, "/admin", Some(&s.0)).await.0, StatusCode::SEE_OTHER);
    let left: i64 = app.conn().query_row("SELECT count(*) FROM admin_sessions", [], |r| r.get(0)).unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_post_with_a_foreign_origin_is_refused() {
    let app = app();
    let s = admin_session(&app, "builder").await;
    let (status, _, _) = post_form_with(&app, "/admin/logout", Some(&s), &[], &[("origin", "https://evil.example")]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(get_with(&app, "/admin", Some(&s.0)).await.0, StatusCode::OK, "the session survives");
    // The login form is checked too.
    let (status, _, _) = post_form_with(&app, "/admin/login", None, &[("email", "builder@test"), ("password", "secret-pass")], &[("origin", "https://evil.example")]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The site's own origin passes.
    let (status, _, _) = post_form_with(&app, "/admin/logout", Some(&s), &[], &[("origin", "http://127.0.0.1:4200")]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn logout_needs_the_sessions_own_csrf_token() {
    let app = app();
    let a = admin_session(&app, "builder").await;
    let b = admin_session(&app, "editor").await;
    let mixed = (a.0.clone(), b.1.clone());
    assert_eq!(post_form(&app, "/admin/logout", Some(&mixed), &[]).await.0, StatusCode::FORBIDDEN);
    let no_token = (a.0.clone(), String::new());
    assert_eq!(post_form(&app, "/admin/logout", Some(&no_token), &[]).await.0, StatusCode::FORBIDDEN);
    assert_eq!(get_with(&app, "/admin", Some(&a.0)).await.0, StatusCode::OK);
}

#[tokio::test]
async fn logout_ends_the_session() {
    let app = app();
    let s = admin_session(&app, "builder").await;
    let (status, h, _) = post_form(&app, "/admin/logout", Some(&s), &[]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&h), "/admin/login");
    assert!(h[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=0"));
    let (status, h, _) = get_with(&app, "/admin", Some(&s.0)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&h), "/admin/login");
}

#[tokio::test]
async fn security_headers_are_on_every_response() {
    let app = app();
    for path in ["/", "/admin/login", "/api/v1/tools", "/admin", "/nowhere"] {
        let (_, h, _) = get_with(&app, path, None).await;
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
        assert_eq!(h[header::REFERRER_POLICY], "strict-origin-when-cross-origin", "{path}");
        assert_eq!(h[header::X_FRAME_OPTIONS], "SAMEORIGIN", "{path}");
        assert_eq!(h["permissions-policy"], "camera=(), microphone=(), geolocation=()", "{path}");
    }
    let (_, h, body) = get_with(&app, "/admin/login", None).await;
    assert_eq!(h[header::CONTENT_SECURITY_POLICY], ADMIN_CSP);
    assert!(!body.contains("<script") && !body.contains("<style") && !body.contains("style="), "{body}");
    // The admin CSP is only for the admin; the site has its own, which allows inline styles.
    let (_, h, _) = get_with(&app, "/", None).await;
    let site = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(site != ADMIN_CSP && site.contains("style-src 'self' 'unsafe-inline'"), "{site}");
}

#[tokio::test]
async fn emails_are_case_insensitive() {
    let app = app();
    auth::create_admin(&app.conn(), "  A@Example.com ", "builder", Some("secret-pass")).unwrap();
    let stored: String = app.conn().query_row("SELECT email FROM admins", [], |r| r.get(0)).unwrap();
    assert_eq!(stored, "a@example.com");
    assert_eq!(login(&app, "a@example.com", "secret-pass").await.0, StatusCode::SEE_OTHER);
    assert_eq!(login(&app, " A@EXAMPLE.COM", "secret-pass").await.0, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn old_mixed_case_admins_still_log_in_and_reset() {
    let app = app();
    app.conn().execute("INSERT INTO admins(email, password_hash, role) VALUES('Old@Example.com', ?1, 'builder')", [auth::hash_password("old-pass")]).unwrap();
    let (_, h, _) = login(&app, "old@example.com", "old-pass").await;
    let old_session = cookie_pair(&h);
    assert_eq!(get_with(&app, "/admin", Some(&old_session)).await.0, StatusCode::OK);
    // Resetting the password updates that row instead of adding a second admin.
    auth::create_admin(&app.conn(), "OLD@example.com", "editor", Some("new-pass")).unwrap();
    let count: i64 = app.conn().query_row("SELECT count(*) FROM admins", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
    assert_eq!(get_with(&app, "/admin", Some(&old_session)).await.0, StatusCode::SEE_OTHER, "a reset ends the sessions");
    assert_eq!(login(&app, "old@example.com", "old-pass").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(login(&app, "old@example.com", "new-pass").await.0, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn changing_the_password_ends_other_sessions() {
    let app = app();
    let kept = admin_session(&app, "builder").await;
    let (_, h, _) = login(&app, "builder@test", "secret-pass").await;
    let other = cookie_pair(&h);
    let user = {
        let mut hm = HeaderMap::new();
        hm.insert(header::COOKIE, kept.0.parse().unwrap());
        session::from_cookie(&app.conn(), &hm).unwrap()
    };
    assert_eq!(user.actor().name, "builder@test");
    assert!(user.actor().admin);
    assert!(session::change_password(&app.conn(), user.id, "wrong", "brand-new-pass", &user.token_hash).is_err());
    assert!(session::change_password(&app.conn(), user.id, "secret-pass", "short", &user.token_hash).is_err());
    assert_eq!(get_with(&app, "/admin", Some(&other)).await.0, StatusCode::OK, "a failed change ends nothing");
    session::change_password(&app.conn(), user.id, "secret-pass", "brand-new-pass", &user.token_hash).unwrap();
    assert_eq!(get_with(&app, "/admin", Some(&other)).await.0, StatusCode::SEE_OTHER);
    assert_eq!(get_with(&app, "/admin", Some(&kept.0)).await.0, StatusCode::OK);
    assert_eq!(login(&app, "builder@test", "secret-pass").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(login(&app, "builder@test", "brand-new-pass").await.0, StatusCode::SEE_OTHER);
}

#[test]
fn csrf_check_compares_the_whole_token() {
    let conn = store::open(":memory:").unwrap();
    auth::create_admin(&conn, "a@test", "builder", Some("secret-pass")).unwrap();
    let token = session::login(&conn, "a@test", "secret-pass", "unknown").unwrap();
    let mut h = HeaderMap::new();
    h.insert(header::COOKIE, format!("theme=x; hc_admin={token}").parse().unwrap());
    let user = session::from_cookie(&conn, &h).unwrap();
    assert!(session::check_csrf(&user, &user.csrf).is_ok());
    assert_eq!(session::check_csrf(&user, &user.csrf[..63]), Err(StatusCode::FORBIDDEN));
    assert_eq!(session::check_csrf(&user, ""), Err(StatusCode::FORBIDDEN));
    session::logout(&conn, &user.token_hash);
    assert!(session::from_cookie(&conn, &h).is_none());
}

#[test]
fn forwarded_for_is_only_trusted_behind_a_proxy() {
    let peer: Option<std::net::SocketAddr> = Some("10.0.0.1:5555".parse().unwrap());
    let mut h = HeaderMap::new();
    h.insert("x-forwarded-for", "6.6.6.6, 9.9.9.9".parse().unwrap());
    assert_eq!(cms_harness::web::client_key(&h, peer, false), "10.0.0.1");
    assert_eq!(cms_harness::web::client_key(&h, peer, true), "9.9.9.9");
    assert_eq!(cms_harness::web::client_key(&HeaderMap::new(), peer, true), "10.0.0.1");
    assert_eq!(cms_harness::web::client_key(&HeaderMap::new(), None, false), "unknown");
}
