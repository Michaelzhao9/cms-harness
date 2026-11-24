mod common;
use axum::http::{header, HeaderMap, Request, StatusCode};
use common::*;
use harness_cms::auth::Scope;
use harness_cms::Shared;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::io::Write;
use tower::ServiceExt;

fn write(app: &Shared, path: &str, content: &str) {
    ok(app, &builder(), "write_site_file", json!({ "path": path, "content": content, "note": "改文件" }));
}

fn record(app: &Shared, collection: &str, data: Value) -> i64 {
    ok(app, &builder(), "create_record", json!({ "collection": collection, "data": data, "note": "新记录" }))["id"].as_i64().unwrap()
}

/// A public `news` collection with one live record; returns its id.
fn public_news(app: &Shared) -> i64 {
    news(app);
    ok(app, &builder(), "update_collection", json!({ "name": "news", "public_read": true, "note": "公开" }));
    let id = record(app, "news", json!({ "title": "开业", "body": "<p>欢迎<b>光临</b></p><script>alert(1)</script>", "date": "2026-11-23", "tag": "公司" }));
    publish_all(app);
    id
}

/// GET with extra headers; (status, headers, body bytes).
async fn fetch(app: &Shared, path: &str, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut req = Request::get(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = harness_cms::api::router(app.clone()).oneshot(req.body(axum::body::Body::empty()).unwrap()).await.unwrap();
    let (status, h) = (resp.status(), resp.headers().clone());
    (status, h, resp.into_body().collect().await.unwrap().to_bytes().to_vec())
}

async fn page(app: &Shared, path: &str, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap, String) {
    let (s, h, b) = fetch(app, path, headers).await;
    (s, h, String::from_utf8(b).unwrap())
}

fn csp(h: &HeaderMap) -> String {
    h[header::CONTENT_SECURITY_POLICY].to_str().unwrap().to_string()
}

const LIVE_CSP: &str = "default-src 'self'; img-src 'self' https: data:; style-src 'self' 'unsafe-inline'; script-src 'self' https://esm.sh; frame-ancestors 'self'; base-uri 'none'; form-action 'self'";

// ---- the fallback site ----

#[tokio::test]
async fn a_fresh_site_shows_its_public_collections_with_the_fallback_templates() {
    let app = app();
    let id = public_news(&app);
    let (status, h, home) = page(&app, "/", &[]).await;
    assert_eq!(status, StatusCode::OK, "{home}");
    assert!(h[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
    assert!(home.contains("测试站") && home.contains("href=\"/news\"") && home.contains("新闻"), "{home}");
    assert_eq!(csp(&h), LIVE_CSP);
    assert!(!home.contains("<script"), "{home}");

    let (status, _, list) = page(&app, "/news", &[]).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert!(list.contains(&format!("href=\"/news/{id}\"")) && list.contains("开业"), "{list}");

    let (status, h, item) = page(&app, &format!("/news/{id}"), &[]).await;
    assert_eq!(status, StatusCode::OK, "{item}");
    // Rich text arrives cleaned, as HTML; the script was removed when it was written.
    assert!(item.contains("<p>欢迎<b>光临</b></p>") && !item.contains("<script") && !item.contains("alert"), "{item}");
    assert!(item.contains("2026-11-23") && item.contains("公司"), "{item}");
    assert_eq!(csp(&h), LIVE_CSP);
    assert_eq!(h[header::CACHE_CONTROL], "no-cache");
}

#[tokio::test]
async fn collections_that_are_not_public_or_not_live_are_404() {
    let app = app();
    let id = public_news(&app);
    ok(&app, &builder(), "create_collection", json!({ "name": "secrets", "title": "机密", "note": "建",
        "fields": [{ "name": "title", "title": "标题", "type": "text" }] }));
    let secret = record(&app, "secrets", json!({ "title": "不能看" }));
    publish_all(&app);
    let draft_only = record(&app, "news", json!({ "title": "还没发" }));
    for path in ["/secrets".to_string(), format!("/secrets/{secret}"), format!("/news/{draft_only}"), format!("/news/{}", id + 100),
                 "/news/abc".into(), "/nothing".into(), "/news/1/2".into(), "/Bad-Name".into()] {
        let (status, _, body) = page(&app, &path, &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
        assert!(!body.contains("不能看") && !body.contains("还没发"), "{path}: {body}");
    }
    let (_, _, home) = page(&app, "/", &[]).await;
    assert!(!home.contains("机密"), "{home}");
    // Reserved paths never reach the site.
    for path in ["/_hc/x.html", "/api/v1/nothing", "/uploads/x.png", "/admin/nothing"] {
        let (status, _, _) = page(&app, path, &[]).await;
        assert!(status == StatusCode::NOT_FOUND || status.is_redirection(), "{path}: {status}");
    }
}

#[tokio::test]
async fn stored_html_in_a_text_field_is_escaped() {
    let app = app();
    public_news(&app);
    let id = record(&app, "news", json!({ "title": "<img src=x onerror=alert(1)>" }));
    publish_all(&app);
    for path in ["/news".to_string(), format!("/news/{id}")] {
        let (_, _, body) = page(&app, &path, &[]).await;
        assert!(body.contains("&lt;img src=x onerror=alert(1)&gt;") && !body.contains("<img src=x"), "{path}: {body}");
    }
    write(&app, "index.html", "<h1>{{ record('news', id).title }}</h1>{% include 'card.svg' %}".replace("id", &id.to_string()).as_str());
    // An included file of another type is escaped as HTML too.
    write(&app, "card.svg", "<svg><text>{{ record('news', id).title }}</text></svg>".replace("id", &id.to_string()).as_str());
    publish_all(&app);
    let (_, _, body) = page(&app, "/", &[]).await;
    assert!(body.contains("<h1>&lt;img src=x onerror=alert(1)&gt;</h1>"), "{body}");
    assert!(body.contains("<svg><text>&lt;img src=x onerror=alert(1)&gt;</text></svg>"), "{body}");
}

// ---- site files ----

#[tokio::test]
async fn site_files_are_served_by_path() {
    let app = app();
    public_news(&app);
    write(&app, "index.html", "<h1>首页</h1>");
    write(&app, "about.html", "<h1>关于</h1>");
    write(&app, "blog/index.html", "<h1>博客</h1>");
    write(&app, "news.html", "<h1>我的新闻页</h1>");
    write(&app, "style.css", "h1 { color: red }");
    write(&app, "assets/index-DiwrgTda.js", "export const a = 1;");
    write(&app, "app.tsx", "export const App = (p: { n: number }) => <b>{p.n}</b>;");
    write(&app, "data.json", "{\"a\": 1}");
    let png: &[u8] = b"\x89PNG\r\n\x1a\nnot-really-an-image";
    write(&app, "img/Logo.png", &b64(png));
    publish_all(&app);

    for (path, needle) in [("/", "首页"), ("/about", "关于"), ("/about.html", "关于"), ("/blog", "博客"), ("/blog/", "博客"), ("/news", "我的新闻页")] {
        let (status, h, body) = page(&app, path, &[]).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(body.contains(needle), "{path}: {body}");
        assert!(h[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"), "{path}");
    }
    let (_, h, body) = page(&app, "/style.css", &[]).await;
    assert_eq!((h[header::CONTENT_TYPE].to_str().unwrap(), body.as_str()), ("text/css; charset=utf-8", "h1 { color: red }"));
    let (_, h, body) = page(&app, "/assets/index-DiwrgTda.js", &[]).await;
    assert_eq!((h[header::CONTENT_TYPE].to_str().unwrap(), body.as_str()), ("text/javascript; charset=utf-8", "export const a = 1;"));
    let (_, h, body) = page(&app, "/app.tsx", &[]).await;
    assert_eq!(h[header::CONTENT_TYPE], "text/javascript; charset=utf-8");
    assert!(body.contains("react/jsx-runtime") && !body.contains("number"), "{body}");
    let (_, h, _) = page(&app, "/data.json", &[]).await;
    assert_eq!(h[header::CONTENT_TYPE], "application/json; charset=utf-8");
    let (status, h, bytes) = fetch(&app, "/img/Logo.png", &[]).await;
    assert_eq!((status, h[header::CONTENT_TYPE].to_str().unwrap(), bytes.as_slice()), (StatusCode::OK, "image/png", png));
    // Paths are case-sensitive.
    assert_eq!(fetch(&app, "/img/logo.png", &[]).await.0, StatusCode::NOT_FOUND);
    // Assets carry a validator, so a browser can revalidate cheaply.
    let etag = h[header::ETAG].to_str().unwrap().to_string();
    assert_eq!(fetch(&app, "/img/Logo.png", &[("if-none-match", &etag)]).await.0, StatusCode::NOT_MODIFIED);
    // Drafts are not served on the live site.
    write(&app, "draft.css", "p{}");
    assert_eq!(fetch(&app, "/draft.css", &[]).await.0, StatusCode::NOT_FOUND);
    // A site 404 page is used when there is one.
    write(&app, "404.html", "<h1>走丢了</h1>");
    publish_all(&app);
    let (status, _, body) = page(&app, "/missing", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("走丢了"), "{body}");
}

#[tokio::test]
async fn templates_read_live_content_and_the_request() {
    let app = app();
    let id = public_news(&app);
    record(&app, "news", json!({ "title": "第二条", "date": "2026-11-24", "tag": "行业" }));
    publish_all(&app);
    write(&app, "index.html", &format!(concat!(
        "<title>{{{{ site.name }}}}</title>",
        "{{% for n in collection('news', sort='-date', limit=5) %}}[{{{{ n.title }}}}]{{% endfor %}}",
        "{{% for n in collection('news', filter={{'tag': '公司'}}) %}}<i>{{{{ n.title }}}}</i>{{% endfor %}}",
        "<b>{{{{ record('news', {id}).title }}}}</b><u>{{{{ record('news', 9999) is none }}}}</u>",
        "<q>{{{{ request.path }}}} {{{{ request.query.q }}}}</q><s>{{{{ member is none }}}}</s>"), id = id));
    publish_all(&app);
    record(&app, "news", json!({ "title": "草稿不显示", "date": "2026-11-25" }));
    let (status, _, body) = page(&app, "/?q=%3Cx%3E", &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("<title>测试站</title>"), "{body}");
    assert!(body.contains("[第二条][开业]") && !body.contains("草稿不显示"), "{body}");
    assert!(body.contains("<i>开业</i>") && !body.contains("<i>第二条</i>"), "{body}");
    assert!(body.contains("<b>开业</b><u>True</u>"), "{body}");
    assert!(body.contains("<q>&#x2f; &lt;x&gt;</q><s>True</s>"), "{body}");
}

#[tokio::test]
async fn a_page_reading_content_passes_publish_and_a_private_read_does_not() {
    let app = app();
    public_news(&app);
    write(&app, "index.html", "{% for n in collection('news') %}{{ n.title }}{% endfor %}");
    publish_all(&app);
    ok(&app, &builder(), "create_collection", json!({ "name": "secrets", "title": "机密", "note": "建",
        "fields": [{ "name": "title", "title": "标题", "type": "text" }] }));
    write(&app, "leak.html", "{% for n in collection('secrets') %}{{ n.title }}{% endfor %}");
    let e = err(&app, &builder(), "publish", json!({ "note": "n" }));
    assert!(e.contains("file:leak.html") && e.contains("secrets"), "{e}");
}

#[tokio::test]
async fn a_runaway_page_runs_out_of_fuel_and_shows_a_plain_error() {
    let app = app();
    write(&app, "index.html", "{% for a in range(100000) %}{% for b in range(100000) %}{% endfor %}{% endfor %}");
    let started = std::time::Instant::now();
    let e = err(&app, &builder(), "publish", json!({ "note": "n" }));
    assert!(e.contains("file:index.html") && e.contains("上限"), "{e}");
    assert!(started.elapsed().as_secs() < 20, "{:?}", started.elapsed());
    // Broken live page (stored past the checks): a plain 500, no details.
    write(&app, "index.html", "<p>ok</p>");
    publish_all(&app);
    app.conn().execute("UPDATE site_files SET published = ?1 WHERE path = 'index.html'", [json!("{{ 1 + 'a' }}").to_string()]).unwrap();
    let (status, _, body) = page(&app, "/", &[]).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!body.contains("index.html") && !body.contains("1 + "), "{body}");
    // The same failure in preview names the problem.
    let (cookie, _) = admin_session(&app, "builder").await;
    let (status, _, body) = page(&app, "/?preview=1", &[("cookie", &cookie)]).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.contains("index.html"), "{body}");
}

#[tokio::test]
async fn the_import_map_is_injected_with_its_hash_and_script_origins_extend_the_csp() {
    let app = app();
    write(&app, "index.html", "<!doctype html><html><head><title>t</title></head><body><script type=\"module\" src=\"/app.js\"></script></body></html>");
    publish_all(&app);
    let (_, h, body) = page(&app, "/", &[]).await;
    assert!(!body.contains("importmap"), "{body}");
    assert_eq!(csp(&h), LIVE_CSP);

    let map = json!({ "imports": { "react": "https://esm.sh/react@19", "x": "</script><script>alert(1)</script>" } });
    {
        let conn = app.conn();
        conn.execute("INSERT INTO site_settings(key, value) VALUES('import_map', ?1)", [map.to_string()]).unwrap();
        conn.execute("INSERT INTO site_settings(key, value) VALUES('script_origins', ?1)",
            [json!(["https://cdn.jsdelivr.net", "https://unpkg.com:8443", "javascript:alert(1)", "https://x; script-src *"]).to_string()]).unwrap();
    }
    let (_, h, body) = page(&app, "/", &[]).await;
    let start = body.find("<script type=\"importmap\">").expect(&body) + "<script type=\"importmap\">".len();
    let inner = &body[start..start + body[start..].find("</script>").unwrap()];
    assert!(body.find("importmap").unwrap() > body.find("<head>").unwrap() && body.find("importmap").unwrap() < body.find("<title>").unwrap(), "{body}");
    assert!(!inner.contains('<'), "{inner}");
    assert_eq!(serde_json::from_str::<Value>(inner).unwrap(), map);
    use base64::Engine;
    use sha2::Digest;
    let hash = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(inner.as_bytes()));
    let policy = csp(&h);
    assert!(policy.contains(&format!("script-src 'self' https://cdn.jsdelivr.net https://unpkg.com:8443 'sha256-{hash}';")), "{policy}");
    assert!(!policy.contains("javascript") && !policy.contains("esm.sh") && !policy.contains("script-src *"), "{policy}");
}

// ---- preview ----

#[tokio::test]
async fn a_draft_template_shows_only_in_preview_and_only_for_an_admin() {
    let app = app();
    write(&app, "index.html", "<h1>v1</h1>");
    write(&app, "style.css", "p{color:red}");
    publish_all(&app);
    write(&app, "index.html", "<h1>v2</h1>");
    write(&app, "style.css", "p{color:blue}");
    write(&app, "new.css", "p{}");

    for path in ["/", "/?preview=1", "/?preview=live"] {
        let (_, h, body) = page(&app, path, &[]).await;
        assert!(body.contains("v1") && !body.contains("/_hc/preview.js"), "{path}: {body}");
        assert!(h.get(header::SET_COOKIE).is_none(), "{path}");
    }
    let (cookie, _) = admin_session(&app, "builder").await;
    let (_, h, body) = page(&app, "/", &[("cookie", &cookie)]).await;
    assert!(body.contains("v1"), "an admin without ?preview sees the live site: {body}");
    assert_eq!(h[header::CACHE_CONTROL], "no-cache");
    let (_, _, body) = page(&app, "/?preview=live", &[("cookie", &cookie)]).await;
    assert!(body.contains("v1"), "{body}");

    let (status, h, body) = page(&app, "/?preview=1", &[("cookie", &cookie)]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("v2") && body.contains("<script src=\"/_hc/preview.js\"") && body.contains("href=\"/_hc/preview.css\""), "{body}");
    assert_eq!(h[header::CACHE_CONTROL], "no-store");
    assert!(csp(&h).starts_with("sandbox allow-scripts allow-forms allow-popups; default-src 'self';"), "{}", csp(&h));
    // The preview cookie lets the sandboxed frame (which sends no admin cookie) fetch drafts.
    let set = h[header::SET_COOKIE].to_str().unwrap();
    assert!(set.starts_with("hc_preview=") && set.contains("HttpOnly") && set.contains("Secure") && set.contains("SameSite=None"), "{set}");
    let preview = set.split(';').next().unwrap().to_string();

    let framed = [("cookie", preview.as_str()), ("sec-fetch-site", "cross-site"), ("sec-fetch-dest", "style")];
    assert_eq!(page(&app, "/style.css", &framed).await.2, "p{color:blue}");
    assert_eq!(page(&app, "/new.css", &framed).await.0, StatusCode::OK);
    assert_eq!(page(&app, "/style.css", &framed).await.1[header::CACHE_CONTROL], "no-store");
    let nav = [("cookie", preview.as_str()), ("sec-fetch-site", "cross-site"), ("sec-fetch-dest", "iframe")];
    assert!(page(&app, "/", &nav).await.2.contains("v2"));
    assert!(page(&app, "/?preview=1", &[("cookie", preview.as_str())]).await.2.contains("v2"));
    // The same cookie on ordinary browsing, or on a top-level visit from another site, shows the live site.
    for headers in [vec![("cookie", preview.as_str())],
                    vec![("cookie", preview.as_str()), ("sec-fetch-site", "same-origin"), ("sec-fetch-dest", "style")],
                    vec![("cookie", preview.as_str()), ("sec-fetch-site", "cross-site"), ("sec-fetch-dest", "document")]] {
        assert_eq!(page(&app, "/style.css", &headers).await.2, "p{color:red}", "{headers:?}");
        assert!(page(&app, "/", &headers).await.2.contains("v1"), "{headers:?}");
    }
    // A made-up key is ignored, and the key dies with the session.
    let forged = [("cookie", "hc_preview=00ff"), ("sec-fetch-site", "cross-site"), ("sec-fetch-dest", "style")];
    assert_eq!(page(&app, "/style.css", &forged).await.2, "p{color:red}");
    let csrf = harness_cms::web::session::from_cookie(&app.conn(), &{ let mut m = HeaderMap::new(); m.insert(header::COOKIE, cookie.parse().unwrap()); m }).unwrap().csrf;
    post_form(&app, "/admin/logout", Some(&(cookie.clone(), csrf)), &[]).await;
    assert_eq!(page(&app, "/style.css", &framed).await.2, "p{color:red}");
    assert!(page(&app, "/?preview=1", &[("cookie", &cookie)]).await.2.contains("v1"));
}

#[tokio::test]
async fn changed_values_are_marked_in_preview_without_changing_template_logic() {
    let app = app();
    news(&app);
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "public_read": true, "note": "公开",
        "add_fields": [{ "name": "price", "title": "价格", "type": "number" }] }));
    let a = record(&app, "news", json!({ "title": "旧标题", "slug": "a", "tag": "公司", "price": 10, "body": "<p>旧</p>" }));
    let same = record(&app, "news", json!({ "title": "没改", "slug": "s", "price": 1 }));
    write(&app, "index.html", concat!(
        "<!doctype html><html><head><title>{{ record('news', A).title }}</title></head><body>",
        "<a title=\"{{ record('news', A).title }}\" href=\"/n/{{ record('news', A).slug }}\">{{ record('news', A).title }}</a>",
        "{% if record('news', A).tag == '行业' %}<em>TAG</em>{% endif %}<i>{{ record('news', A).price * 2 }}</i>",
        "<div>{{ record('news', A).body }}</div><u>{{ record('news', S).title }}</u>",
        "{% for n in collection('news') %}<li>{{ n.title }}</li>{% endfor %}</body></html>")
        .replace("A)", &format!("{a})")).replace("S)", &format!("{same})")).as_str());
    publish_all(&app);
    ok(&app, &builder(), "update_record", json!({ "id": a, "data": { "title": "新标题", "tag": "行业", "price": 21, "body": "<p>新</p>" }, "note": "改" }));
    let fresh = record(&app, "news", json!({ "title": "新来的" }));
    let pending: Vec<String> = ok(&app, &reader(), "list_pending", json!({})).as_array().unwrap().iter().map(|p| p["target"].as_str().unwrap().to_string()).collect();
    let n = |t: String| pending.iter().position(|p| *p == t).unwrap() + 1;
    let (na, nf) = (n(format!("record:{a}")), n(format!("record:{fresh}")));

    let (cookie, _) = admin_session(&app, "builder").await;
    let (_, _, body) = page(&app, "/?preview=1", &[("cookie", &cookie)]).await;
    // Marks only in text, never inside a tag or a raw-text element.
    assert!(body.contains("<title>新标题</title>"), "{body}");
    assert!(body.contains("<a title=\"新标题\" href=\"/n/a\">"), "{body}");
    assert!(body.contains(&format!("<span data-hc-change=\"{na}\">新标题</span></a>")), "{body}");
    // Template logic sees the plain values.
    assert!(body.contains("<em>TAG</em>") && body.contains("<i>42</i>"), "{body}");
    assert!(body.contains(&format!("<div><span data-hc-change=\"{na}\"><p>新</p></span></div>")), "{body}");
    assert!(body.contains("<u>没改</u>"), "{body}");
    assert!(body.contains(&format!("<li><span data-hc-change=\"{nf}\">新来的</span></li>")), "{body}");
    assert!(body.contains(&format!("<li><span data-hc-change=\"{na}\">新标题</span></li>")) && body.contains("<li>没改</li>"), "{body}");
    assert!(!body.chars().any(|c| ('\u{E000}'..='\u{E006}').contains(&c)), "{body}");
    // The live site is untouched.
    let (_, _, live) = page(&app, "/", &[]).await;
    assert!(live.contains("<title>旧标题</title>") && !live.contains("data-hc-change") && !live.contains("新来的"), "{live}");
}

