mod common;
use axum::http::{header, HeaderMap, StatusCode};
use common::*;
use cms_harness::auth::{self, Scope};
use cms_harness::changes::{self, Target};
use cms_harness::web::{admin, session};
use cms_harness::Shared;
use serde_json::{json, Value};

/// Every admin page: no scripts, no inline styles the CSP would drop.
fn assert_clean(page: &str) {
    assert_only_admin_js(page);
}

fn location(h: &HeaderMap) -> &str {
    h[header::LOCATION].to_str().unwrap()
}

fn count(app: &Shared, sql: &str) -> i64 {
    app.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn create(app: &Shared, data: Value) -> i64 {
    ok(app, &editor(), "create_record", json!({ "collection": "news", "note": "新建", "data": data }))["id"].as_i64().unwrap()
}

fn status_of(app: &Shared, id: i64) -> String {
    ok(app, &reader(), "get_record", json!({ "id": id }))["status"].as_str().unwrap().to_string()
}

/// Everything a dry run could touch, as text: data tables, history, and the index list.
fn snapshot(app: &Shared) -> String {
    let conn = app.conn();
    let mut out = String::new();
    for sql in [
        "SELECT id, collection, published, draft, draft_deleted, draft_actor, draft_note, updated_at FROM records ORDER BY id",
        "SELECT name, published, draft, draft_deleted, deleted FROM collections ORDER BY name",
        "SELECT id, actor, note FROM releases ORDER BY id",
        "SELECT id, release_id, target, doc, prev FROM versions ORDER BY id",
        "SELECT name, coalesce(sql, '') FROM sqlite_master ORDER BY name",
    ] {
        let mut stmt = conn.prepare(sql).unwrap();
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| Ok((0..n).map(|i| format!("{:?}", r.get_ref(i).unwrap())).collect::<Vec<_>>().join("|")))
            .unwrap();
        for row in rows {
            out.push_str(&row.unwrap());
            out.push('\n');
        }
    }
    out
}

fn user_of(app: &Shared, cookie: &str) -> session::AdminUser {
    let mut h = HeaderMap::new();
    h.insert(header::COOKIE, cookie.parse().unwrap());
    session::from_cookie(&app.conn(), &h).unwrap()
}

async fn login(app: &Shared, email: &str, password: &str) -> StatusCode {
    post_form(app, "/admin/login", None, &[("email", email), ("password", password)]).await.0
}

/// The text between `start` and the next `end` after it.
fn between<'a>(page: &'a str, start: &str, end: &str) -> &'a str {
    let at = page.find(start).unwrap_or_else(|| panic!("no {start} in {page}")) + start.len();
    &page[at..at + page[at..].find(end).unwrap()]
}

