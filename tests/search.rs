mod common;
use cms_harness::access::Viewer;
use cms_harness::auth::Scope;
use cms_harness::search::search;
use common::*;
use serde_json::json;

fn posts(app: &cms_harness::Shared, name: &str, read: &str) {
    ok(app, &builder(), "create_collection", json!({ "name": name, "title": name, "read": read, "create": "public", "note": "建",
        "fields": [{ "name": "title", "title": "标题", "type": "text" }, { "name": "body", "title": "正文", "type": "richtext" }] }));
}

fn add(app: &cms_harness::Shared, collection: &str, title: &str, body: &str) -> i64 {
    ok(app, &builder(), "create_record", json!({ "collection": collection, "note": "写", "data": { "title": title, "body": body } }))["id"].as_i64().unwrap()
}

fn ids(app: &cms_harness::Shared, q: &str, viewer: Viewer) -> Vec<i64> {
    search(&app.conn(), q, None, viewer, 50).unwrap().into_iter().map(|h| h.id).collect()
}

#[test]
fn finds_published_chinese_and_english_and_follows_publishing() {
    let app = app();
    posts(&app, "posts", "public");
    let a = add(&app, "posts", "手工陶瓷杯", "<p>每一只<b>杯子</b>要烧三周。</p>");
    let b = add(&app, "posts", "Winter opening hours", "<p>Closed on Mondays.</p>");
    // Drafts are not searchable.
    assert!(ids(&app, "陶瓷", Viewer::Anon).is_empty());
    publish_all(&app);
    assert_eq!(ids(&app, "陶瓷", Viewer::Anon), vec![a]);
    assert_eq!(ids(&app, "杯", Viewer::Anon), vec![a], "a single character");
    assert_eq!(ids(&app, "烧三周", Viewer::Anon), vec![a], "rich text, without its tags");
    assert_eq!(ids(&app, "瓷陶", Viewer::Anon), Vec::<i64>::new(), "pairs in order");
    assert_eq!(ids(&app, "monday", Viewer::Anon), vec![b], "a word prefix, any case");
    assert_eq!(ids(&app, "winter hours", Viewer::Anon), vec![b]);
    assert!(ids(&app, "\"*) OR NEAR(", Viewer::Anon).is_empty(), "query syntax is plain text");
    let hit = &search(&app.conn(), "杯子", None, Viewer::Anon, 5).unwrap()[0];
    assert!(hit.snippet.contains("杯子") && !hit.snippet.contains('<'), "{}", hit.snippet);

    // An edit counts once published; a deletion once published.
    ok(&app, &builder(), "update_record", json!({ "id": a, "note": "改", "data": { "title": "青釉茶壶" } }));
    assert_eq!(ids(&app, "陶瓷", Viewer::Anon), vec![a], "still the live text");
    publish_all(&app);
    assert!(ids(&app, "陶瓷", Viewer::Anon).is_empty());
    assert_eq!(ids(&app, "茶壶", Viewer::Anon), vec![a]);
    ok(&app, &builder(), "delete_record", json!({ "id": a, "note": "删" }));
    publish_all(&app);
    assert!(ids(&app, "茶壶", Viewer::Anon).is_empty());
}

#[test]
fn hits_follow_the_read_rules() {
    let app = app();
    posts(&app, "open", "public");
    posts(&app, "club", "members");
    posts(&app, "mine", "owner");
    posts(&app, "secret", "private");
    let open = add(&app, "open", "公开的花瓶", "");
    let club = add(&app, "club", "会员的花瓶", "");
    let mine = add(&app, "mine", "我的花瓶", "");
    let theirs = add(&app, "mine", "别人的花瓶", "");
    let secret = add(&app, "secret", "内部的花瓶", "");
    publish_all(&app);
    app.conn().execute("UPDATE records SET owner = 7 WHERE id = ?1", [mine]).unwrap();
    app.conn().execute("UPDATE records SET owner = 8 WHERE id = ?1", [theirs]).unwrap();
    let sorted = |mut v: Vec<i64>| { v.sort(); v };
    assert_eq!(sorted(ids(&app, "花瓶", Viewer::Anon)), vec![open]);
    assert_eq!(sorted(ids(&app, "花瓶", Viewer::Member(7))), vec![open, club, mine]);
    assert_eq!(sorted(ids(&app, "花瓶", Viewer::Key)), vec![open, club, mine, theirs, secret]);
    let only: Vec<i64> = search(&app.conn(), "花瓶", Some("club"), Viewer::Key, 50).unwrap().into_iter().map(|h| h.id).collect();
    assert_eq!(only, vec![club]);
}