#[tokio::test]
async fn a_changed_include_is_wrapped_in_a_region_and_a_changed_layout_marks_the_page() {
    let app = app();
    write(&app, "layout.html", "<!doctype html><html><head>{% include 'meta.html' %}</head><body>{% block main %}{% endblock %}</body></html>");
    write(&app, "meta.html", "<meta name=\"v\" content=\"1\">");
    write(&app, "part.html", "<p>v1</p>");
    write(&app, "index.html", "{% extends 'layout.html' %}{% block main %}{% include 'part.html' %}<a href=\"/x\">x</a>{% endblock %}");
    publish_all(&app);
    write(&app, "part.html", "<p>v2</p>");
    write(&app, "meta.html", "<meta name=\"v\" content=\"2\">");
    write(&app, "layout.html", "<!doctype html><html><head>{% include 'meta.html' %}</head><body class=\"new\">{% block main %}{% endblock %}</body></html>");
    let pending: Vec<String> = ok(&app, &reader(), "list_pending", json!({})).as_array().unwrap().iter().map(|p| p["target"].as_str().unwrap().to_string()).collect();
    let n = |t: &str| pending.iter().position(|p| p == t).unwrap() + 1;

    let (cookie, _) = admin_session(&app, "builder").await;
    let (_, _, body) = page(&app, "/?preview=1", &[("cookie", &cookie)]).await;
    let part = n("file:part.html");
    assert!(body.contains(&format!("<hc-region data-hc-file=\"part.html\" data-hc-change=\"{part}\" style=\"display:contents\"><p>v2</p></hc-region>")), "{body}");
    // An include inside <head> is not wrapped; it and the layout are changes of the whole page.
    assert!(body.contains("<head><meta name=\"v\" content=\"2\">") || body.contains("<meta name=\"v\" content=\"2\">"), "{body}");
    assert!(!body.contains("data-hc-file=\"meta.html\"") && !body.contains("data-hc-file=\"layout.html\""), "{body}");
    let mut whole = [n("file:layout.html"), n("file:meta.html")];
    whole.sort();
    let attr = format!("data-hc-page=\"{}\"", whole.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(","));
    assert!(body.contains(&attr), "{attr}: {body}");
    // The preview assets come right after <head>, before the page's own tags.
    assert!(body.starts_with("<!doctype html><html><head><link rel=\"stylesheet\" href=\"/_hc/preview.css\"><script src=\"/_hc/preview.js\""), "{body}");
}