#[tokio::test]
async fn publishing_from_the_admin_uses_the_publish_tool() {
    let app = app();
    news(&app);
    publish_all(&app);
    let id = create(&app, json!({ "title": "春季新品发布会", "date": "2026-12-01" }));
    publish_all(&app);
    ok(&app, &editor(), "update_record", json!({ "id": id, "note": "改日期", "data": { "date": "2026-12-06" } }));
    let fresh = create(&app, json!({ "title": "新写的" }));
    // A structure change the editor may not publish.
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "title": "新闻动态", "note": "改名" }));
    let s = admin_session(&app, "editor").await;

    let (_, _, home) = get_with(&app, "/admin", Some(&s.0)).await;
    assert!(home.contains("href=\"/admin/publish\""), "the workbench links to the review: {home}");

    let (status, _, page) = get_with(&app, "/admin/publish", Some(&s.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&page);
    for needle in [
        "发布前看一遍", &format!("name=\"target\" value=\"record:{id}\" checked"), &format!("name=\"target\" value=\"record:{fresh}\" checked"),
        "<s>2026-12-01</s>", "<b>2026-12-06</b>", "日期", "春季新品发布会", "改日期", "editor@test", "新写的", "新建",
        "collection:news", "需要 structure 权限", "检查通过", "action=\"/admin/publish\"",
    ] {
        assert!(page.contains(needle), "missing {needle}: {page}");
    }
    assert!(!page.contains("value=\"collection:news\""), "no checkbox for what the editor may not publish: {page}");

    // Nothing ticked: refused, nothing published.
    let releases = count(&app, "SELECT count(*) FROM releases");
    let (status, _, page) = post_form(&app, "/admin/publish", Some(&s), &[("note", "空的")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("role=\"alert\"") && page.contains("targets 是空的"), "{page}");
    assert_eq!(count(&app, "SELECT count(*) FROM releases"), releases);

    // One item: only that goes live, through the publish tool, with the admin as actor.
    let (status, headers, _) = post_form(&app, "/admin/publish", Some(&s), &[("target", &format!("record:{id}")), ("note", "上线日期")]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let done = location(&headers).to_string();
    assert!(done.starts_with("/admin/publish?done="), "{done}");
    let (actor, note): (String, String) =
        app.conn().query_row("SELECT actor, note FROM releases ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((actor.as_str(), note.as_str()), ("editor@test", "上线日期"));
    assert_eq!(status_of(&app, id), "published");
    assert_eq!(status_of(&app, fresh), "draft");
    assert_eq!(changes::collection_status(&app.conn(), "news").unwrap(), "changed");
    let (status, _, page) = get_with(&app, &done, Some(&s.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&page);
    assert!(page.contains("已发布") && page.contains("春季新品发布会") && page.contains("href=\"/admin/releases\""), "{page}");
    assert!(!page.contains("新写的"), "only what went live is listed: {page}");

    // A tool refusal keeps the form as it was.
    let (status, _, page) = post_form(&app, "/admin/publish", Some(&s), &[("target", "collection:news"), ("note", "试试")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("没有权限发布") && page.contains("value=\"试试\""), "{page}");
}

#[tokio::test]
async fn the_dry_run_changes_nothing() {
    let app = app();
    news(&app);
    publish_all(&app);
    let a = create(&app, json!({ "title": "A", "slug": "a" }));
    publish_all(&app);
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "改", "data": { "title": "A2" } }));
    create(&app, json!({ "title": "B", "slug": "b" }));
    // A definition change that rewrites records and adds an index.
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "note": "加字段", "add_fields": [
        { "name": "rank", "title": "排序", "type": "number", "index": true, "default": 0 }] }));

    let before = snapshot(&app);
    let release = changes::dry_run_publish(&app.conn(), &builder(), None).unwrap();
    assert!(release.published.contains(&"collection:news".to_string()) && release.published.len() == 3, "{release:?}");
    assert_eq!(snapshot(&app), before, "a passing dry run writes nothing");
    let only = [Target::Record(a)];
    assert_eq!(changes::dry_run_publish(&app.conn(), &editor(), Some(&only)).unwrap().published, ["record:".to_string() + &a.to_string()]);
    assert_eq!(snapshot(&app), before);

    let s = admin_session(&app, "builder").await;
    let before = snapshot(&app);
    let (_, _, page) = get_with(&app, "/admin/publish", Some(&s.0)).await;
    assert!(page.contains("检查通过"), "{page}");
    assert_eq!(snapshot(&app), before, "showing the review writes nothing");

    // A duplicate slug slipped past the draft-time check: the review shows the problem, and still nothing is written.
    app.conn().execute("INSERT INTO records(collection, draft) VALUES('news', '{\"title\":\"C\",\"slug\":\"a\"}')", []).unwrap();
    let before = snapshot(&app);
    let e = changes::dry_run_publish(&app.conn(), &builder(), None).unwrap_err();
    assert!(e.contains("必须唯一"), "{e}");
    let (status, _, page) = get_with(&app, "/admin/publish", Some(&s.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&page);
    assert!(page.contains("检查没通过") && page.contains("必须唯一") && !page.contains("检查通过"), "{page}");
    assert_eq!(snapshot(&app), before);
    // The real publish agrees with the dry run.
    assert!(err(&app, &builder(), "publish", json!({ "note": "发" })).contains("必须唯一"));
}

