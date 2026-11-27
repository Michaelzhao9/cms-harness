//! Server functions: the back end the harness writes. A site file under `_fn/` is a script the server runs when
//! its address is asked for: sandboxed, inside one transaction, a draft until published like everything else.

mod common;

use axum::http::{header, Method, Request, StatusCode};
use common::*;
use cms_harness::Shared;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn file(app: &Shared, path: &str, content: &str) {
    ok(app, &builder(), "write_site_file", json!({"path": path, "content": content, "note": "写函数"}));
}

async fn call(app: &Shared, method: Method, path: &str, body: Option<Value>, headers: &[(&str, &str)]) -> (StatusCode, Value) {
    let mut r = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    let body = match body {
        Some(b) => {
            r = r.header(header::CONTENT_TYPE, "application/json");
            axum::body::Body::from(b.to_string())
        }
        None => axum::body::Body::empty(),
    };
    let resp = cms_harness::api::router(app.clone()).oneshot(r.body(body).unwrap()).await.unwrap();
    let s = resp.status();
    let b = resp.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&b).unwrap_or_else(|_| json!(String::from_utf8_lossy(&b))))
}

async fn get_fn(app: &Shared, path: &str) -> (StatusCode, Value) {
    call(app, Method::GET, path, None, &[]).await
}

async fn post_fn(app: &Shared, path: &str, body: Value) -> (StatusCode, Value) {
    call(app, Method::POST, path, Some(body), &[]).await
}

/// A live `notes` collection (text, stars) with no way in from the site: only functions write to it.
fn notes(app: &Shared) {
    ok(app, &builder(), "create_collection", json!({"name": "notes", "title": "留言", "read": "public", "note": "x",
        "fields": [{"name": "text", "title": "内容", "type": "text", "required": true}, {"name": "stars", "title": "评分", "type": "number"}]}));
    publish_all(app);
}

fn live_notes(app: &Shared) -> Vec<String> {
    let found = ok(app, &reader(), "query_records", json!({"collection": "notes", "status": ["published"], "sort": "id"}));
    found["items"].as_array().unwrap().iter().map(|i| i["data"]["text"].as_str().unwrap().to_string()).collect()
}

const ADD: &str = "export default function (req) {\n  const id = db.create('notes', { text: req.body.text, stars: 5 });\n  const all = db.query('notes', { sort: '-id', limit: 10 });\n  return { id, total: all.total, first: all.items[0].text, stars: db.get('notes', id).stars };\n}";