#[tokio::test]
async fn preview_assets_are_served() {
    let app = app();
    let (status, h, js) = page(&app, "/_hc/preview.js", &[]).await;
    assert_eq!((status, h[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "text/javascript; charset=utf-8"));
    for needle in ["hc:ready", "hc:picked", "hc:navigated", "hc:activate", "hc:pick", "hc:back", "hc:forward", "hc:mark-color", "postMessage"] {
        assert!(js.contains(needle), "{needle}");
    }
    let (status, h, css) = page(&app, "/_hc/preview.css", &[]).await;
    assert_eq!((status, h[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "text/css; charset=utf-8"));
    assert!(css.contains("--mark") && css.contains("#f5b700"), "{css}");
}

#[tokio::test]
async fn the_admin_frames_the_preview_in_a_sandbox_without_same_origin() {
    let app = app();
    let (cookie, _) = admin_session(&app, "builder").await;
    let (_, _, body) = get_with(&app, "/admin", Some(&cookie)).await;
    assert!(body.contains("<iframe class=\"preview\" sandbox=\"allow-scripts allow-forms allow-popups\" src=\"/?preview=1\""), "{body}");
    assert!(!body.contains("allow-same-origin"), "{body}");
}

// ---- bundle upload over HTTP ----

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    for (name, data) in entries {
        w.start_file(*name, opts).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn post_zip(app: &Shared, path: &str, key: Option<&str>, body: Vec<u8>) -> (StatusCode, Value) {
    let mut req = Request::post(path).header(header::CONTENT_TYPE, "application/zip");
    if let Some(k) = key {
        req = req.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    let resp = harness_cms::api::router(app.clone()).oneshot(req.body(axum::body::Body::from(body)).unwrap()).await.unwrap();
    let status = resp.status();
    (status, serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap_or(Value::Null))
}

#[tokio::test]
async fn a_raw_zip_uploads_over_http_with_replace() {
    let app = app();
    write(&app, "old.html", "<p>old</p>");
    write(&app, "keep-draft.css", "p{}");
    ok(&app, &builder(), "publish", json!({ "targets": ["file:old.html"], "note": "n" }));
    let zip = zip_of(&[("index.html", b"<p>hi</p>"), ("assets/Logo.PNG".to_lowercase().as_str(), b"\x89PNG\r\n\x1a\n..")]);

    assert_eq!(post_zip(&app, "/api/v1/site/bundle?note=x", None, zip.clone()).await.0, StatusCode::UNAUTHORIZED);
    let writer = key(&app, &[Scope::Write]);
    let (status, body) = post_zip(&app, "/api/v1/site/bundle?note=x", Some(&writer), zip.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("structure"), "{body}");
    let k = key(&app, &[Scope::Structure]);
    let (status, body) = post_zip(&app, "/api/v1/site/bundle", Some(&k), zip.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a note is required: {body}");

    let (status, body) = post_zip(&app, "/api/v1/site/bundle?note=%E6%95%B4%E5%8C%85&replace=1", Some(&k), zip).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!((body["result"]["written"].as_i64(), body["result"]["deleted"].as_i64()), (Some(2), Some(1)), "{body}");
    let status_of = |p: &str| ok(&app, &reader(), "get_site_file", json!({ "path": p }))["status"].clone();
    assert_eq!(status_of("old.html"), "deleting");
    assert_eq!(status_of("keep-draft.css"), "draft");
    assert_eq!(status_of("index.html"), "draft");
    let pending = ok(&app, &reader(), "list_pending", json!({}));
    assert!(pending.as_array().unwrap().iter().all(|p| p["note"] == "整包" || p["target"] == "file:keep-draft.css"), "{pending}");
}

#[tokio::test]
async fn a_normal_page_stays_well_inside_the_fuel_budget() {
    let app = app();
    public_news(&app);
    for i in 0..100 {
        record(&app, "news", json!({ "title": format!("第 {i} 条"), "body": "<p>正文</p>", "date": "2026-11-23", "tag": "行业" }));
    }
    publish_all(&app);
    // The built-in list shows 100 records; a site page loops over 100 records five times.
    let (status, _, body) = page(&app, "/news", &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    write(&app, "index.html", "{% for _ in range(5) %}{% for n in collection('news', limit=100) %}<h2>{{ n.title }}</h2>{{ n.body }}<p>{{ n.date }} {{ n.tag }} {{ n.slug }}</p>{% endfor %}{% endfor %}");
    publish_all(&app);
    let (status, _, body) = page(&app, "/", &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("<h2>").count(), 500);
}