#[tokio::test]
async fn editors_cannot_manage_keys_or_admins() {
    let app = app();
    let b = admin_session(&app, "builder").await;
    let e = admin_session(&app, "editor").await;
    let key_id: i64 = {
        auth::create_key(&app.conn(), "已有", &[Scope::Read]).unwrap();
        count(&app, "SELECT max(id) FROM api_keys")
    };
    let builder_id = user_of(&app, &b.0).id;
    let routes: Vec<(String, Vec<(&str, &str)>)> = vec![
        ("/admin/settings/keys".into(), vec![("name", "偷偷的"), ("scope", "write")]),
        (format!("/admin/settings/keys/{key_id}/revoke"), vec![("confirm", "1")]),
        ("/admin/settings/admins".into(), vec![("email", "new@test"), ("role", "builder")]),
        (format!("/admin/settings/admins/{builder_id}/reset"), vec![]),
        (format!("/admin/settings/admins/{builder_id}/delete"), vec![("confirm", "1")]),
    ];
    let state = |app: &Shared| {
        let conn = app.conn();
        let keys: String = conn.query_row("SELECT group_concat(name || revoked) FROM api_keys", [], |r| r.get(0)).unwrap();
        let admins: String = conn.query_row("SELECT group_concat(email || role || password_hash) FROM admins", [], |r| r.get(0)).unwrap();
        (keys, admins)
    };
    let before = state(&app);
    for (path, fields) in &routes {
        let (status, _, body) = post_form(&app, path, Some(&e), fields).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path} as editor: {body}");
        // A builder without the right token is refused too.
        let (status, _, _) = post_form(&app, path, Some(&(b.0.clone(), e.1.clone())), fields).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path} with another session's token");
        let (status, _, _) = post_form(&app, path, None, fields).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} without a session");
    }
    for path in ["/admin/settings/notes", "/admin/settings/password", "/admin/settings/appearance", "/admin/publish", "/admin/restore"] {
        let (status, _, _) = post_form(&app, path, Some(&(b.0.clone(), e.1.clone())), &[("mark", "blue")]).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path} with another session's token");
    }
    assert_eq!(state(&app), before, "nothing changed");
    assert!(!user_of(&app, &b.0).prefs["mark"].is_string());

    // The editor's settings page offers none of those forms.
    let (status, _, page) = get_with(&app, "/admin/settings", Some(&e.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&page);
    assert!(page.contains("已有") && page.contains("只有搭建者"), "{page}");
    for action in ["/admin/settings/keys", "/admin/settings/admins", "/admin/settings/notes"] {
        assert!(!page.contains(&format!("action=\"{action}")), "{action} offered to an editor: {page}");
    }
    assert!(page.contains("action=\"/admin/settings/password\"") && page.contains("action=\"/admin/logout\""), "{page}");
}