#[tokio::test]
async fn a_function_answers_at_its_address_once_it_is_published() {
    let app = app();
    file(&app, "_fn/hello.js", "export default function (req) { return { hello: req.query.name || 'world', method: req.method, path: req.path } }");
    assert_eq!(get_fn(&app, "/_fn/hello").await.0, StatusCode::NOT_FOUND, "a draft is not live");
    publish_all(&app);
    let (s, v) = get_fn(&app, "/_fn/hello?name=%E5%B0%8F%E7%8E%8B").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"hello": "小王", "method": "GET", "path": "/_fn/hello"}));
    assert_eq!(get_fn(&app, "/_fn/nope").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_source_of_a_function_is_never_served() {
    let app = app();
    file(&app, "_fn/hello.js", "export default () => 'secret logic'");
    file(&app, "shop/_fn/price.js", "export default () => 1");
    file(&app, "app.js", "console.log('a page script')");
    publish_all(&app);
    for path in ["/_fn/hello.js", "/shop/_fn/price.js"] {
        let (s, v) = get_fn(&app, path).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{path}: {v}");
        assert!(!v.to_string().contains("secret"), "{v}");
    }
    assert_eq!(get_fn(&app, "/app.js").await.0, StatusCode::OK, "ordinary scripts are still served");
    assert_eq!(ok(&app, &reader(), "check_page", json!({"path": "/_fn/hello.js"}))["status"], 404, "nor in the preview");
}

#[tokio::test]
async fn a_function_reads_and_writes_the_environments_data() {
    let app = app();
    notes(&app);
    file(&app, "_fn/add.js", ADD);
    file(&app, "_fn/edit.js", "export default function (req) {\n  db.update('notes', req.body.id, { stars: req.body.stars });\n  if (req.body.remove) db.delete('notes', req.body.remove);\n  return db.query('notes', { filter: {}, sort: 'id' }).items.map(n => [n.id, n.text, n.stars]);\n}");
    publish_all(&app);

    let (s, v) = post_fn(&app, "/_fn/add", json!({"text": "来自函数"})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!((v["total"].as_i64(), v["first"].as_str(), v["stars"].as_i64()), (Some(1), Some("来自函数"), Some(5)));
    let id = v["id"].as_i64().unwrap();
    assert_eq!(live_notes(&app), vec!["来自函数"], "live at once, like a visitor's submission");
    let actor: String = app.conn().query_row("SELECT actor FROM versions WHERE target = ?1", [format!("record:{id}")], |r| r.get(0)).unwrap();
    assert_eq!(actor, "fn:_fn/add", "history says which function wrote it");

    let second = post_fn(&app, "/_fn/add", json!({"text": "第二条"})).await.1["id"].as_i64().unwrap();
    let (s, v) = post_fn(&app, "/_fn/edit", json!({"id": id, "stars": 1, "remove": second})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v, json!([[id, "来自函数", 1]]));
}

#[tokio::test]
async fn a_run_that_fails_or_refuses_leaves_nothing_behind() {
    let app = app();
    notes(&app);
    file(&app, "_fn/throws.js", "export default function (req) { db.create('notes', { text: '写了一半' }); throw new Error('后面出错了'); }");
    file(&app, "_fn/bad.js", "export default function (req) { db.create('notes', { text: '先写一条' }); db.create('notes', { nope: 1 }); }");
    file(&app, "_fn/refuse.js", "export default function (req) { db.create('notes', { text: '也写了' }); return reply(403, { error: '不行' }); }");
    file(&app, "_fn/created.js", "export default function (req) { const id = db.create('notes', { text: '成了' }); return reply(201, { id }); }");
    publish_all(&app);

    let (s, v) = post_fn(&app, "/_fn/throws", json!({})).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!v.to_string().contains("后面出错了"), "a visitor does not see the script's own error: {v}");
    let (s, _) = post_fn(&app, "/_fn/bad", json!({})).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    let (s, v) = post_fn(&app, "/_fn/refuse", json!({})).await;
    assert_eq!((s, &v), (StatusCode::FORBIDDEN, &json!({"error": "不行"})));
    assert!(live_notes(&app).is_empty(), "nothing any of the three wrote is kept: {:?}", live_notes(&app));

    let (s, v) = post_fn(&app, "/_fn/created", json!({})).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(live_notes(&app), vec!["成了"]);

    // What went wrong is on record for whoever builds, with the script's own words.
    let log = ok(&app, &reader(), "function_log", json!({})).to_string();
    assert!(log.contains("后面出错了") && log.contains("_fn/throws") && log.contains("nope"), "{log}");
}

#[tokio::test]
async fn a_function_that_runs_away_is_stopped_and_the_server_goes_on() {
    let app = app();
    notes(&app);
    file(&app, "_fn/spin.js", "export default function () { db.create('notes', { text: '转之前' }); while (true) {} }");
    file(&app, "_fn/grow.js", "export default function () { const a = []; for (;;) a.push('x'.repeat(1 << 20)); }");
    file(&app, "_fn/deep.js", "export default function () { const f = () => f(); return f(); }");
    file(&app, "_fn/regex.js", "export default function () { return /^(a+)+$/.test('a'.repeat(40) + 'b'); }");
    file(&app, "_fn/flood.js", "export default function () { for (let i = 0; i < 100000; i++) db.query('notes', {}); return 1; }");
    file(&app, "_fn/out.js", "export default function () { return [typeof fetch, typeof require, typeof process, typeof Deno, typeof XMLHttpRequest, typeof WebSocket] }");
    publish_all(&app);
    let started = std::time::Instant::now();
    for path in ["/_fn/spin", "/_fn/grow", "/_fn/deep", "/_fn/regex", "/_fn/flood"] {
        let (s, v) = get_fn(&app, path).await;
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{path}: {v}");
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(8), "each is cut off, not waited out: {:?}", started.elapsed());
    assert!(live_notes(&app).is_empty());
    assert_eq!(get_fn(&app, "/_fn/out").await.1, json!(["undefined", "undefined", "undefined", "undefined", "undefined", "undefined"]), "no way out of the box");
    let log = ok(&app, &reader(), "function_log", json!({"path": "/_fn/spin"})).to_string();
    assert!(log.contains("时间"), "the log says it ran out of time: {log}");
}

async fn member(app: &Shared, email: &str) -> String {
    let resp = cms_harness::api::router(app.clone())
        .oneshot(Request::post("/api/v1/members/signup").header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(json!({"email": email, "password": "long-enough-1"}).to_string())).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn a_function_knows_who_is_asking_and_sits_behind_its_ends_door() {
    let app = app();
    ok(&app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    ok(&app, &builder(), "set_end", json!({"name": "teacher", "title": "老师端", "enter": "members", "note": "建老师端"}));
    file(&app, "teacher/_fn/who.js", "export default (req) => ({ email: req.member.email })");
    file(&app, "_fn/who.js", "export default (req) => ({ member: req.member })");
    publish_all(&app);
    let m = member(&app, "t@example.com").await;

    let (s, v) = get_fn(&app, "/teacher/_fn/who").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "a visitor is not let into the end's functions either: {v}");
    let (s, v) = call(&app, Method::GET, "/teacher/_fn/who", None, &[("cookie", &m)]).await;
    assert_eq!((s, &v), (StatusCode::OK, &json!({"email": "t@example.com"})));
    assert_eq!(get_fn(&app, "/_fn/who").await.1, json!({"member": null}), "outside an end, a visitor is simply nobody");
}

#[tokio::test]
async fn posts_are_checked_like_every_other_write() {
    let app = app();
    file(&app, "_fn/echo.js", "export default async (req) => await Promise.resolve({ got: req.body, method: req.method })");
    publish_all(&app);
    let (s, v) = post_fn(&app, "/_fn/echo", json!({"a": [1, 2]})).await;
    assert_eq!((s, &v), (StatusCode::OK, &json!({"got": {"a": [1, 2]}, "method": "POST"})), "an async function works too");
    let form = Request::post("/_fn/echo").header(header::CONTENT_TYPE, "application/x-www-form-urlencoded").body(axum::body::Body::from("name=%E5%B0%8F%E7%8E%8B&n=1")).unwrap();
    let resp = cms_harness::api::router(app.clone()).oneshot(form).await.unwrap();
    let v: Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["got"], json!({"name": "小王", "n": "1"}), "a plain form arrives as its fields");

    let (s, _) = call(&app, Method::POST, "/_fn/echo", Some(json!({})), &[("origin", "https://evil.example")]).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "another site cannot post to a function");
    let bad = Request::post("/_fn/echo").header(header::CONTENT_TYPE, "application/json").body(axum::body::Body::from("{not json")).unwrap();
    assert_eq!(cms_harness::api::router(app.clone()).oneshot(bad).await.unwrap().status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_builder_tries_a_draft_function_without_touching_the_data() {
    let app = app();
    notes(&app);
    ok(&app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    publish_all(&app);
    let m = member(&app, "s@example.com").await;
    let _ = m;
    // Not published: only the tool can run it.
    file(&app, "_fn/add.js", &ADD.replace("const id", "console.log('收到', req.body.text, req.member && req.member.email);\n  const id"));
    let r = ok(&app, &reader(), "run_function", json!({"path": "/_fn/add", "method": "POST", "body": {"text": "试一下"}, "member_id": 1}));
    assert_eq!((r["status"].as_i64(), r["dry_run"].as_bool()), (Some(200), Some(true)), "{r}");
    assert_eq!((r["body"]["first"].as_str(), r["body"]["total"].as_i64()), (Some("试一下"), Some(1)), "inside the run, its own write is there");
    assert!(r["logs"].to_string().contains("收到") && r["logs"].to_string().contains("s@example.com"), "what it logged, and who it ran as: {r}");
    assert!(live_notes(&app).is_empty(), "and afterwards nothing was written");
    assert_eq!(post_fn(&app, "/_fn/add", json!({"text": "访客"})).await.0, StatusCode::NOT_FOUND, "still not live");

    file(&app, "_fn/broken.js", "export default function (req) { return req.body.missing.deeper; }");
    let r = ok(&app, &reader(), "run_function", json!({"path": "/_fn/broken"}));
    assert_eq!(r["status"], 500);
    let e = r["error"].as_str().unwrap();
    assert!(e.contains("missing") && e.contains("_fn/broken.js:1"), "the script's own error with where it happened, for the builder: {r}");
    let r = ok(&app, &reader(), "run_function", json!({"path": "/_fn/none"}));
    assert_eq!(r["status"], 404);
    assert!(err(&app, &reader(), "run_function", json!({"path": "_fn/add"})).contains('/'));
}

#[tokio::test]
async fn a_function_file_is_checked_when_it_is_written() {
    let app = app();
    let e = err(&app, &builder(), "write_site_file", json!({"path": "_fn/bad.js", "content": "export default function ( {", "note": "x"}));
    assert!(e.contains("_fn/bad.js"), "{e}");
    let e = err(&app, &builder(), "write_site_file", json!({"path": "_fn/page.html", "content": "<p>x</p>", "note": "x"}));
    assert!(e.contains("_fn"), "only scripts live under _fn: {e}");
}
