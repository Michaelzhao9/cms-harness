//! 防护: the kernel guards everything the environment serves. Rate limits, crawlers, probes and the
//! proof-of-work challenge; what was stopped is on record for the admin.

mod common;

use axum::http::{header, Request, StatusCode};
use cms_harness::shield::{self, Config, Mode, Shield, Verdict};
use common::*;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use tower::ServiceExt;

const CHROME: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";

/// A visitor's GET of `path`.
fn get(s: &Shield, client: &str, path: &str, ua: &str, at: Instant) -> Verdict {
    s.check(client, "GET", path, ua, false, at)
}

/// The nonce a browser would find for a challenge.
fn solve(seed: &str) -> String {
    (0u64..).map(|n| n.to_string()).find(|n| shield::leading_zero_bits(&Sha256::digest(format!("{seed}.{n}"))) >= shield::BITS).unwrap()
}

#[test]
fn a_client_gets_so_many_requests_a_minute_and_a_fresh_count_the_next() {
    let (s, t) = (Shield::default(), Instant::now());
    let limit = Config::default().per_minute;
    for i in 0..limit {
        assert_eq!(get(&s, "1.1.1.1", "/api/v1/content/news", CHROME, t), Verdict::Pass, "request {i}");
    }
    assert_eq!(get(&s, "1.1.1.1", "/api/v1/content/news", CHROME, t), Verdict::Limited);
    assert_eq!(get(&s, "2.2.2.2", "/api/v1/content/news", CHROME, t), Verdict::Pass, "each client has its own count");
    assert_eq!(get(&s, "1.1.1.1", "/api/v1/content/news", CHROME, t + Duration::from_secs(61)), Verdict::Pass);
}

#[test]
fn a_client_that_keeps_going_over_is_shut_out_for_a_while() {
    let (s, mut t) = (Shield::default(), Instant::now());
    let limit = Config::default().per_minute;
    for _minute in 0..3 {
        for _ in 0..=limit {
            get(&s, "1.1.1.1", "/api/v1/content/news", CHROME, t);
        }
        t += Duration::from_secs(61);
    }
    assert!(matches!(get(&s, "1.1.1.1", "/api/v1/content/news", CHROME, t), Verdict::Blocked(_)), "three minutes over the limit");
    assert_eq!(get(&s, "1.1.1.1", "/api/v1/content/news", CHROME, t + Duration::from_secs(11 * 60)), Verdict::Pass, "and let back in later");
}

#[test]
fn asking_for_a_scanners_path_shuts_the_client_out_at_once() {
    for probe in ["/wp-login.php", "/.env", "/.git/config", "/phpmyadmin/index.php", "/cgi-bin/luci", "/vendor/phpunit/phpunit/src/Util/PHP/eval-stdin.php"] {
        let (s, t) = (Shield::default(), Instant::now());
        assert!(matches!(get(&s, "1.1.1.1", probe, CHROME, t), Verdict::Blocked(_)), "{probe}");
        assert!(matches!(get(&s, "1.1.1.1", "/", CHROME, t), Verdict::Blocked(_)), "{probe}: the next page too");
        assert_eq!(get(&s, "2.2.2.2", "/", CHROME, t), Verdict::Pass);
    }
}

#[test]
fn declared_crawlers_are_sorted_search_engines_in_ai_crawlers_and_tools_out() {
    let (s, t) = (Shield::default(), Instant::now());
    for ua in ["Mozilla/5.0 (compatible; GPTBot/1.2; +https://openai.com/gptbot)", "Mozilla/5.0 (compatible; ClaudeBot/1.0)", "CCBot/2.0", "Mozilla/5.0 (compatible; Bytespider)"] {
        assert!(matches!(get(&s, ua, "/news/first", ua, t), Verdict::Blocked(_)), "{ua}");
    }
    for ua in ["python-requests/2.32", "Scrapy/2.11 (+https://scrapy.org)", "curl/8.4.0", "Mozilla/5.0 HeadlessChrome/130.0"] {
        assert!(matches!(get(&s, ua, "/news/first", ua, t), Verdict::Blocked(_)), "{ua} on a page");
        assert_eq!(get(&s, ua, "/api/v1/content/news", ua, t), Verdict::Pass, "{ua}: the API is for programs");
    }
    for ua in ["Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)", "Mozilla/5.0 (compatible; Baiduspider/2.0)", "Mozilla/5.0 (compatible; bingbot/2.0)"] {
        assert_eq!(get(&s, ua, "/news/first", ua, t), Verdict::Pass, "{ua}");
    }
}