#[tokio::test]
async fn a_new_key_is_shown_once() {
    let app = app();
    let s = admin_session(&app, "builder").await;
    let (status, headers, page) = post_form(&app, "/admin/settings/keys", Some(&s), &[("name", "我的 agent"), ("scope", "write"), ("scope", "publish")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&page);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let key = format!("hc_{}", between(&page, "hc_", "<"));
    assert_eq!(key.len(), 43, "{key}");
    let actor = auth::actor_from_key(&app.conn(), &key).unwrap();
    assert_eq!((actor.name.as_str(), actor.scopes.as_slice()), ("key:我的 agent", &[Scope::Read, Scope::Write, Scope::Publish][..]));
    assert!(page.contains("只显示这一次"), "{page}");

    let (_, _, page) = get_with(&app, "/admin/settings", Some(&s.0)).await;
    assert_clean(&page);
    assert!(!page.contains(&key), "the key is shown only once: {page}");
    let page = page.replace("&#x2f;", "/");
    for needle in ["我的 agent", "读、写、发布", "http://127.0.0.1:4200/mcp", "http://127.0.0.1:4200/api/v1/content/"] {
        assert!(page.contains(needle), "missing {needle}: {page}");
    }

    // Bad input is refused and creates nothing.
    let keys = count(&app, "SELECT count(*) FROM api_keys");
    let (status, _, page) = post_form(&app, "/admin/settings/keys", Some(&s), &[("name", " ")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("role=\"alert\""), "{page}");
    assert_eq!(post_form(&app, "/admin/settings/keys", Some(&s), &[("name", "x"), ("scope", "root")]).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(count(&app, "SELECT count(*) FROM api_keys"), keys);

    let id = count(&app, "SELECT max(id) FROM api_keys");
    let (status, headers, _) = post_form(&app, &format!("/admin/settings/keys/{id}/revoke"), Some(&s), &[("confirm", "1")]).await;
    assert_eq!((status, location(&headers)), (StatusCode::SEE_OTHER, "/admin/settings"));
    assert!(auth::actor_from_key(&app.conn(), &key).is_none());
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&s.0)).await;
    assert!(page.contains("已吊销"), "{page}");
}

#[tokio::test]
async fn the_last_builder_cannot_be_removed() {
    let app = app();
    let s = admin_session(&app, "builder").await;
    admin_session(&app, "editor").await;
    let me = user_of(&app, &s.0).id;
    let editor_id: i64 = app.conn().query_row("SELECT id FROM admins WHERE email = 'editor@test'", [], |r| r.get(0)).unwrap();

    let (status, _, page) = post_form(&app, &format!("/admin/settings/admins/{me}/delete"), Some(&s), &[("confirm", "1")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("至少要留一位搭建者"), "{page}");
    assert_eq!(count(&app, "SELECT count(*) FROM admins WHERE role = 'builder'"), 1);

    // Adding an existing email would demote or reset it: refused.
    let (status, _, page) = post_form(&app, "/admin/settings/admins", Some(&s), &[("email", "Builder@Test"), ("role", "editor")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("已经是管理员"), "{page}");
    assert_eq!(user_of(&app, &s.0).role, "builder");
    assert_eq!(post_form(&app, "/admin/settings/admins", Some(&s), &[("email", "x@test"), ("role", "owner")]).await.0, StatusCode::BAD_REQUEST);

    // A new admin gets a password shown once.
    let (status, headers, page) = post_form(&app, "/admin/settings/admins", Some(&s), &[("email", "second@test"), ("role", "builder")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&page);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let password = between(&page, "<code class=\"secret\">", "<").to_string();
    assert_eq!(login(&app, "second@test", &password).await, StatusCode::SEE_OTHER);
    let (_, _, again) = get_with(&app, "/admin/settings", Some(&s.0)).await;
    assert!(!again.contains(&password) && again.contains("second@test"), "{again}");

    // Resetting another admin's password shows the new one once and ends their sessions.
    let (status, _, page) = post_form(&app, &format!("/admin/settings/admins/{editor_id}/reset"), Some(&s), &[]).await;
    assert_eq!(status, StatusCode::OK);
    let reset = between(&page, "<code class=\"secret\">", "<").to_string();
    assert_eq!(login(&app, "editor@test", "secret-pass").await, StatusCode::UNAUTHORIZED);
    assert_eq!(login(&app, "editor@test", &reset).await, StatusCode::SEE_OTHER);
    assert_eq!(count(&app, &format!("SELECT role = 'editor' FROM admins WHERE id = {editor_id}")), 1, "a reset keeps the role");
    // Your own password is changed with the old one, not reset.
    assert_eq!(post_form(&app, &format!("/admin/settings/admins/{me}/reset"), Some(&s), &[]).await.0, StatusCode::BAD_REQUEST);

    // With a second builder, removing works, yourself included: you are logged out.
    let (status, headers, _) = post_form(&app, &format!("/admin/settings/admins/{editor_id}/delete"), Some(&s), &[("confirm", "1")]).await;
    assert_eq!((status, location(&headers)), (StatusCode::SEE_OTHER, "/admin/settings"));
    let (status, headers, _) = post_form(&app, &format!("/admin/settings/admins/{me}/delete"), Some(&s), &[("confirm", "1")]).await;
    assert_eq!((status, location(&headers)), (StatusCode::SEE_OTHER, "/admin/login"));
    assert_eq!(get_with(&app, "/admin/settings", Some(&s.0)).await.0, StatusCode::SEE_OTHER);
    assert_eq!(count(&app, "SELECT count(*) FROM admins"), 1);
    assert_eq!(count(&app, "SELECT count(*) FROM admins WHERE role = 'builder'"), 1);
}

#[tokio::test]
async fn the_model_key_is_never_rendered() {
    // A model set by the server's environment: shown read-only, its key never.
    let app = app();
    let _ = app.env_model.set(cms_harness::llm::Settings {
        provider: cms_harness::llm::Provider::Anthropic,
        api_key: Some("sk-test-secret".into()),
        model: "claude-opus-5-5".into(),
        base_url: cms_harness::llm::DEFAULT_ANTHROPIC_URL.into(),
    });
    news(&app);
    create(&app, json!({ "title": "一条" }));
    let s = admin_session(&app, "builder").await;
    for path in ["/admin", "/admin/settings", "/admin/publish", "/admin/releases"] {
        let (status, _, page) = get_with(&app, path, Some(&s.0)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(!page.contains("sk-test-secret"), "{path} shows the model key: {page}");
        if path == "/admin/settings" {
            assert!(page.contains("已设置") && page.contains("claude-opus-5-5"), "{page}");
        }
    }
}

#[tokio::test]
async fn deleting_an_unpublished_record_asks_first() {
    let app = app();
    news(&app);
    publish_all(&app);
    let live = create(&app, json!({ "title": "上过线" }));
    publish_all(&app);
    let fresh = create(&app, json!({ "title": "还没发" }));
    let s = admin_session(&app, "editor").await;

    let (status, _, page) = post_form(&app, &format!("/admin/r/{fresh}/delete"), Some(&s), &[("note", "不要了")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&page);
    for needle in ["还没发", "不能找回", &format!("action=\"/admin/r/{fresh}/delete\""), "name=\"confirm\" value=\"1\"", "value=\"不要了\"", &format!("href=\"/admin/r/{fresh}\"")] {
        assert!(page.contains(needle), "missing {needle}: {page}");
    }
    assert_eq!(status_of(&app, fresh), "draft", "asking deletes nothing");
    // 丢弃这条草稿 removes it just the same, so it asks too.
    let (status, _, page) = post_form(&app, &format!("/admin/r/{fresh}/discard"), Some(&s), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("name=\"confirm\" value=\"1\""), "{page}");
    assert_eq!(status_of(&app, fresh), "draft", "asking discards nothing");

    let (status, headers, _) = post_form(&app, &format!("/admin/r/{fresh}/delete"), Some(&s), &[("confirm", "1")]).await;
    assert_eq!((status, location(&headers)), (StatusCode::SEE_OTHER, "/admin/c/news"));
    assert!(cms_harness::tools::call(&app, &reader(), "get_record", json!({ "id": fresh })).is_err());

    // A published record keeps its history: deleting it only marks it, without asking.
    let (status, headers, _) = post_form(&app, &format!("/admin/r/{live}/delete"), Some(&s), &[]).await;
    assert_eq!((status, location(&headers)), (StatusCode::SEE_OTHER, format!("/admin/r/{live}").as_str()));
    assert_eq!(status_of(&app, live), "deleting");
}

#[tokio::test]
async fn the_mark_color_preference_is_saved_and_applied() {
    let app = app();
    let s = admin_session(&app, "editor").await;
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&s.0)).await;
    assert!(page.contains("<body class=\"mark-yellow\">"), "yellow by default: {page}");
    assert!(page.contains("name=\"mark\" value=\"yellow\" aria-pressed=\"true\""), "{page}");

    let (status, headers, _) = post_form(&app, "/admin/settings/appearance", Some(&s), &[("mark", "blue")]).await;
    assert_eq!((status, location(&headers)), (StatusCode::SEE_OTHER, "/admin/settings"));
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&s.0)).await;
    assert_clean(&page);
    assert!(page.contains("<body class=\"mark-blue\">"), "{page}");
    assert!(page.contains("name=\"mark\" value=\"blue\" aria-pressed=\"true\""), "{page}");
    let (_, _, other) = get_with(&app, "/admin", Some(&s.0)).await;
    assert!(other.contains("<body class=\"mark-blue\">"), "every admin page: {other}");

    // Stored on the account, so a new session keeps it; the session's own nav preference is separate.
    assert_eq!(post_form(&app, "/admin/prefs", Some(&s), &[("nav", "collapsed")]).await.0, StatusCode::SEE_OTHER);
    assert_eq!(login(&app, "editor@test", "secret-pass").await, StatusCode::SEE_OTHER);
    let token: String = app.conn().query_row("SELECT prefs FROM admins WHERE email = 'editor@test'", [], |r| r.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&token).unwrap(), json!({ "mark": "blue", "lang": "zh" }), "the first login also keeps the browser's language");
    let u = user_of(&app, &s.0);
    assert_eq!((u.prefs["mark"].as_str(), u.prefs["nav"].as_str()), (Some("blue"), Some("collapsed")));

    // Only the presets; never a colour string.
    for bad in [("mark", "#ff0000"), ("mark", "red"), ("chat", "sideways"), ("other", "x")] {
        let (status, _, _) = post_form(&app, "/admin/settings/appearance", Some(&s), &[bad]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
    }
    assert_eq!(user_of(&app, &s.0).prefs["mark"], "blue");

    // The chat mode is stored and handed to the templates.
    assert_eq!(post_form(&app, "/admin/settings/appearance", Some(&s), &[("chat", "floating")]).await.0, StatusCode::SEE_OTHER);
    let u = user_of(&app, &s.0);
    assert_eq!(admin::nav(&app.conn(), &u, "settings")["chat"], "floating");
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&s.0)).await;
    assert!(page.contains("name=\"chat\" value=\"floating\" aria-pressed=\"true\""), "{page}");
}