#[tokio::test]
async fn site_pages_rest_and_the_tool_search_alike() {
    let app = app();
    posts(&app, "posts", "public");
    posts(&app, "secret", "private");
    let a = add(&app, "posts", "青釉茶壶", "<p>壶身是手拉坯。</p>");
    add(&app, "secret", "青釉的成本价", "");
    ok(&app, &builder(), "write_site_file", json!({ "path": "search.html", "note": "搜索页",
        "content": "{% for r in search(request.query.q or '') %}<li data-c=\"{{ r._collection }}\">{{ r.title }}|{{ r._snippet }}</li>{% endfor %}" }));
    publish_all(&app);

    let page = get_text(&app, "/search?q=%E9%9D%92%E9%87%89").await; // 青釉
    assert!(page.contains("data-c=\"posts\">青釉茶壶|"), "{page}");
    assert!(!page.contains("成本价"), "a visitor never finds private content: {page}");

    let (status, h, v) = get(&app, "/api/v1/search?q=%E9%9D%92%E9%87%89", None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{v}");
    assert_eq!(v["items"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["items"][0]["data"]["title"], "青釉茶壶");
    assert_eq!(h["access-control-allow-origin"], "*");
    let k = key(&app, &[]);
    let (_, h, v) = get(&app, "/api/v1/search?q=%E9%9D%92%E9%87%89", Some(&k)).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 2, "a key sees every live collection: {v}");
    assert!(h.get("access-control-allow-origin").is_none());

    let hits = ok(&app, &reader(), "search", json!({ "q": "拉坯" }));
    assert_eq!(hits[0]["id"], a);
    let _ = Scope::Read;
}

#[tokio::test]
async fn a_visitor_submission_is_searchable_at_once() {
    let app = app();
    posts(&app, "guestbook", "public");
    publish_all(&app);
    let (status, v) = post_json(&app, "/api/v1/content/guestbook", None, json!({ "title": "来过的留言" })).await;
    assert_eq!(status, axum::http::StatusCode::CREATED, "{v}");
    assert_eq!(ids(&app, "留言", Viewer::Anon).len(), 1);
}

#[test]
fn an_older_database_is_indexed_when_opened() {
    let path = std::env::temp_dir().join(format!("hc-search-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let conn = cms_harness::store::open(path.to_str().unwrap()).unwrap();
        conn.execute_batch("DROP TRIGGER records_search_insert; DROP TABLE search;
            INSERT INTO records(collection, published) VALUES ('x', '{\"title\":\"旧的记录\"}');").unwrap();
    }
    let conn = cms_harness::store::open(path.to_str().unwrap()).unwrap();
    let n: i64 = conn.query_row("SELECT count(*) FROM search WHERE search MATCH '\"记录\"'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
    drop(conn);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn text_of_a_removed_field_is_not_found() {
    let app = app();
    ok(&app, &builder(), "create_collection", json!({ "name": "people", "title": "人", "read": "public", "note": "建",
        "fields": [{ "name": "name", "title": "名字", "type": "text" }, { "name": "phone", "title": "电话", "type": "text" }] }));
    ok(&app, &builder(), "create_record", json!({ "collection": "people", "note": "写", "data": { "name": "小王", "phone": "机密号码 13800" } }));
    publish_all(&app);
    assert_eq!(ids(&app, "机密号码", Viewer::Anon).len(), 1);
    // The field goes; its content stays in the record (so it can come back) but no longer shows anywhere.
    ok(&app, &builder(), "update_collection", json!({ "name": "people", "note": "去掉电话", "remove_fields": ["phone"], "confirm": 1 }));
    publish_all(&app);
    assert!(ids(&app, "机密号码", Viewer::Anon).is_empty());
    let hits = search(&app.conn(), "小王", None, Viewer::Anon, 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(!hits[0].snippet.contains("13800"), "{}", hits[0].snippet);
}
