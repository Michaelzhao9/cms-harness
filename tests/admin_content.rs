mod common;
use axum::http::{header, HeaderMap, Request, StatusCode};
use common::*;
use cms_harness::web::session;
use cms_harness::Shared;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbImage::new(width, height).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// `authors` (name) and `news` (the common six fields plus author, editors, featured, score), published.
fn site(app: &Shared) {
    ok(app, &builder(), "create_collection", json!({ "name": "authors", "title": "作者", "note": "n",
        "fields": [{ "name": "name", "title": "姓名", "type": "text" }] }));
    news(app);
    ok(app, &builder(), "update_collection", json!({ "name": "news", "note": "n", "add_fields": [
        { "name": "author", "title": "作者", "type": "relation", "target": "authors" },
        { "name": "editors", "title": "编辑", "type": "relation", "target": "authors", "multiple": true },
        { "name": "featured", "title": "推荐", "type": "boolean" },
        { "name": "score", "title": "评分", "type": "number" }
    ] }));
    publish_all(app);
}

fn create(app: &Shared, collection: &str, data: Value) -> i64 {
    ok(app, &builder(), "create_record", json!({ "collection": collection, "note": "n", "data": data }))["id"].as_i64().unwrap()
}

fn record(app: &Shared, id: i64) -> Value {
    ok(app, &builder(), "get_record", json!({ "id": id }))
}

/// Every admin page: no scripts, no inline styles the CSP would drop.
fn assert_clean(page: &str) {
    assert_only_admin_js(page);
}

/// The value of the first `name="<name>" value="…"` attribute pair.
fn hidden(page: &str, name: &str) -> String {
    let at = page.find(&format!("name=\"{name}\" value=\"")).unwrap_or_else(|| panic!("no {name} in {page}"));
    let rest = &page[at + name.len() + 15..];
    rest[..rest.find('"').unwrap()].to_string()
}

fn uploads(app: &Shared) -> usize {
    std::fs::read_dir(app.data_dir.join("uploads")).map(|d| d.count()).unwrap_or(0)
}

/// A multipart POST with the session cookie. Parts are `(name, Some(filename), bytes)` for files, `(name, None, text)` for fields.
async fn post_multipart(app: &Shared, path: &str, cookie: Option<&str>, parts: &[(&str, Option<&str>, &[u8])]) -> (StatusCode, HeaderMap, String) {
    let boundary = "XBOUNDARYX";
    let mut body = vec![];
    for (name, file, bytes) in parts {
        let disposition = match file {
            Some(f) => format!("form-data; name=\"{name}\"; filename=\"{f}\"\r\nContent-Type: application/octet-stream"),
            None => format!("form-data; name=\"{name}\""),
        };
        body.extend(format!("--{boundary}\r\nContent-Disposition: {disposition}\r\n\r\n").as_bytes());
        body.extend(*bytes);
        body.extend(b"\r\n");
    }
    body.extend(format!("--{boundary}--\r\n").as_bytes());
    let mut req = Request::post(path).header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"));
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    let resp = cms_harness::api::router(app.clone()).oneshot(req.body(axum::body::Body::from(body)).unwrap()).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}

fn location(h: &HeaderMap) -> &str {
    h[header::LOCATION].to_str().unwrap()
}

/// Rows of the list table.
fn rows(page: &str) -> usize {
    page.matches("<tr class=\"row").count()
}

