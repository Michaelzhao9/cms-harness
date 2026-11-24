mod common;
use common::*;
use serde_json::json;

fn write(app: &cms_harness::Shared, path: &str, content: &str) {
    ok(app, &builder(), "write_site_file", json!({ "path": path, "content": content, "note": "页面" }));
}

#[tokio::test]
async fn the_live_site_becomes_static_files() {
    let app = app();
    ok(&app, &builder(), "create_collection", json!({ "name": "news", "title": "新闻", "read": "public", "note": "建",
        "fields": [{ "name": "title", "title": "标题", "type": "text" }] }));
    ok(&app, &builder(), "create_record", json!({ "collection": "news", "note": "写", "data": { "title": "鼓手大赛" } }));
    ok(&app, &builder(), "create_record", json!({ "collection": "news", "note": "写", "data": { "title": "还没发布" } }));
    write(&app, "index.html", "<link rel=stylesheet href=/style.css>{% for n in collection('news') %}<a href=\"{{ n.url }}\">{{ n.title }}</a>{% endfor %}");
    write(&app, "news/_item.html", "<h1>{{ item.title }}</h1>");
    write(&app, "contact.html", "{% extends 'layout.html' %}{% block main %}<form method=post action=/messages/new></form>{% endblock %}");
    write(&app, "style.css", "body{color:red}");
    write(&app, "app.jsx", "export const A = () => <b>hi</b>;");
    write(&app, "layout.html", "{% block main %}{% endblock %}");
    std::fs::create_dir_all(app.data_dir.join("uploads")).unwrap();
    std::fs::write(app.data_dir.join("uploads").join("ab12.png"), b"png").unwrap();
    ok(&app, &builder(), "publish", json!({ "note": "上线", "targets": ["collection:news", "record:1", "file:index.html", "file:news/_item.html",
        "file:contact.html", "file:style.css", "file:app.jsx", "file:layout.html"] }));

    let dir = std::env::temp_dir().join(format!("hc-export-{:016x}", rand::random::<u64>()));
    let s = cms_harness::export::export(&app, &dir).await.unwrap();
    let read = |p: &str| std::fs::read_to_string(dir.join(p)).unwrap_or_else(|e| panic!("{p}: {e}"));
    assert!(read("index.html").contains("鼓手大赛"));
    assert!(read("news/鼓手大赛.html").contains("<h1>鼓手大赛</h1>"), "record pages are files too");
    assert!(!dir.join("news/还没发布.html").exists(), "drafts are not exported");
    assert!(!dir.join("layout.html").exists() && !dir.join("news/_item.html").exists(), "templates are not pages");
    assert_eq!(read("style.css"), "body{color:red}");
    assert!(!read("app.jsx").contains("<b>"), "scripts as the site serves them, transpiled");
    assert!(read("sitemap.xml").contains("%E9%BC%93") && read("robots.txt").contains("Sitemap:"));
    assert!(dir.join("404.html").exists() && dir.join("uploads/ab12.png").exists());
    assert_eq!(s.need_server, ["/contact"]);
    assert_eq!(s.uploads, 1);
}