#[test]
fn reading_page_after_page_brings_the_challenge_and_solving_it_lets_the_client_through() {
    let (s, t) = (Shield::default(), Instant::now());
    let pages = Config::default().pages_per_minute;
    for i in 0..pages {
        assert_eq!(get(&s, "1.1.1.1", &format!("/news/{i}"), CHROME, t), Verdict::Pass, "page {i}");
    }
    assert_eq!(get(&s, "1.1.1.1", "/news/more", CHROME, t), Verdict::Challenge);
    assert_eq!(get(&s, "1.1.1.1", "/style.css", CHROME, t), Verdict::Pass, "a page's own files are not pages");
    assert_eq!(s.check("1.1.1.1", "GET", "/news/more", CHROME, true, t), Verdict::Pass, "with the challenge solved");
    let google = "Mozilla/5.0 (compatible; Googlebot/2.1)";
    for i in 0..pages + 5 {
        assert_eq!(get(&s, "66.249.1.1", &format!("/news/{i}"), google, t), Verdict::Pass, "search engines are not challenged");
    }
}

#[test]
fn a_solved_challenge_is_good_for_that_client_until_it_expires() {
    let s = Shield::default();
    let seed = s.issue("1.1.1.1", CHROME, 1_000);
    let pass = format!("{seed}.{}", solve(&seed));
    assert!(s.passes(&pass, "1.1.1.1", CHROME, 1_000 + 3600));
    assert!(!s.passes(&pass, "2.2.2.2", CHROME, 1_000 + 3600), "another client cannot use it");
    assert!(!s.passes(&pass, "1.1.1.1", "curl/8", 1_000 + 3600), "nor another browser");
    assert!(!s.passes(&pass, "1.1.1.1", CHROME, 1_000 + shield::PASS_SECONDS + 1), "expired");
    assert!(!s.passes(&format!("{seed}.0"), "1.1.1.1", CHROME, 1_000 + 3600), "the work was not done");
    let forged = format!("9{}", &pass[1..]);
    assert!(!s.passes(&forged, "1.1.1.1", CHROME, 1_000 + 3600), "a seed we did not sign");
    assert!(!Shield::default().passes(&pass, "1.1.1.1", CHROME, 1_000 + 3600), "another server's seed");
    assert!(!s.passes("", "1.1.1.1", CHROME, 1_000) && !s.passes("a.b", "1.1.1.1", CHROME, 1_000));
}

#[test]
fn the_modes_and_the_block_list() {
    let t = Instant::now();
    let off = Shield::default();
    off.set_config(Config { mode: Mode::Off, ..Config::default() });
    assert_eq!(get(&off, "1.1.1.1", "/.env", "GPTBot", t), Verdict::Pass, "off stops nothing");

    let strict = Shield::default();
    strict.set_config(Config { mode: Mode::Strict, ..Config::default() });
    assert_eq!(get(&strict, "1.1.1.1", "/", CHROME, t), Verdict::Challenge, "strict: every visitor proves they run a browser");
    assert_eq!(get(&strict, "1.1.1.1", "/api/v1/content/news", CHROME, t), Verdict::Pass);
    assert_eq!(get(&strict, "1.1.1.1", "/admin/login", CHROME, t), Verdict::Pass, "the admin stays reachable");

    let listed = Shield::default();
    listed.set_config(Config { blocked: vec!["9.9.9.9".into()], ..Config::default() });
    assert!(matches!(get(&listed, "9.9.9.9", "/", CHROME, t), Verdict::Blocked(_)));
    assert_eq!(get(&listed, "9.9.9.9", "/admin/login", CHROME, t), Verdict::Pass, "a blocked admin is not locked out of the admin");
    assert_eq!(get(&listed, "9.9.9.8", "/", CHROME, t), Verdict::Pass);
}