#[tokio::test]
async fn a_record_can_be_edited_by_hand_and_lands_as_a_draft() {
    let app = app();
    site(&app);
    let id = create(&app, "news", json!({ "title": "春季新品发布会", "date": "2026-12-01", "tag": "公司", "score": 3 }));
    let timed = create(&app, "news", json!({ "title": "有时间的", "date": "2026-12-06T10:30:00Z" }));
    publish_all(&app);
    let s = admin_session(&app, "editor").await;

    let (status, _, list) = get_with(&app, "/admin/c/news", Some(&s.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&list);
    for needle in ["<h1>新闻</h1>", "春季新品发布会", &format!("href=\"/admin/r/{id}\""), "已发布", "标题", "网址名", "正文", "日期", "标签"] {
        assert!(list.contains(needle), "missing {needle}: {list}");
    }
    assert!(!list.contains("<th scope=\"col\">封面</th>"), "only the first five fields are columns: {list}");
    assert_eq!(rows(&list), 2);

    let (status, _, page) = get_with(&app, &format!("/admin/r/{id}"), Some(&s.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&page);
    for needle in [
        "value=\"春季新品发布会\"", "type=\"date\"", "value=\"2026-12-01\"", "type=\"number\" step=\"any\"", "value=\"3\"",
        "type=\"checkbox\"", "必填", "保存时会清理", &format!("href=\"/admin?ctx=record:{id}\""), "交给对话来改",
        &format!("action=\"/admin/r/{id}\""), "enctype=\"multipart/form-data\"", "value=\"后台手动修改\"",
    ] {
        assert!(page.contains(needle), "missing {needle}: {page}");
    }
    let (_, _, timed_page) = get_with(&app, &format!("/admin/r/{timed}"), Some(&s.0)).await;
    assert!(timed_page.contains("type=\"datetime-local\"") && timed_page.contains("value=\"2026-12-06T10:30:00\""), "{timed_page}");
    // The workbench link target exists; the context is read in a later task.
    assert_eq!(get_with(&app, &format!("/admin?ctx=record:{id}"), Some(&s.0)).await.0, StatusCode::OK);

    // The whole form sent back unchanged is not a change.
    let seen = hidden(&page, "seen");
    let full = [("seen", seen.as_str()), ("note", "后台手动修改"), ("title", "春季新品发布会"), ("slug", ""), ("body", ""), ("date", "2026-12-01"),
        ("tag", "公司"), ("cover", ""), ("author", ""), ("editors", ""), ("featured", "false"), ("score", "3")];
    let (status, headers, body) = post_form(&app, &format!("/admin/r/{id}"), Some(&s), &full).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert!(location(&headers).starts_with(&format!("/admin/r/{id}")));
    assert_eq!(record(&app, id)["status"], "published");
    let timed_seen = hidden(&timed_page, "seen");
    post_form(&app, &format!("/admin/r/{timed}"), Some(&s), &[("seen", &timed_seen), ("title", "有时间的"), ("date", "2026-12-06T10:30:00")]).await;
    assert_eq!(record(&app, timed)["status"], "published", "a datetime round-trips");

    // One field changed: only it is sent, as a draft, by this admin.
    let mut changed = full;
    changed[5] = ("date", "2026-12-06");
    let (status, headers, _) = post_form(&app, &format!("/admin/r/{id}"), Some(&s), &changed).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&headers), format!("/admin/r/{id}?saved=1"));
    let r = record(&app, id);
    assert_eq!((r["status"].as_str(), r["data"]["date"].as_str(), r["data"]["title"].as_str()), (Some("changed"), Some("2026-12-06"), Some("春季新品发布会")));
    let pending = ok(&app, &builder(), "list_pending", json!({}));
    let item = pending.as_array().unwrap().iter().find(|p| p["target"] == format!("record:{id}")).unwrap_or_else(|| panic!("{pending}"));
    assert_eq!(item["actor"], "editor@test", "{item}");
    assert_eq!(item["note"], "后台手动修改", "{item}");
    assert_eq!(item["fields"], json!(["date"]), "{item}");
    let (_, _, live) = get(&app, &format!("/api/v1/content/news/{id}"), Some(&key(&app, &[cms_harness::auth::Scope::Read]))).await;
    assert_eq!(live["date"], "2026-12-01", "the live copy waits for publish");

    let (_, _, page) = get_with(&app, &format!("/admin/r/{id}?saved=1"), Some(&s.0)).await;
    assert!(page.contains("已存为草稿"), "{page}");
    assert!(page.contains("<s>2026-12-01</s>") && page.contains("<b>2026-12-06</b>"), "old struck, new bold: {page}");
    assert_clean(&page);

    // Status tabs.
    let fresh = {
        let (status, headers, body) = post_form(&app, "/admin/c/news/new", Some(&s), &[("title", "新写的"), ("featured", "false"), ("featured", "true")]).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
        location(&headers).trim_start_matches("/admin/r/").split('?').next().unwrap().parse::<i64>().unwrap()
    };
    let r = record(&app, fresh);
    assert_eq!((r["status"].as_str(), r["data"]["featured"].as_bool()), (Some("draft"), Some(true)), "{r}");
    let (_, _, all) = get_with(&app, "/admin/c/news", Some(&s.0)).await;
    assert_eq!(rows(&all), 3);
    assert!(all.contains("3 条，其中 1 条有改动、1 条草稿"), "{all}");
    let (_, _, changed_tab) = get_with(&app, "/admin/c/news?status=changed", Some(&s.0)).await;
    assert_eq!(rows(&changed_tab), 1);
    assert!(changed_tab.contains("春季新品发布会") && changed_tab.contains("有改动"), "{changed_tab}");
    let (_, _, draft_tab) = get_with(&app, "/admin/c/news?status=draft", Some(&s.0)).await;
    assert_eq!(rows(&draft_tab), 1);
    assert!(draft_tab.contains("新写的") && !draft_tab.contains("春季新品发布会"), "{draft_tab}");
    let (_, _, new_form) = get_with(&app, "/admin/c/news?new=1", Some(&s.0)).await;
    assert!(new_form.contains("action=\"/admin/c/news/new\""), "{new_form}");

    // Delete a never-published record: it asks first, then it is gone, back to the list.
    let (status, _, _) = post_form(&app, &format!("/admin/r/{fresh}/delete"), Some(&s), &[]).await;
    assert_eq!(status, StatusCode::OK);
    let (status, headers, _) = post_form(&app, &format!("/admin/r/{fresh}/delete"), Some(&s), &[("confirm", "1")]).await;
    assert_eq!((status, location(&headers)), (StatusCode::SEE_OTHER, "/admin/c/news"));
    assert!(cms_harness::tools::call(&app, &builder(), "get_record", json!({ "id": fresh })).is_err());
    // Delete a published one: it waits for publish.
    let (_, headers, _) = post_form(&app, &format!("/admin/r/{timed}/delete"), Some(&s), &[("note", "过期了")]).await;
    assert_eq!(location(&headers), format!("/admin/r/{timed}"));
    assert_eq!(record(&app, timed)["status"], "deleting");
    let (_, _, page) = get_with(&app, &format!("/admin/r/{timed}"), Some(&s.0)).await;
    assert!(page.contains("等待删除"), "{page}");

    // The status filter behind the tabs is a query_records argument.
    let deleting = ok(&app, &builder(), "query_records", json!({ "collection": "news", "status": ["deleting"] }));
    assert_eq!((deleting["total"].as_i64(), deleting["items"][0]["id"].as_i64()), (Some(1), Some(timed)));
    assert!(cms_harness::tools::call(&app, &builder(), "query_records", json!({ "collection": "news", "status": ["gone"] })).is_err());

    // Discard goes back to the live copy.
    let (status, _, _) = post_form(&app, &format!("/admin/r/{id}/discard"), Some(&s), &[]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let r = record(&app, id);
    assert_eq!((r["status"].as_str(), r["data"]["date"].as_str()), (Some("published"), Some("2026-12-01")));
}

#[tokio::test]
async fn lists_page_by_twenty_and_views_follow_their_declaration() {
    let app = app();
    site(&app);
    for n in 0..21 {
        create(&app, "news", json!({ "title": format!("第{n}条"), "date": format!("2026-01-{:02}", n + 1), "tag": if n % 2 == 0 { "公司" } else { "行业" } }));
    }
    ok(&app, &builder(), "set_view", json!({ "name": "by_date", "title": "按日期", "collection": "news", "note": "n",
        "columns": ["date", "title"], "filters": ["tag"], "sort": "date", "actions": ["edit"] }));
    let s = admin_session(&app, "editor").await;

    let (_, _, first) = get_with(&app, "/admin/c/news", Some(&s.0)).await;
    assert_eq!(rows(&first), 20);
    assert!(first.contains("page=2") && first.replace("&#x2f;", "/").contains("第 1 / 2 页"), "{first}");
    let (_, _, second) = get_with(&app, "/admin/c/news?page=2", Some(&s.0)).await;
    assert_eq!(rows(&second), 1);
    assert!(second.contains("第0条"), "newest first, so the oldest is on page 2: {second}");

    let (status, _, view) = get_with(&app, "/admin/v/by_date", Some(&s.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_clean(&view);
    assert!(view.contains("<h1>按日期</h1>"), "{view}");
    let heads: Vec<&str> = view.split("<th scope=\"col\">").skip(1).map(|h| &h[..h.find('<').unwrap()]).collect();
    assert_eq!(heads, ["日期", "标题", "状态"], "{view}");
    assert!(view.find("第0条").unwrap() < view.find("第1条").unwrap(), "the view's sort applies");
    assert!(!view.contains(">新建<"), "the view does not allow create: {view}");
    assert!(view.contains("name=\"tag\""), "the view's filter is offered: {view}");
    let (_, _, filtered) = get_with(&app, "/admin/v/by_date?tag=%E8%A1%8C%E4%B8%9A", Some(&s.0)).await;
    assert_eq!(rows(&filtered), 10, "{filtered}");
    assert!(!filtered.contains("第0条"), "{filtered}");

    assert_eq!(get_with(&app, "/admin/v/nope", Some(&s.0)).await.0, StatusCode::NOT_FOUND);
    assert_eq!(get_with(&app, "/admin/c/nope", Some(&s.0)).await.0, StatusCode::NOT_FOUND);
    assert_eq!(get_with(&app, "/admin/r/99999", Some(&s.0)).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn edit_errors_keep_the_values_typed() {
    let app = app();
    site(&app);
    let id = create(&app, "news", json!({ "title": "原标题", "slug": "taken" }));
    let other = create(&app, "news", json!({ "title": "另一条", "slug": "mine" }));
    publish_all(&app);
    let s = admin_session(&app, "editor").await;
    let (_, _, page) = get_with(&app, &format!("/admin/r/{other}"), Some(&s.0)).await;
    let seen = hidden(&page, "seen");

    // A duplicate unique value: the tool's message, the typed values, nothing saved.
    let (status, _, body) = post_form(&app, &format!("/admin/r/{other}"), Some(&s),
        &[("seen", &seen), ("title", "我新打的标题"), ("slug", "taken"), ("note", "我写的说明")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_clean(&body);
    assert!(body.contains("role=\"alert\"") && body.contains("taken"), "{body}");
    assert!(body.contains("value=\"我新打的标题\"") && body.contains("value=\"我写的说明\""), "{body}");
    assert_eq!(record(&app, other)["status"], "published");

    // A required field emptied.
    let (status, _, body) = post_form(&app, &format!("/admin/r/{other}"), Some(&s), &[("seen", &seen), ("title", ""), ("score", "12.5")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("必填的") && body.contains("value=\"12.5\""), "{body}");

    // Someone else saved after this form was opened.
    app.conn().execute("UPDATE records SET updated_at = '2000-01-01 00:00:00' WHERE id = ?1", [other]).unwrap();
    let (status, _, body) = post_form(&app, &format!("/admin/r/{other}"), Some(&s), &[("seen", &seen), ("title", "晚了一步")]).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("被改过") && body.contains("value=\"晚了一步\""), "{body}");
    assert_eq!(record(&app, other)["data"]["title"], "另一条");
    // Saving again from the refused form is still refused: only a reload picks up the other change.
    let again = hidden(&body, "seen");
    assert_eq!(again, seen);
    let (status, _, _) = post_form(&app, &format!("/admin/r/{other}"), Some(&s), &[("seen", &again), ("title", "晚了一步")]).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(record(&app, other)["data"]["title"], "另一条");

    // The new-record form keeps what was typed too.
    let (status, _, body) = post_form(&app, "/admin/c/news/new", Some(&s), &[("slug", "taken"), ("title", "新的")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("value=\"新的\"") && body.contains("action=\"/admin/c/news/new\""), "{body}");
    assert_eq!(record(&app, id)["status"], "published");
}

#[tokio::test]
async fn richtext_never_renders_inline_in_the_admin() {
    let app = app();
    site(&app);
    let id = create(&app, "news", json!({ "title": "富文本", "body": "<p>安全的</p>" }));
    publish_all(&app);
    // Straight into the draft, past the cleaner, as an attacker with database access would.
    let evil = "<img src=x onerror=1>";
    app.conn()
        .execute("UPDATE records SET draft = json_set(published, '$.body', ?1, '$.title', ?1) WHERE id = ?2", rusqlite::params![evil, id])
        .unwrap();
    let s = admin_session(&app, "builder").await;
    for path in [format!("/admin/r/{id}"), "/admin/c/news".to_string()] {
        let (status, _, page) = get_with(&app, &path, Some(&s.0)).await;
        assert_eq!(status, StatusCode::OK);
        assert_clean(&page);
        assert!(!page.contains("<img src=x") && !page.contains("onerror=1>"), "{path}: {page}");
    }
    let (_, _, page) = get_with(&app, &format!("/admin/r/{id}"), Some(&s.0)).await;
    assert!(page.contains("srcdoc=\"&lt;img src=x onerror=1&gt;\""), "{page}");
    let frame = &page[page.find("<iframe").unwrap()..];
    let frame = &frame[..frame.find('>').unwrap()];
    assert!(frame.contains(" sandbox ") && !frame.contains("allow-"), "{frame}");
    assert!(page.contains("&lt;img src=x onerror=1&gt;</textarea>"), "the source is escaped in the textarea: {page}");
}

#[tokio::test]
async fn a_post_without_csrf_changes_nothing() {
    let app = app();
    site(&app);
    let id = create(&app, "news", json!({ "title": "不许动" }));
    publish_all(&app);
    let s = admin_session(&app, "editor").await;
    let other = admin_session(&app, "builder").await;
    let (_, _, page) = get_with(&app, &format!("/admin/r/{id}"), Some(&s.0)).await;
    let seen = hidden(&page, "seen");
    let file = png(2, 2);
    let routes = [format!("/admin/r/{id}"), format!("/admin/r/{id}/delete"), format!("/admin/r/{id}/discard"), "/admin/c/news/new".into(), "/admin/media".into()];
    for path in &routes {
        for token in [None, Some(other.1.as_str())] {
            let mut fields = vec![("seen", seen.as_str()), ("title", "改了")];
            if let Some(t) = token {
                fields.push(("csrf", t));
            }
            let (status, _, _) = post_form(&app, path, None, &fields).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} without a session");
            let req = Request::post(path.as_str()).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded").header(header::COOKIE, &s.0);
            let body = fields.iter().map(|(k, v)| format!("{k}={}", v.replace(' ', "+"))).collect::<Vec<_>>().join("&");
            let resp = cms_harness::api::router(app.clone()).oneshot(req.body(axum::body::Body::from(body)).unwrap()).await.unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{path} with token {token:?}");
            let mut parts: Vec<(&str, Option<&str>, &[u8])> = vec![("seen", None, seen.as_bytes()), ("title", None, "改了".as_bytes()),
                ("upload:cover", Some("a.png"), &file), ("file", Some("a.png"), &file)];
            if let Some(t) = token {
                parts.insert(0, ("csrf", None, t.as_bytes()));
            }
            let (status, _, _) = post_multipart(&app, path, Some(&s.0), &parts).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "multipart {path} with token {token:?}");
        }
    }
    let r = record(&app, id);
    assert_eq!((r["status"].as_str(), r["data"]["title"].as_str()), (Some("published"), Some("不许动")));
    assert_eq!(ok(&app, &builder(), "query_records", json!({ "collection": "news" }))["total"], 1);
    assert_eq!(uploads(&app), 0, "nothing was stored");
    // GET never changes anything, whatever it carries.
    get_with(&app, &format!("/admin/r/{id}/delete"), Some(&s.0)).await;
    assert_eq!(record(&app, id)["status"], "published");
}

#[tokio::test]
async fn an_editor_cannot_reach_structure_actions() {
    let app = app();
    site(&app);
    let id = create(&app, "news", json!({ "title": "一条" }));
    ok(&app, &builder(), "set_view", json!({ "name": "all_news", "title": "全部新闻", "collection": "news", "note": "n",
        "columns": ["title"], "actions": ["create", "edit", "delete"] }));
    publish_all(&app);
    let s = admin_session(&app, "editor").await;
    for path in ["/admin/c/news".to_string(), "/admin/c/news?new=1".into(), format!("/admin/r/{id}"), "/admin/v/all_news".into(), "/admin/media".into()] {
        let (status, _, page) = get_with(&app, &path, Some(&s.0)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_clean(&page);
        for needle in ["新建集合", "删除集合", "修改集合", "字段"] {
            assert!(!page.contains(needle), "{path} offers {needle}: {page}");
        }
        // Every form posts to a record-level route or the shell's own.
        for action in page.split("action=\"").skip(1).map(|a| &a[..a.find('"').unwrap()]) {
            let allowed = ["/admin/prefs", "/admin/chat", "/admin/chat/new", "/admin/chat/stop", "/admin/c/news/new", "/admin/media"].contains(&action)
                || action.strip_prefix("/admin/r/").is_some_and(|rest| {
                    let n = rest.trim_end_matches("/delete").trim_end_matches("/discard");
                    n.parse::<i64>().is_ok()
                });
            assert!(allowed, "{path} posts to {action}");
        }
    }
    // The session's own actor is the one the forms write with; structure tools refuse it.
    let mut h = HeaderMap::new();
    h.insert(header::COOKIE, s.0.parse().unwrap());
    let actor = session::from_cookie(&app.conn(), &h).unwrap().actor();
    let e = cms_harness::tools::call(&app, &actor, "create_collection", json!({ "name": "sneaky", "title": "偷偷建", "note": "n",
        "fields": [{ "name": "title", "title": "标题", "type": "text" }] })).unwrap_err();
    assert!(e.contains("structure"), "{e}");
    let e = cms_harness::tools::call(&app, &actor, "discard_draft", json!({ "target": "view:all_news" })).unwrap_err();
    assert!(e.contains("structure"), "{e}");
    assert!(cms_harness::tools::call(&app, &builder(), "get_collection", json!({ "name": "sneaky" })).is_err());
}

#[tokio::test]
async fn relation_select_lists_target_records() {
    let app = app();
    site(&app);
    let zhou = create(&app, "authors", json!({ "name": "周以宁" }));
    let he = create(&app, "authors", json!({ "name": "贺川" }));
    let id = create(&app, "news", json!({ "title": "有作者", "author": zhou }));
    publish_all(&app);
    let shen = create(&app, "authors", json!({ "name": "沈知微" })); // a draft author is offered too
    let s = admin_session(&app, "editor").await;

    let (_, _, page) = get_with(&app, &format!("/admin/r/{id}"), Some(&s.0)).await;
    assert_clean(&page);
    let select = &page[page.find("name=\"author\"").unwrap()..];
    let select = &select[..select.find("</select>").unwrap()];
    for (who, name) in [(zhou, "周以宁"), (he, "贺川"), (shen, "沈知微")] {
        assert!(select.contains(&format!("value=\"{who}\"")) && select.contains(&format!("#{who} {name}")), "{who}: {select}");
    }
    assert!(select.contains(&format!("value=\"{zhou}\" selected")), "{select}");
    assert!(page.contains("name=\"editors\" multiple"), "{page}");

    let seen = hidden(&page, "seen");
    let (status, _, body) = post_form(&app, &format!("/admin/r/{id}"), Some(&s),
        &[("seen", &seen), ("author", &he.to_string()), ("editors", ""), ("editors", &zhou.to_string()), ("editors", &shen.to_string())]).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let r = record(&app, id);
    assert_eq!(r["data"]["author"], he);
    assert_eq!(r["data"]["editors"], json!([zhou, shen]));
    let (_, _, list) = get_with(&app, "/admin/c/authors", Some(&s.0)).await;
    assert!(list.contains("周以宁"), "{list}");
}

#[tokio::test]
async fn media_upload_from_the_record_form_sets_the_field() {
    let app = app();
    site(&app);
    let id = create(&app, "news", json!({ "title": "有封面" }));
    publish_all(&app);
    let s = admin_session(&app, "editor").await;
    let (_, _, page) = get_with(&app, &format!("/admin/r/{id}"), Some(&s.0)).await;
    assert!(page.contains("name=\"upload:cover\"") && page.contains("type=\"file\""), "{page}");
    let seen = hidden(&page, "seen");
    let file = png(3, 2);

    let (status, headers, body) = post_multipart(&app, &format!("/admin/r/{id}"), Some(&s.0), &[
        ("csrf", None, s.1.as_bytes()), ("seen", None, seen.as_bytes()), ("title", None, "有封面".as_bytes()), ("cover", None, b""),
        ("upload:cover", Some("cover.png"), &file),
    ]).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(location(&headers), format!("/admin/r/{id}?saved=1"));
    let path = record(&app, id)["data"]["cover"].as_str().unwrap().to_string();
    assert!(path.starts_with("/uploads/") && path.ends_with(".png"), "{path}");
    let name = path.trim_start_matches("/uploads/").to_string();
    assert!(cms_harness::media::read(&app, &name).is_some());

    let (_, _, page) = get_with(&app, &format!("/admin/r/{id}"), Some(&s.0)).await;
    assert_clean(&page);
    assert!(page.contains(&format!("src=\"/uploads/{name}\"")), "the current image is shown: {page}");
    let select = &page[page.find("name=\"cover\"").unwrap()..];
    assert!(select[..select.find("</select>").unwrap()].contains(&format!("{name}\" selected")), "{select}");

    // An empty file input changes nothing; choosing another upload from the select sets it.
    let (status, _, body) = post_multipart(&app, "/admin/media", Some(&s.0), &[("csrf", None, s.1.as_bytes()), ("file", Some("b.png"), &png(5, 5))]).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let (_, _, library) = get_with(&app, "/admin/media", Some(&s.0)).await;
    assert_clean(&library);
    assert_eq!(library.matches("<figure").count(), 2, "{library}");
    let newest = library.split("src=\"/uploads/").nth(1).map(|s| s[..s.find('"').unwrap()].to_string()).unwrap();
    assert_ne!(newest, name, "newest first");
    let seen = hidden(&page, "seen");
    let (status, _, body) = post_multipart(&app, &format!("/admin/r/{id}"), Some(&s.0), &[
        ("csrf", None, s.1.as_bytes()), ("seen", None, seen.as_bytes()), ("cover", None, format!("/uploads/{newest}").as_bytes()),
        ("upload:cover", Some(""), b""),
    ]).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(record(&app, id)["data"]["cover"], format!("/uploads/{newest}"));

    // Files past the default 2 MB body limit get through on all three upload routes.
    let mut big = b"%PDF-1.4 ".to_vec();
    big.resize(3 * 1024 * 1024, b' ');
    let (status, _, body) = post_multipart(&app, "/admin/media", Some(&s.0), &[("csrf", None, s.1.as_bytes()), ("file", Some("big.pdf"), &big)]).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let (status, _, body) = post_multipart(&app, "/admin/c/news/new", Some(&s.0), &[("csrf", None, s.1.as_bytes()), ("title", None, "大文件".as_bytes()), ("upload:cover", Some("big.pdf"), &big)]).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let seen = hidden(&get_with(&app, &format!("/admin/r/{id}"), Some(&s.0)).await.2, "seen");
    let (status, _, body) = post_multipart(&app, &format!("/admin/r/{id}"), Some(&s.0), &[("csrf", None, s.1.as_bytes()), ("seen", None, seen.as_bytes()), ("upload:cover", Some("big.pdf"), &big)]).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert!(record(&app, id)["data"]["cover"].as_str().unwrap().ends_with(".pdf"));

    // A file the uploader refuses is shown on the form.
    let seen = hidden(&get_with(&app, &format!("/admin/r/{id}"), Some(&s.0)).await.2, "seen");
    let (status, _, body) = post_multipart(&app, &format!("/admin/r/{id}"), Some(&s.0), &[
        ("csrf", None, s.1.as_bytes()), ("seen", None, seen.as_bytes()), ("title", None, "改标题".as_bytes()), ("upload:cover", Some("x.txt"), b"hello"),
    ]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("x.txt") && body.contains("value=\"改标题\""), "{body}");
}