#[tokio::test]
async fn a_past_version_can_be_restored_from_releases() {
    let app = app();
    news(&app);
    publish_all(&app);
    let id = create(&app, json!({ "title": "第一版" }));
    ok(&app, &builder(), "publish", json!({ "note": "首发" }));
    ok(&app, &editor(), "update_record", json!({ "id": id, "note": "改", "data": { "title": "第二版" } }));
    ok(&app, &builder(), "publish", json!({ "note": "再发" }));
    let s = admin_session(&app, "editor").await;

    let (status, _, page) = get_with(&app, "/admin/releases", Some(&s.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&page);
    let (at_second, at_first) = (page.find("再发").unwrap(), page.find("首发").unwrap());
    assert!(at_second < at_first, "newest first: {page}");
    assert!(page.contains("第二版") && page.contains("第一版") && page.contains("builder@test"), "{page}");
    let first: i64 = app
        .conn()
        .query_row("SELECT v.id FROM versions v JOIN releases r ON r.id = v.release_id WHERE r.note = '首发' AND v.target = ?1", [format!("record:{id}")], |r| r.get(0))
        .unwrap();
    assert!(page.contains(&format!("name=\"version_id\" value=\"{first}\"")), "{page}");

    let target = format!("record:{id}");
    let (status, headers, _) = post_form(&app, "/admin/restore", Some(&s), &[("target", &target), ("version_id", &first.to_string()), ("note", "改回去")]).await;
    assert_eq!((status, location(&headers)), (StatusCode::SEE_OTHER, "/admin/releases?restored=1"));
    let r = ok(&app, &reader(), "get_record", json!({ "id": id }));
    assert_eq!((r["status"].as_str(), r["data"]["title"].as_str()), (Some("changed"), Some("第一版")));
    let (_, _, page) = get_with(&app, "/admin/releases?restored=1", Some(&s.0)).await;
    assert!(page.contains("放回草稿"), "{page}");

    let (status, _, page) = post_form(&app, "/admin/restore", Some(&s), &[("target", &target), ("version_id", "99999"), ("note", "x")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("role=\"alert\"") && page.contains("99999"), "{page}");
}

#[tokio::test]
async fn site_notes_and_password_are_changed_in_settings() {
    let app = app();
    let b = admin_session(&app, "builder").await;
    let e = admin_session(&app, "editor").await;
    let (status, headers, body) = post_form(&app, "/admin/settings/notes", Some(&b), &[("text", "这是一家花店。\r\n周一休息。"), ("note", "写说明")]).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert_eq!((status, location(&headers)), (StatusCode::SEE_OTHER, "/admin/settings?saved=notes"));
    let notes = ok(&app, &reader(), "get_site_notes", json!({}));
    assert_eq!((notes["text"].as_str(), notes["status"].as_str()), (Some("这是一家花店。\n周一休息。"), Some("draft")));
    let (_, _, page) = get_with(&app, "/admin/settings?saved=notes", Some(&b.0)).await;
    assert_clean(&page);
    assert!(page.contains("周一休息") && page.contains("发布后"), "{page}");
    // Editors read the notes but the tool refuses them a change.
    let (_, _, page) = get_with(&app, "/admin/settings", Some(&e.0)).await;
    assert!(page.contains("周一休息"), "{page}");
    let (status, _, page) = post_form(&app, "/admin/settings/notes", Some(&e), &[("text", "改掉"), ("note", "n")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("structure"), "{page}");
    assert_eq!(ok(&app, &reader(), "get_site_notes", json!({}))["text"], "这是一家花店。\n周一休息。");

    // Own password: the old one is needed, other sessions end, this one stays.
    let (status, _, page) = post_form(&app, "/admin/settings/password", Some(&e), &[("old", "wrong"), ("new", "a-longer-pass")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("原密码不正确"), "{page}");
    let (_, headers, _) = post_form(&app, "/admin/login", None, &[("email", "editor@test"), ("password", "secret-pass")]).await;
    let other = headers[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
    let (status, headers, _) = post_form(&app, "/admin/settings/password", Some(&e), &[("old", "secret-pass"), ("new", "a-longer-pass")]).await;
    assert_eq!((status, location(&headers)), (StatusCode::SEE_OTHER, "/admin/settings?saved=password"));
    assert_eq!(get_with(&app, "/admin/settings", Some(&e.0)).await.0, StatusCode::OK);
    assert_eq!(get_with(&app, "/admin/settings", Some(&other)).await.0, StatusCode::SEE_OTHER);
    assert_eq!(login(&app, "editor@test", "a-longer-pass").await, StatusCode::SEE_OTHER);
}

#[test]
fn an_old_database_gets_the_admin_prefs_column() {
    let path = std::env::temp_dir().join(format!("hc-old-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE admins(id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, password_hash TEXT NOT NULL, role TEXT NOT NULL);
            INSERT INTO admins(email, password_hash, role) VALUES('old@test', 'x', 'builder');").unwrap();
    }
    let conn = cms_harness::store::open(path.to_str().unwrap()).unwrap();
    let prefs: String = conn.query_row("SELECT prefs FROM admins", [], |r| r.get(0)).unwrap();
    assert_eq!(prefs, "{}");
    drop(conn);
    cms_harness::store::open(path.to_str().unwrap()).unwrap();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn revoking_a_key_and_removing_an_admin_ask_first() {
    let app = app();
    let s = admin_session(&app, "builder").await;
    admin_session(&app, "editor").await;
    key(&app, &[Scope::Write]);
    let key_id: i64 = app.conn().query_row("SELECT id FROM api_keys", [], |r| r.get(0)).unwrap();
    let editor_id: i64 = app.conn().query_row("SELECT id FROM admins WHERE email = 'editor@test'", [], |r| r.get(0)).unwrap();
    let (status, _, page) = post_form(&app, &format!("/admin/settings/keys/{key_id}/revoke"), Some(&s), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("确认吊销") && page.contains("name=\"confirm\""));
    assert_eq!(count(&app, "SELECT count(*) FROM api_keys WHERE revoked = 0"), 1, "nothing revoked yet");
    let (status, _, page) = post_form(&app, &format!("/admin/settings/admins/{editor_id}/delete"), Some(&s), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("确认删除") && page.contains("editor@test"));
    assert_eq!(count(&app, "SELECT count(*) FROM admins"), 2, "nobody removed yet");
}