async fn fetch(app: &cms_harness::Shared, path: &str, ua: &str) -> (StatusCode, String) {
    let req = Request::get(path).header(header::USER_AGENT, ua).body(axum::body::Body::empty()).unwrap();
    let resp = cms_harness::api::router(app.clone()).oneshot(req).await.unwrap();
    let s = resp.status();
    let b = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
    (s, String::from_utf8_lossy(&b).into_owned())
}

#[tokio::test]
async fn the_site_refuses_ai_crawlers_and_says_so_in_robots_txt() {
    let app = app();
    assert_eq!(fetch(&app, "/", CHROME).await.0, StatusCode::OK);
    assert_eq!(fetch(&app, "/", "Mozilla/5.0 (compatible; GPTBot/1.2)").await.0, StatusCode::FORBIDDEN);
    let robots = fetch(&app, "/robots.txt", CHROME).await.1;
    assert!(robots.contains("User-agent: GPTBot\nDisallow: /"), "{robots}");
    assert!(robots.contains("User-agent: *\nAllow: /"), "everyone else is still welcome: {robots}");
}

#[tokio::test]
async fn a_probe_is_refused_and_recorded_and_the_admin_shows_nothing_of_it() {
    let app = app();
    assert_eq!(fetch(&app, "/wp-login.php", CHROME).await.0, StatusCode::FORBIDDEN);
    assert_eq!(fetch(&app, "/", CHROME).await.0, StatusCode::FORBIDDEN, "the same client's next request");
    let events = shield::events(&app.conn(), 10).unwrap();
    assert_eq!((events[0]["kind"].as_str(), events[0]["path"].as_str()), (Some("probe"), Some("/wp-login.php")), "{events:?}");

    // Protection is not something the admin shows or sets: it is on, as the code says.
    let session = admin_session(&app, "builder").await;
    let cookie = session.0.clone();
    assert_eq!(get_with(&app, "/admin/shield", Some(&cookie)).await.0, StatusCode::NOT_FOUND);
    assert!(post_form(&app, "/admin/shield", Some(&session), &[("mode", "off")]).await.0.is_client_error());
    for path in ["/admin", "/admin/settings"] {
        let (s, _, page) = get_with(&app, path, Some(&cookie)).await;
        assert_eq!(s, StatusCode::OK, "the admin is not shut out with the visitor");
        for word in ["防护", "反爬", "爬虫", "封禁", "/admin/shield"] {
            assert!(!page.contains(word), "{path} mentions {word}");
        }
    }
}

#[tokio::test]
async fn strict_mode_serves_the_challenge_and_a_solved_one_lets_the_visitor_in() {
    let app = app();
    assert_eq!(fetch(&app, "/", CHROME).await.0, StatusCode::OK);
    // Only code changes the level.
    app.shield.set_config(Config { mode: Mode::Strict, ..Config::default() });

    let (s, page) = fetch(&app, "/", CHROME).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(page.contains("/_hc/shield.js") && page.contains("data-seed="), "{page}");
    let (s, js) = fetch(&app, "/_hc/shield.js", CHROME).await;
    assert_eq!(s, StatusCode::OK);
    assert!(js.contains("hc_pass"));

    // What the page's script does: find the nonce, keep it in a cookie, ask again.
    let seed = page.split("data-seed=\"").nth(1).unwrap().split('"').next().unwrap().to_string();
    let req = Request::get("/").header(header::USER_AGENT, CHROME).header(header::COOKIE, format!("hc_pass={seed}.{}", solve(&seed))).body(axum::body::Body::empty()).unwrap();
    assert_eq!(cms_harness::api::router(app.clone()).oneshot(req).await.unwrap().status(), StatusCode::OK);
}

#[test]
fn the_level_out_of_the_box_is_the_full_one() {
    let c = Config::default();
    assert_eq!(c.mode, Mode::Standard);
    assert!(c.block_ai && c.block_tools && c.per_minute <= 240 && c.pages_per_minute <= 40);
    // Nothing saved in the database turns it down.
    let app = app();
    app.conn().execute("INSERT INTO site_settings(key, value) VALUES ('shield', '{\"mode\":\"off\"}')", []).unwrap();
    let again = cms_harness::App::new(cms_harness::store::open(":memory:").unwrap(), std::env::temp_dir(), "http://127.0.0.1:4200".into(), "x".into());
    assert_eq!(again.shield.config().mode, Mode::Standard);
}
