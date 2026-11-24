mod common;
use common::*;
use serde_json::json;

fn write(app: &cms_harness::Shared, path: &str, content: &str) {
    ok(app, &builder(), "write_site_file", json!({ "path": path, "content": content, "note": "写页面" }));
}

#[tokio::test]
async fn a_fresh_site_lists_its_welcome_and_robots_points_to_the_sitemap() {
    let app = app();
    let xml = get_text(&app, "/sitemap.xml").await;
    assert!(xml.starts_with("<?xml") && xml.contains("<loc>http://127.0.0.1:4200/</loc>"), "{xml}");
    assert_eq!(xml.matches("<url>").count(), 1);
    let robots = get_text(&app, "/robots.txt").await;
    assert!(robots.contains("Sitemap: http://127.0.0.1:4200/sitemap.xml") && robots.contains("Disallow: /admin"), "{robots}");
}

#[tokio::test]
async fn pages_are_listed_once_published_without_layouts_and_fragments() {
    let app = app();
    write(&app, "layout.html", "<html>{% block main %}{% endblock %}</html>");
    write(&app, "index.html", "{% extends \"layout.html\" %}{% block main %}home{% endblock %}");
    write(&app, "about.html", "{% extends 'layout.html' %}{% block main %}{% include 'card.html' %}{% endblock %}");
    write(&app, "card.html", "<div>card</div>");
    write(&app, "blog/index.html", "<p>blog</p>");
    write(&app, "_parts/nav.html", "<nav></nav>");
    write(&app, "404.html", "<p>missing</p>");
    write(&app, "style.css", "body{}");
    assert_eq!(get_text(&app, "/sitemap.xml").await.matches("<url>").count(), 1, "drafts are not listed");
    publish_all(&app);
    let xml = get_text(&app, "/sitemap.xml").await;
    let locs: Vec<&str> = xml.split("<loc>").skip(1).map(|s| s.split("</loc>").next().unwrap()).collect();
    assert_eq!(locs, ["http://127.0.0.1:4200/about", "http://127.0.0.1:4200/blog/", "http://127.0.0.1:4200/"], "{xml}");
    assert_eq!(xml.matches("<lastmod>").count(), 3, "{xml}");
    // A page deleted and published leaves the sitemap.
    ok(&app, &builder(), "delete_site_file", json!({ "path": "about.html", "note": "删" }));
    publish_all(&app);
    assert!(!get_text(&app, "/sitemap.xml").await.contains("/about"));
}

#[tokio::test]
async fn the_sites_own_files_win() {
    let app = app();
    write(&app, "robots.txt", "User-agent: *\nDisallow: /");
    write(&app, "sitemap.xml", "<urlset>mine</urlset>");
    publish_all(&app);
    assert_eq!(get_text(&app, "/robots.txt").await, "User-agent: *\nDisallow: /");
    assert_eq!(get_text(&app, "/sitemap.xml").await, "<urlset>mine</urlset>");
}
