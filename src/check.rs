//! `check_page`: what is wrong with a draft page, as the preview would render it. The builder (the chat, or an
//! agent over MCP) writes a page and then looks at it here, instead of waiting for a person to notice.
//!
//! Read from the server's own render, with no browser: the render error if there is one, links and files that
//! lead nowhere, and the handful of omissions a careful builder would fix.

use crate::canvas::{attrs, tags, Tag};
use crate::App;
use rusqlite::Connection;
use serde_json::{json, Value};

/// Addresses looked up for one page; a page with more is checked up to here and says so.
// ponytail: each internal link is rendered once to see whether it exists; cache per publish if this is ever slow.
const MAX_TARGETS: usize = 60;

/// Text with tags taken out and whitespace collapsed, cut to `max` characters.
fn text(html: &str, max: usize) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' if inside => {
                inside = false;
                out.push(' ');
            }
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    let joined = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let entity = joined.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#x2f;", "/").replace("&#39;", "'");
    if entity.chars().count() > max {
        format!("{}…", entity.chars().take(max).collect::<String>())
    } else {
        entity
    }
}

/// What an element holds: from its start tag to its closing tag.
fn inner<'a>(html: &'a str, tag: &Tag) -> &'a str {
    let rest = &html[tag.open_end..];
    let close = format!("</{}", tag.name);
    let end = rest.to_ascii_lowercase().find(&close).unwrap_or(rest.len());
    &rest[..end]
}

/// Where an `href` or `src` leads on this site (`/about`), or `None` for another site, an anchor, `mailto:`.
fn target(page: &str, raw: &str) -> Option<String> {
    let raw = raw.trim();
    let path = raw.split(['#', '?']).next().unwrap_or_default();
    if path.is_empty() || path.starts_with("//") || path.split('/').next().is_some_and(|first| first.contains(':')) {
        return None;
    }
    let abs = if path.starts_with('/') {
        path.to_string()
    } else {
        // Beside the page: `about` on `/shop/index` is `/shop/about`.
        format!("{}/{}", page.rsplit_once('/').map_or("", |(dir, _)| dir), path.trim_start_matches("./"))
    };
    // The server's own pages and the preview's helpers are not the site's.
    (!["/_hc/", "/admin", "/api/", "/mcp"].iter().any(|p| abs.starts_with(p))).then_some(abs)
}

/// Whether an address answers in the drafts.
fn exists(app: &App, conn: &Connection, path: &str) -> bool {
    match path.strip_prefix("/uploads/") {
        Some(name) => crate::media::read(app, name).is_some(),
        None => crate::web::site::draft(app, conn, path).0 != 404,
    }
}

pub fn page(app: &App, conn: &Connection, path: &str) -> Result<Value, String> {
    if !path.starts_with('/') || path.len() > 300 {
        return Err("path 是页面的网址，以 / 开头：比如 / 是首页，/about 是 about.html。".into());
    }
    let (status, body) = crate::web::site::draft(app, conn, path);
    let html = String::from_utf8_lossy(&body).into_owned();
    if status == 500 {
        return Ok(json!({ "path": path, "status": 500, "ok": false, "error": text(&html, 800) }));
    }
    if status == 404 {
        return Ok(json!({ "path": path, "status": 404, "ok": false, "error": "草稿里没有这个页面：检查网址，或者先用 write_site_file 写出它的文件。" }));
    }
    if (300..400).contains(&status) {
        return Ok(json!({ "path": path, "status": status, "ok": true, "note": "这个网址会跳转到别处。" }));
    }

    let all = tags(&html);
    let attr = |tag: &Tag, name: &str| attrs(&html, tag).into_iter().find(|a| a.name.eq_ignore_ascii_case(name)).map(|a| a.value);
    let named = |name: &'static str| all.iter().filter(move |t| t.name == name);

    let title = named("title").next().map(|t| text(inner(&html, t), 120)).unwrap_or_default();
    let headings: Vec<String> = all.iter().filter(|t| matches!(t.name.as_str(), "h1" | "h2" | "h3")).map(|t| format!("{} {}", t.name, text(inner(&html, t), 80))).collect();
    let h1 = named("h1").count();
    let images_without_alt = named("img").filter(|t| attr(t, "alt").is_none()).count();

    let (mut links, mut files): (Vec<String>, Vec<String>) = (vec![], vec![]);
    for t in &all {
        let (list, raw) = match t.name.as_str() {
            "a" => (&mut links, attr(t, "href")),
            "link" => (&mut files, attr(t, "href")),
            "img" | "script" | "source" | "video" | "audio" | "iframe" => (&mut files, attr(t, "src")),
            _ => continue,
        };
        if let Some(to) = raw.and_then(|r| target(path, &r)) {
            if !list.contains(&to) {
                list.push(to);
            }
        }
    }
    let mut warnings: Vec<String> = vec![];
    if links.len() + files.len() > MAX_TARGETS {
        warnings.push(format!("这一页的链接和文件有 {} 个，只检查了前 {MAX_TARGETS} 个。", links.len() + files.len()));
        files.truncate(MAX_TARGETS);
        links.truncate(MAX_TARGETS - files.len());
    }
    let broken: Vec<String> = links.into_iter().filter(|l| !exists(app, conn, l)).collect();
    let missing: Vec<String> = files.into_iter().filter(|f| !exists(app, conn, f)).collect();

    if title.is_empty() {
        warnings.push("页面没有 <title>：浏览器标签和搜索结果里会没有名字。".into());
    }
    if !named("meta").any(|t| attr(t, "name").is_some_and(|n| n.eq_ignore_ascii_case("viewport"))) {
        warnings.push("没有 <meta name=\"viewport\">：手机上会按电脑的宽度缩小显示。".into());
    }
    if !named("html").any(|t| attr(t, "lang").is_some_and(|l| !l.is_empty())) {
        warnings.push("<html> 没有 lang 属性：读屏软件和翻译工具不知道页面是什么语言。".into());
    }
    match h1 {
        0 => warnings.push("页面没有 h1 标题。".into()),
        1 => {}
        n => warnings.push(format!("页面有 {n} 个 h1：一个页面一个主标题。")),
    }
    if images_without_alt > 0 {
        warnings.push(format!("{images_without_alt} 张图片没有 alt：有内容的图写上说明，纯装饰的写 alt=\"\"。"));
    }
    Ok(json!({
        "path": path, "status": status, "ok": broken.is_empty() && missing.is_empty(),
        "title": title, "headings": headings, "broken_links": broken, "missing_files": missing,
        "images_without_alt": images_without_alt, "warnings": warnings, "bytes": body.len(),
    }))
}
