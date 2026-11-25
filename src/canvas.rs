//! Hand edits on the preview ("画板"): what an admin changes by pointing at the page instead of asking the chat.
//!
//! Every element of a site template is known by where it starts in its file: `<path>#<n>`, the n-th start tag
//! (`src`). A preview render marks each one with `data-hc-src`, so a click names its source. Style edits are kept
//! apart from the site files, in the drafted option `setting:canvas`, and reach the page as one stylesheet; the
//! elements they apply to carry `data-hc="<id>"` in preview and live renders alike. An element in a shared
//! template (a layout, an include, a record page's template) is the same element on every page that uses it.
//!
//! Each edit remembers a fingerprint of its element (tag and the text right after it). When the file changes
//! above it and its position moves, the edit follows the one element with the same fingerprint, and applies to
//! nothing when there is none, rather than to a wrong element.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// The drafted option holding every hand edit.
pub const KEY: &str = "canvas";

/// Width tiers, with the media query each one's edits sit under (`all` has none).
pub const TIERS: [(&str, &str); 4] = [
    ("all", ""),
    ("desktop", "(min-width: 1024px)"),
    ("tablet", "(min-width: 600px) and (max-width: 1023.98px)"),
    ("mobile", "(max-width: 599.98px)"),
];

/// Tags that are not drawn, or whose content is not markup: counted, never marked.
const UNMARKED: [&str; 12] = ["html", "head", "body", "meta", "link", "title", "script", "style", "base", "noscript", "br", "wbr"];
/// Elements whose content is raw text up to their closing tag.
const RAW: [&str; 4] = ["script", "style", "textarea", "title"];

/// One start tag of a template source.
#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    /// 1-based position among the file's start tags.
    pub n: usize,
    pub name: String,
    /// Byte offset of `<`.
    pub start: usize,
    /// Byte offset right after the tag name, where attributes may be inserted.
    pub name_end: usize,
    /// Byte offset right after `>`.
    pub open_end: usize,
}

/// The start tags of an HTML template, skipping template blocks (`{{ }}`, `{% %}`, `{# #}`, `{% raw %}`),
/// comments and raw-text elements.
pub fn tags(src: &str) -> Vec<Tag> {
    let b = src.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        let rest = &src[i..];
        if rest.starts_with("{%") && rest[2..].trim_start_matches('-').trim_start().starts_with("raw") {
            i += rest.find("endraw").map_or(rest.len(), |e| e + 6);
        } else if rest.starts_with("{{") {
            i += rest.find("}}").map_or(rest.len(), |e| e + 2);
        } else if rest.starts_with("{%") {
            i += rest.find("%}").map_or(rest.len(), |e| e + 2);
        } else if rest.starts_with("{#") {
            i += rest.find("#}").map_or(rest.len(), |e| e + 2);
        } else if rest.starts_with("<!--") {
            i += rest.find("-->").map_or(rest.len(), |e| e + 3);
        } else if b[i] == b'<' && b.get(i + 1).is_some_and(u8::is_ascii_alphabetic) {
            let name_len = rest[1..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).unwrap_or(rest.len() - 1);
            let name = rest[1..1 + name_len].to_ascii_lowercase();
            let open_end = i + tag_end(rest);
            out.push(Tag { n: out.len() + 1, name: name.clone(), start: i, name_end: i + 1 + name_len, open_end });
            i = open_end;
            if RAW.contains(&name.as_str()) {
                let lower = src[i..].to_ascii_lowercase();
                i += lower.find(&format!("</{name}")).unwrap_or(lower.len());
            }
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// Length of the start tag at the beginning of `s`, up to and including `>`: quotes and template blocks respected.
fn tag_end(s: &str) -> usize {
    let mut quote: Option<char> = None;
    let mut it = s.char_indices().skip(1).peekable();
    while let Some((j, c)) = it.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '{') if matches!(it.peek(), Some((_, '{' | '%' | '#'))) => {
                let close = match it.peek() { Some((_, '{')) => "}}", Some((_, '%')) => "%}", _ => "#}" };
                match s[j..].find(close) {
                    Some(e) => while it.peek().is_some_and(|(k, _)| *k < j + e + 2) { it.next(); },
                    None => return s.len(),
                }
            }
            (None, '>') => return j + 1,
            _ => {}
        }
    }
    s.len()
}

/// What identifies an element besides its position: its tag and the first literal text after it.
pub fn fingerprint(src: &str, tag: &Tag) -> String {
    let after = &src[tag.open_end..];
    let text: String = after.chars().take_while(|c| *c != '<' && *c != '{').collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{}|{}", tag.name, text.chars().take(32).collect::<String>())
}

/// The short id of an edited element, used in `data-hc` and the stylesheet.
pub fn id(key: &str) -> String {
    hex::encode(&Sha256::digest(key.as_bytes())[..5])
}

fn attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")
}

/// Splits `src` (`<path>#<n>`) into its parts.
pub fn parse_src(src: &str) -> Option<(&str, usize)> {
    let (path, n) = src.rsplit_once('#')?;
    let n: usize = n.parse().ok().filter(|n| *n > 0)?;
    crate::site::check_path(path).ok()?;
    Some((path, n))
}

/// Where each of `doc`'s edits for the file at `path` lands in `src` now: position → edit key. An edit whose
/// element moved follows its fingerprint when exactly one element has it.
pub fn resolve(doc: &Value, path: &str, src: &str, tags: &[Tag]) -> HashMap<usize, String> {
    let mut out = HashMap::new();
    let Some(els) = doc["els"].as_object() else { return out };
    for (key, el) in els {
        let Some((p, n)) = parse_src(key) else { continue };
        if p != path {
            continue;
        }
        let fp = el["fp"].as_str().unwrap_or_default();
        let at = tags.get(n - 1).filter(|t| fingerprint(src, t) == fp).map(|t| t.n).or_else(|| {
            let same: Vec<usize> = tags.iter().filter(|t| fingerprint(src, t) == fp).map(|t| t.n).collect();
            (same.len() == 1).then(|| same[0])
        });
        if let Some(n) = at {
            out.insert(n, key.clone());
        }
    }
    out
}

/// A site template as a render loads it: in preview every drawn element names its source (`data-hc-src`); in
/// both modes an element with hand edits carries its id (`data-hc`).
pub fn annotate(path: &str, src: &str, doc: &Value, preview: bool) -> String {
    if !path.ends_with(".html") {
        return src.to_string();
    }
    let tags = tags(src);
    let edited = resolve(doc, path, src, &tags);
    if !preview && edited.is_empty() {
        return src.to_string();
    }
    let mut out = String::with_capacity(src.len() + tags.len() * 24);
    let mut from = 0;
    for t in &tags {
        if UNMARKED.contains(&t.name.as_str()) {
            continue;
        }
        let mut add = String::new();
        if preview {
            add.push_str(&format!(" data-hc-src=\"{}#{}\"", attr(path), t.n));
        }
        if let Some(key) = edited.get(&t.n) {
            add.push_str(&format!(" data-hc=\"{}\"", id(key)));
        }
        if add.is_empty() {
            continue;
        }
        out.push_str(&src[from..t.name_end]);
        out.push_str(&add);
        from = t.name_end;
    }
    out.push_str(&src[from..]);
    out
}

// ---- styles ----

/// Shadow presets: the only shadows an edit may set.
pub const SHADOWS: [&str; 4] = ["none", "0 1px 3px rgba(16,16,24,.12)", "0 8px 24px -8px rgba(16,16,24,.25)", "0 24px 48px -16px rgba(16,16,24,.35)"];

fn number_in(v: &str, unit: &str, lo: f64, hi: f64) -> Option<String> {
    let n: f64 = v.strip_suffix(unit)?.parse().ok()?;
    (n.is_finite() && (lo..=hi).contains(&n)).then(|| format!("{}{unit}", (n * 100.0).round() / 100.0))
}

fn color(v: &str) -> Option<String> {
    let v = v.trim().to_ascii_lowercase();
    if v == "transparent" {
        return Some(v);
    }
    let hex = v.strip_prefix('#')?;
    ([3, 4, 6, 8].contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(v)
}

/// The CSS properties a hand edit may set.
/// Position and size (`translate`, `width`, `height`), stacking (`z-index`) and hiding (`display: none`) are
/// edits of one width tier in practice; the admin sends them for the tier being looked at.
pub const PROPS: [&str; 20] = [
    "color", "background-color", "border-color", "font-size", "font-weight", "font-style", "text-decoration-line", "text-align",
    "line-height", "letter-spacing", "border-radius", "border-width", "padding", "opacity", "box-shadow",
    "translate", "width", "height", "z-index", "display",
];

/// Each value a hand edit sets, checked against what that property may hold. The page
/// is a site visitor's too: nothing but these values reaches its stylesheet.
pub fn check(prop: &str, value: &str) -> Result<String, String> {
    let v = value.trim();
    let ok = match prop {
        "color" | "background-color" | "border-color" => color(v),
        "font-size" => number_in(v, "px", 6.0, 400.0),
        "font-weight" => v.parse::<u16>().ok().filter(|w| (100..=900).contains(w) && w % 100 == 0).map(|w| w.to_string()),
        "font-style" => ["normal", "italic"].contains(&v).then(|| v.to_string()),
        "text-decoration-line" => ["none", "underline", "line-through"].contains(&v).then(|| v.to_string()),
        "text-align" => ["left", "center", "right", "justify"].contains(&v).then(|| v.to_string()),
        "line-height" => v.parse::<f64>().ok().filter(|n| (0.5..=4.0).contains(n)).map(|n| format!("{}", (n * 100.0).round() / 100.0)),
        "letter-spacing" => number_in(v, "em", -0.2, 1.0),
        "border-radius" => number_in(v, "px", 0.0, 999.0),
        "border-width" => number_in(v, "px", 0.0, 20.0),
        "padding" => number_in(v, "px", 0.0, 400.0),
        "opacity" => v.parse::<f64>().ok().filter(|n| (0.0..=1.0).contains(n)).map(|n| format!("{}", (n * 100.0).round() / 100.0)),
        "box-shadow" => SHADOWS.contains(&v).then(|| v.to_string()),
        "translate" => v.split_once(' ').and_then(|(x, y)| Some(format!("{} {}", number_in(x, "px", -5000.0, 5000.0)?, number_in(y.trim(), "px", -5000.0, 5000.0)?))),
        "width" | "height" => number_in(v, "px", 1.0, 5000.0),
        "z-index" => v.parse::<i32>().ok().filter(|n| (-50..=50).contains(n)).map(|n| n.to_string()),
        "display" => (v == "none").then(|| v.to_string()),
        _ => return Err(format!("不能在画板上改“{prop}”。")),
    };
    ok.ok_or_else(|| format!("“{prop}”不能设成“{value}”。"))
}

/// Checks a page address an edit may be limited to.
fn check_page(page: &str) -> Result<(), String> {
    (page.starts_with('/') && page.len() <= 500 && !page.chars().any(char::is_control)).then_some(()).ok_or_else(|| format!("“{page}”不是这个网站的页面地址。"))
}

/// The styles of one element by width: for every page (`page` None) or for one page only.
fn styles<'a>(el: &'a Value, page: Option<&str>) -> &'a Value {
    match page {
        Some(p) => &el["pages"][p],
        None => &el["style"],
    }
}

/// Sets (`Some`) or clears (`None`) one property of the element at `key` for `tier`, on every page using it
/// or (`page`) on that page only, with `fp` as its fingerprint. Returns the new document.
pub fn set_style(doc: &Value, key: &str, fp: &str, page: Option<&str>, tier: &str, prop: &str, value: Option<&str>) -> Result<Value, String> {
    if !TIERS.iter().any(|(t, _)| *t == tier) {
        return Err(format!("宽度只能是 all、desktop、tablet 或 mobile，不是“{tier}”。"));
    }
    if !PROPS.contains(&prop) {
        return Err(format!("不能在画板上改“{prop}”。"));
    }
    if let Some(p) = page {
        check_page(p)?;
    }
    let value = value.map(|v| check(prop, v)).transpose()?;
    let mut doc = if doc.is_object() { doc.clone() } else { json!({}) };
    if !doc["els"].is_object() {
        doc["els"] = json!({});
    }
    let Some(els) = doc["els"].as_object_mut() else { unreachable!() };
    let el = els.entry(key.to_string()).or_insert_with(|| json!({}));
    if !el.is_object() {
        *el = json!({});
    }
    el["fp"] = json!(fp);
    let ptr = match page {
        Some(p) => format!("/pages/{}/{tier}", p.replace('~', "~0").replace('/', "~1")),
        None => format!("/style/{tier}"),
    };
    // Make the path, then set or clear the property.
    let mut at = &mut *el;
    for part in ptr.trim_start_matches('/').split('/') {
        let part = part.replace("~1", "/").replace("~0", "~");
        if !at.is_object() {
            *at = json!({});
        }
        at = at.as_object_mut().map(|m| m.entry(part).or_insert_with(|| json!({}))).unwrap_or_else(|| unreachable!());
    }
    if !at.is_object() {
        *at = json!({});
    }
    match value {
        Some(v) => at[prop] = json!(v),
        None => {
            at.as_object_mut().map(|m| m.remove(prop));
        }
    }
    // Drop what became empty.
    let prune = |m: &mut Value| {
        if let Some(o) = m.as_object_mut() {
            o.retain(|_, t| t.as_object().is_some_and(|t| !t.is_empty()));
        }
    };
    prune(&mut el["style"]);
    if let Some(pages) = el["pages"].as_object_mut() {
        pages.values_mut().for_each(prune);
        pages.retain(|_, t| t.as_object().is_some_and(|t| !t.is_empty()));
    }
    let empty = |v: &Value| v.as_object().is_none_or(|o| o.is_empty());
    if empty(&el["pages"]) {
        el.as_object_mut().map(|o| o.remove("pages"));
    }
    if empty(&el["style"]) && el.get("pages").is_none() {
        els.remove(key);
    } else if el.get("style").is_none() {
        el["style"] = json!({});
    }
    Ok(doc)
}

/// The rule of one element's properties, or `None` when none of them is valid.
fn rule(key: &str, props: &Map<String, Value>) -> Option<String> {
    let decls: Vec<String> = props.iter().filter_map(|(p, v)| v.as_str().and_then(|v| check(p, v).ok()).map(|v| format!("{p}:{v} !important"))).collect();
    if decls.is_empty() {
        return None;
    }
    let mut extra = String::new();
    if props.contains_key("border-width") {
        extra.push_str(";border-style:solid !important");
    }
    if props.contains_key("z-index") {
        extra.push_str(";position:relative");
    }
    if props.contains_key("width") || props.contains_key("height") {
        extra.push_str(";box-sizing:border-box !important;max-width:none !important");
    }
    Some(format!("[data-hc=\"{}\"]{{{}{extra}}}", id(key), decls.join(";")))
}

/// The stylesheet of every hand edit that applies on the page at `page` (its address), or `None` when there
/// are none. Edits for that page only come after the ones for every page, so they win.
pub fn css(doc: &Value, page: &str) -> Option<String> {
    let els: &Map<String, Value> = doc["els"].as_object()?;
    let mut out = String::new();
    for (tier, media) in TIERS {
        let mut rules = String::new();
        for scope in [None, Some(page)] {
            for (key, el) in els {
                if let Some(r) = styles(el, scope)[tier].as_object().and_then(|props| rule(key, props)) {
                    rules.push_str(&r);
                }
            }
        }
        if rules.is_empty() {
            continue;
        }
        if media.is_empty() {
            out.push_str(&rules);
        } else {
            out.push_str(&format!("@media {media}{{{rules}}}"));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Where the canvas stylesheet goes: right before `</head>`, after the page's own styles; else where injected
/// tags go.
pub fn head_end(html: &str) -> usize {
    html.to_ascii_lowercase().find("</head").unwrap_or_else(|| crate::site::head_insert_at(html))
}

// ---- text ----

/// Replaces the text inside the element at position `n` of `src` with `text`. Only an element whose content
/// is plain text in the file (no tags but `<br>`, no template blocks) can be edited this way: anything else
/// comes from data or markup the page builds.
pub fn set_text(src: &str, n: usize, text: &str) -> Result<String, String> {
    let all = tags(src);
    let tag = all.get(n.wrapping_sub(1)).ok_or("这个元素已经不在文件里了：请刷新预览。")?;
    if UNMARKED.contains(&tag.name.as_str()) || RAW.contains(&tag.name.as_str()) {
        return Err("这个元素的文字不能在画板上改。".into());
    }
    let rest = &src[tag.open_end..];
    let close = rest.to_ascii_lowercase().find(&format!("</{}", tag.name)).ok_or("找不到这个元素的结尾：它的文字不能在画板上改。")?;
    let inner = &rest[..close];
    let plain = inner.replace("<br>", "").replace("<br/>", "").replace("<br />", "");
    if plain.contains('<') || plain.contains('{') {
        return Err("这段文字来自数据或模板代码，不能在画板上直接改：请在对话里说，或去内容里改。".into());
    }
    if text.chars().count() > 5000 {
        return Err("文字太长了（最多 5000 字）。".into());
    }
    let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('{', "&#123;").replace('\n', "<br>");
    Ok(format!("{}{}{}", &src[..tag.open_end], escaped, &rest[close..]))
}

// ---- elements ----

/// Elements that have no content and no closing tag.
const VOID: [&str; 14] = ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr", "param"];

/// Byte range of the whole element that `tag` opens, up to its matching closing tag; template blocks and
/// comments inside are skipped when matching.
pub fn element_range(src: &str, tag: &Tag) -> Option<(usize, usize)> {
    if VOID.contains(&tag.name.as_str()) || src[..tag.open_end].ends_with("/>") {
        return Some((tag.start, tag.open_end));
    }
    let name = &tag.name;
    let lower = src.to_ascii_lowercase();
    let boundary = |at: usize| lower[at..].chars().next().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-'));
    let mut depth = 1;
    let mut i = tag.open_end;
    while i < src.len() {
        let rest = &src[i..];
        let skip = [("{{", "}}"), ("{%", "%}"), ("{#", "#}"), ("<!--", "-->")].iter().find(|(o, _)| rest.starts_with(o));
        if let Some((_, close)) = skip {
            i += rest.find(close)? + close.len();
        } else if lower[i..].starts_with(&format!("</{name}")) && boundary(i + 2 + name.len()) {
            depth -= 1;
            let end = i + rest.find('>')? + 1;
            if depth == 0 {
                return Some((tag.start, end));
            }
            i = end;
        } else if lower[i..].starts_with(&format!("<{name}")) && boundary(i + 1 + name.len()) {
            let len = tag_end(rest);
            if !rest[..len].ends_with("/>") {
                depth += 1;
            }
            i += len;
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    None
}

/// Removes (`copy` false) or duplicates right after itself (`copy` true) the element at position `n`.
/// Returns the new source, and how the positions after it move: (first moved position, shift).
pub fn restructure(src: &str, n: usize, copy: bool) -> Result<(String, usize, isize), String> {
    let all = tags(src);
    let tag = all.get(n.wrapping_sub(1)).ok_or("这个元素已经不在文件里了：请刷新预览。")?;
    if UNMARKED.contains(&tag.name.as_str()) {
        return Err("这个元素不能复制或删除。".into());
    }
    let (start, end) = element_range(src, tag).ok_or("找不到这个元素的结尾：它不能复制或删除。")?;
    let inside = all.iter().filter(|t| t.start >= start && t.start < end).count();
    if copy {
        let out = format!("{}{}{}", &src[..end], &src[start..end], &src[end..]);
        Ok((out, n + inside, inside as isize))
    } else {
        Ok((format!("{}{}", &src[..start], &src[end..]), n, -(inside as isize)))
    }
}

/// Moves the edits of the file at `path` after a restructure: positions from `from` on shift by `by`; with a
/// negative shift, edits of the removed elements (`from` up to `from - by`) are dropped.
pub fn shift(doc: &Value, path: &str, from: usize, by: isize) -> Value {
    let mut doc = doc.clone();
    let Some(els) = doc["els"].as_object() else { return doc };
    let mut out = Map::new();
    for (key, el) in els {
        let moved = parse_src(key).filter(|(p, n)| *p == path && *n >= from).map(|(_, n)| n);
        match moved {
            Some(n) if by < 0 && n < from + by.unsigned_abs() => {}
            Some(n) => {
                out.insert(format!("{path}#{}", (n as isize + by) as usize), el.clone());
            }
            None => {
                out.insert(key.clone(), el.clone());
            }
        }
    }
    doc["els"] = Value::Object(out);
    doc
}

/// What the pen adds: one element each, plain markup with its own look, so it shows on any site.
pub fn new_element(kind: &str, en: bool) -> Option<&'static str> {
    Some(match kind {
        "text" if en => "<p style=\"margin:0\">Double-click to edit</p>",
        "button" if en => "<a href=\"#\" style=\"display:inline-block;padding:12px 24px;border-radius:999px;background:#1d1d1f;color:#fff;text-decoration:none\">Button</a>",
        "text" =>"<p style=\"margin:0\">双击改文字</p>",
        "rect" => "<div style=\"width:160px;height:100px;border-radius:8px;background:#e5e5ea\"></div>",
        "circle" => "<div style=\"width:100px;height:100px;border-radius:50%;background:#e5e5ea\"></div>",
        "line" => "<hr style=\"width:200px;margin:0;border:0;border-top:2px solid #1d1d1f\">",
        "button" => "<a href=\"#\" style=\"display:inline-block;padding:12px 24px;border-radius:999px;background:#1d1d1f;color:#fff;text-decoration:none\">按钮</a>",
        _ => return None,
    })
}

/// An uploaded file's address, the only image source the canvas writes into a template.
fn upload_url(url: &str) -> Result<&str, String> {
    let ok = url.starts_with("/uploads/") && !url.contains("..") && url.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b));
    ok.then_some(url).ok_or_else(|| format!("“{url}”不是上传的文件。"))
}

/// The markup of an image added with the pen.
pub fn image_element(url: &str) -> Result<String, String> {
    Ok(format!("<img src=\"{}\" alt=\"\" style=\"width:240px;height:auto\">", upload_url(url)?))
}

/// Points the `<img>` at position `n` to the uploaded file `url` (dropping its `srcset`, which would win).
pub fn replace_image(src: &str, n: usize, url: &str) -> Result<String, String> {
    let url = upload_url(url)?;
    let all = tags(src);
    let tag = all.get(n.wrapping_sub(1)).ok_or("这个元素已经不在文件里了：请刷新预览。")?;
    if tag.name != "img" {
        return Err("只有图片能换图片。".into());
    }
    // The tag's attributes, rewritten without src and srcset.
    let attrs = &src[tag.name_end..tag.open_end];
    let mut kept = String::new();
    let mut i = 0;
    let b = attrs.as_bytes();
    while i < b.len() {
        let rest = &attrs[i..];
        let lower = rest.to_ascii_lowercase();
        let name_len = rest.find(|c: char| c.is_whitespace() || c == '=' || c == '>' || c == '/').unwrap_or(rest.len());
        let starts_attr = i > 0 && b[i - 1].is_ascii_whitespace() && name_len > 0;
        if starts_attr && (lower[..name_len] == *"src" || lower[..name_len] == *"srcset") {
            // Skip the attribute and its value, with the space before it.
            while kept.ends_with(char::is_whitespace) {
                kept.pop();
            }
            let mut j = i + name_len;
            let after = attrs[j..].trim_start();
            if let Some(v) = after.strip_prefix('=') {
                let v = v.trim_start();
                j = attrs.len() - v.len();
                let end = match v.chars().next() {
                    Some(q @ ('"' | '\'')) => v[1..].find(q).map_or(v.len(), |e| e + 2),
                    _ => v.find(|c: char| c.is_whitespace() || c == '>').unwrap_or(v.len()),
                };
                j += end;
            }
            i = j;
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        kept.push(c);
        i += c.len_utf8();
    }
    Ok(format!("{} src=\"{url}\"{kept}{}", &src[..tag.name_end], &src[tag.open_end..]))
}

/// Elements that can hold what the pen adds.
const CONTAINERS: [&str; 13] = ["body", "main", "section", "article", "header", "footer", "aside", "nav", "div", "li", "form", "figure", "td"];

/// Adds a `kind` element as the last child of the element at position `n` (or of its nearest container that
/// is), with English placeholder text when `en`. Returns the new source and the new element's position.
pub fn add(src: &str, n: usize, kind: &str, en: bool) -> Result<(String, usize), String> {
    add_markup(src, n, new_element(kind, en).ok_or_else(|| format!("不能添加“{kind}”。"))?)
}

/// Adds `markup` (one element) where `add` would.
pub fn add_markup(src: &str, n: usize, markup: &str) -> Result<(String, usize), String> {
    let all = tags(src);
    let tag = all.get(n.wrapping_sub(1)).ok_or("这个元素已经不在文件里了：请刷新预览。")?;
    // The clicked element if it can hold children, else the innermost container around it in this file; with
    // none (a page that fills a layout's block), right after the clicked element.
    let container = all
        .iter()
        .rev()
        .filter(|t| CONTAINERS.contains(&t.name.as_str()) && t.start <= tag.start)
        .find(|t| element_range(src, t).is_some_and(|(_, end)| end >= tag.open_end));
    let at = match container {
        Some(c) => {
            let (_, end) = element_range(src, c).ok_or("找不到这个区块的结尾：这里放不了新元素。")?;
            src[..end].rfind("</").unwrap_or(end)
        }
        None => element_range(src, tag).ok_or("这里放不了新元素：请点在一个区块里面。")?.1,
    };
    let new_n = all.iter().filter(|t| t.start < at).count() + 1;
    Ok((format!("{}{markup}{}", &src[..at], &src[at..]), new_n))
}

/// Follows the edits of the file at `path` into its new source `new`: an edit stays where its element still
/// is, moves to the one element with its fingerprint, or is lost. Returns the document with moved edits
/// re-keyed (lost ones dropped) and the keys of the lost ones.
pub fn remap(doc: &Value, path: &str, new: &str) -> (Value, Vec<String>) {
    let mut doc = doc.clone();
    let Some(els) = doc["els"].as_object() else { return (doc, vec![]) };
    let found = resolve(&doc, path, new, &tags(new));
    let mut out = Map::new();
    let mut lost = vec![];
    for (key, el) in els {
        match parse_src(key) {
            Some((p, _)) if p == path => match found.iter().find(|(_, k)| *k == key) {
                Some((n, _)) => {
                    out.insert(format!("{path}#{n}"), el.clone());
                }
                None => lost.push(key.clone()),
            },
            _ => {
                out.insert(key.clone(), el.clone());
            }
        }
    }
    doc["els"] = Value::Object(out);
    (doc, lost)
}

/// The hand edits of one file, for the AI to see: element, what it is (tag and its first text), styles by width.
pub fn edits_of(doc: &Value, path: &str) -> Vec<Value> {
    doc["els"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| parse_src(k).is_some_and(|(p, _)| p == path))
        .map(|(k, el)| json!({ "element": k, "is": el["fp"], "style": el["style"] }))
        .collect()
}

/// The value one edit has now, for undo: `None` when it is not set.
pub fn current(doc: &Value, key: &str, page: Option<&str>, tier: &str, prop: &str) -> Option<String> {
    styles(&doc["els"][key], page)[tier][prop].as_str().map(String::from)
}

/// The widths (other than `all`) at which the element at `key` has edits of its own, on every page or on `page`.
pub fn tiers_of(doc: &Value, key: &str, page: &str) -> Vec<String> {
    let el = &doc["els"][key];
    TIERS
        .iter()
        .filter(|(t, _)| *t != "all")
        .filter(|(t, _)| [None, Some(page)].iter().any(|s| styles(el, *s)[*t].as_object().is_some_and(|m| !m.is_empty())))
        .map(|(t, _)| t.to_string())
        .collect()
}

/// Removes every edit the element at `key` has for `tier` (on every page and on each one), so it looks the
/// way the wider widths make it.
pub fn reset_tier(doc: &Value, key: &str, tier: &str) -> Value {
    let mut doc = doc.clone();
    let el = &mut doc["els"][key];
    if let Some(m) = el["style"].as_object_mut() {
        m.remove(tier);
    }
    if let Some(pages) = el["pages"].as_object_mut() {
        for p in pages.values_mut() {
            p.as_object_mut().map(|m| m.remove(tier));
        }
        pages.retain(|_, t| t.as_object().is_some_and(|t| !t.is_empty()));
    }
    let empty = |v: &Value| v.as_object().is_none_or(|o| o.is_empty());
    if empty(&el["style"]) && empty(&el["pages"]) {
        doc["els"].as_object_mut().map(|m| m.remove(key));
    }
    doc
}

/// How many pages show the file at `path`: the page files that are it or pull it in (through `extends`,
/// `include` or `import`), counting layouts and fragments only through the pages that use them. Record
/// page templates (`_item.html`) count once per collection using them.
pub fn pages_using(files: &[(String, String)], path: &str) -> usize {
    let refs: HashMap<&str, Vec<String>> = files.iter().map(|(p, s)| (p.as_str(), crate::site::referenced(s))).collect();
    let used: std::collections::HashSet<&str> = refs.values().flatten().map(String::as_str).collect();
    let reaches = |from: &str| {
        let mut seen = std::collections::HashSet::new();
        let mut todo = vec![from.to_string()];
        while let Some(f) = todo.pop() {
            if f == path {
                return true;
            }
            if seen.insert(f.clone()) {
                todo.extend(refs.get(f.as_str()).into_iter().flatten().cloned());
            }
        }
        false
    };
    files.iter().filter(|(p, _)| p.ends_with(".html") && (!used.contains(p.as_str()) || p.ends_with("_item.html")) && reaches(p)).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "<!doctype html><html><head><title>t</title><style>a<b{}</style></head><body>\n<h1 class=\"{{ c }}\">春季 新品</h1>{% if x %}<p>{{ \"<i>\" }}</p>{% endif %}<script>let a = '<div>';</script><a href=\"/\">去</a></body></html>";

    #[test]
    fn tags_skip_template_and_raw_text() {
        let names: Vec<String> = tags(PAGE).into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["html", "head", "title", "style", "body", "h1", "p", "script", "a"]);
    }

    #[test]
    fn annotate_marks_drawn_elements_and_edited_ones() {
        let out = annotate("index.html", PAGE, &json!({}), true);
        assert!(out.contains("<h1 data-hc-src=\"index.html#6\" class=\"{{ c }}\">"));
        assert!(out.contains("<a data-hc-src=\"index.html#9\" href"));
        assert!(!out.contains("<body data-hc"));
        assert_eq!(annotate("index.html", PAGE, &json!({}), false), PAGE);

        let doc = set_style(&json!({}), "index.html#6", "h1|春季 新品", None, "all", "color", Some("#FFF")).unwrap();
        let live = annotate("index.html", PAGE, &doc, false);
        assert!(live.contains(&format!("<h1 data-hc=\"{}\" class", id("index.html#6"))));
        assert_eq!(css(&doc, "/").unwrap(), format!("[data-hc=\"{}\"]{{color:#fff !important}}", id("index.html#6")));
    }

    #[test]
    fn an_edit_follows_its_element_or_applies_to_nothing() {
        let doc = set_style(&json!({}), "index.html#6", "h1|春季 新品", None, "mobile", "font-size", Some("20px")).unwrap();
        let moved = PAGE.replace("<body>", "<body><nav>x</nav>");
        let src = tags(&moved);
        assert_eq!(resolve(&doc, "index.html", &moved, &src).get(&7).map(String::as_str), Some("index.html#6"));
        let gone = PAGE.replace("春季 新品", "冬季");
        assert!(resolve(&doc, "index.html", &gone, &tags(&gone)).is_empty());
        assert!(css(&doc, "/").unwrap().starts_with("@media (max-width: 599.98px){"));
    }

    #[test]
    fn values_are_checked() {
        assert!(check("color", "red;}body{display:none").is_err());
        assert!(check("position", "fixed").is_err());
        assert_eq!(check("font-size", "54px").unwrap(), "54px");
        assert!(check("font-size", "5000px").is_err());
        assert!(check("box-shadow", "0 0 0 red").is_err());
        let doc = set_style(&json!({}), "a.html#1", "p|", None, "all", "color", Some("#123")).unwrap();
        let cleared = set_style(&doc, "a.html#1", "p|", None, "all", "color", None).unwrap();
        assert_eq!(cleared, json!({ "els": {} }));
        assert!(set_style(&doc, "a.html#1", "p|", None, "all", "position", None).is_err());
    }

    #[test]
    fn elements_are_removed_and_copied_whole() {
        let src = "<div><section><div>a{% if x %}</div>{% endif %}<img src=\"x\"><div>b</div></div></section><p>c</p></div>";
        let (copied, from, by) = restructure(src, 2, true).unwrap();
        assert_eq!(copied, "<div><section><div>a{% if x %}</div>{% endif %}<img src=\"x\"><div>b</div></div></section><section><div>a{% if x %}</div>{% endif %}<img src=\"x\"><div>b</div></div></section><p>c</p></div>");
        assert_eq!((from, by), (2 + 4, 4));
        let (removed, from, by) = restructure(src, 2, false).unwrap();
        assert_eq!(removed, "<div><p>c</p></div>");
        assert_eq!((from, by), (2, -4));
        let (no_img, ..) = restructure(src, 4, false).unwrap();
        assert!(no_img.contains("{% endif %}<div>b</div>"));

        let doc = set_style(&json!({}), "a.html#3", "div|a", None, "all", "color", Some("#000")).unwrap();
        let doc = set_style(&doc, "a.html#7", "p|c", None, "all", "color", Some("#111")).unwrap();
        let doc = set_style(&doc, "b.html#7", "p|c", None, "all", "color", Some("#222")).unwrap();
        let after = shift(&doc, "a.html", 2, -4);
        let keys: Vec<&String> = after["els"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["a.html#3", "b.html#7"].map(String::from).iter().collect::<Vec<_>>());
        assert_eq!(after["els"]["a.html#3"]["style"]["all"]["color"], "#111");
    }

    #[test]
    fn the_pen_adds_into_the_nearest_container() {
        let src = "<body><section><h2>甲</h2></section><p>乙</p></body>";
        let (out, n) = add(src, 3, "rect", false).unwrap();
        assert!(out.starts_with("<body><section><h2>甲</h2><div style="), "{out}");
        assert_eq!(n, 4);
        assert_eq!(tags(&out)[n - 1].name, "div");
        let (out, n) = add(src, 4, "text", false).unwrap();
        assert!(out.ends_with("<p>乙</p><p style=\"margin:0\">双击改文字</p></body>"), "{out}");
        assert_eq!(n, 5);
        assert!(add(src, 2, "script", false).is_err());
        // A page that only fills a layout's block: after the clicked element.
        let block = "{% extends 'l.html' %}{% block main %}<h1>甲</h1><p>乙</p>{% endblock %}";
        let (out, n) = add(block, 1, "line", false).unwrap();
        assert!(out.contains("<h1>甲</h1><hr style="), "{out}");
        assert_eq!(n, 2);
    }

    #[test]
    fn edits_can_be_for_one_page_or_one_width() {
        let doc = set_style(&json!({}), "l.html#3", "h1|甲", None, "all", "color", Some("#111")).unwrap();
        let doc = set_style(&doc, "l.html#3", "h1|甲", Some("/about"), "all", "color", Some("#222")).unwrap();
        let doc = set_style(&doc, "l.html#3", "h1|甲", Some("/about"), "mobile", "display", Some("none")).unwrap();
        let about = css(&doc, "/about").unwrap();
        assert!(about.find("#111").unwrap() < about.find("#222").unwrap(), "{about}");
        assert!(!css(&doc, "/").unwrap().contains("#222"));
        assert_eq!(current(&doc, "l.html#3", Some("/about"), "all", "color").as_deref(), Some("#222"));
        assert_eq!(tiers_of(&doc, "l.html#3", "/about"), ["mobile"]);
        assert!(tiers_of(&doc, "l.html#3", "/").is_empty());
        let reset = reset_tier(&doc, "l.html#3", "mobile");
        assert!(tiers_of(&reset, "l.html#3", "/about").is_empty());
        let cleared = set_style(&reset, "l.html#3", "h1|甲", Some("/about"), "all", "color", None).unwrap();
        assert!(cleared["els"]["l.html#3"].get("pages").is_none(), "{cleared}");
        assert!(set_style(&doc, "l.html#3", "h1|甲", Some("about"), "all", "color", Some("#000")).is_err());

        let files = [
            ("layout.html".to_string(), "<body>{% block m %}{% endblock %}</body>".to_string()),
            ("a.html".to_string(), "{% extends 'layout.html' %}".to_string()),
            ("b.html".to_string(), "{% extends \"layout.html\" %}{% include 'part.html' %}".to_string()),
            ("part.html".to_string(), "<p>x</p>".to_string()),
        ];
        assert_eq!(pages_using(&files, "layout.html"), 2);
        assert_eq!(pages_using(&files, "part.html"), 1);
        assert_eq!(pages_using(&files, "a.html"), 1);
    }

    #[test]
    fn a_rewrite_keeps_moves_or_loses_edits() {
        let src = "<body><h1>甲</h1><p>乙</p></body>";
        let doc = set_style(&json!({}), "a.html#2", "h1|甲", None, "all", "color", Some("#111")).unwrap();
        let doc = set_style(&doc, "a.html#3", "p|乙", None, "all", "color", Some("#222")).unwrap();
        let (moved, lost) = remap(&doc, "a.html", &src.replace("<body>", "<body><nav>x</nav>"));
        assert!(lost.is_empty());
        assert_eq!(moved["els"]["a.html#3"]["fp"], "h1|甲");
        assert_eq!(moved["els"]["a.html#4"]["fp"], "p|乙");
        let (kept, lost) = remap(&doc, "a.html", &src.replace("乙", "丙"));
        assert_eq!(lost, ["a.html#3"]);
        assert!(kept["els"].get("a.html#3").is_none() && kept["els"].get("a.html#2").is_some());
        assert_eq!(edits_of(&doc, "a.html").len(), 2);
    }

    #[test]
    fn images_are_uploads_only() {
        let src = "<div><img class=\"a\" srcset='x 1x, y 2x' src=\"{{ item.cover }}\" alt=\"封面\"><p>x</p></div>";
        let out = replace_image(src, 2, "/uploads/2026/a-b_c.webp").unwrap();
        assert_eq!(out, "<div><img src=\"/uploads/2026/a-b_c.webp\" class=\"a\" alt=\"封面\"><p>x</p></div>");
        assert!(replace_image(src, 3, "/uploads/a.png").is_err());
        assert!(replace_image(src, 2, "https://evil.example/a.png").is_err());
        assert!(replace_image(src, 2, "/uploads/a\" onerror=\"x").is_err());
        assert!(image_element("/uploads/../a.png").is_err());
        let (added, n) = add_markup(src, 1, &image_element("/uploads/a.png").unwrap()).unwrap();
        assert!(added.ends_with("<p>x</p><img src=\"/uploads/a.png\" alt=\"\" style=\"width:240px;height:auto\"></div>"));
        assert_eq!(n, 4);
    }

    #[test]
    fn positions_and_sizes_are_checked() {
        assert_eq!(check("translate", "12.345px -8px").unwrap(), "12.35px -8px");
        assert!(check("translate", "12px").is_err());
        assert!(check("translate", "1px 9999px").is_err());
        assert!(check("display", "block").is_err());
        let doc = set_style(&json!({}), "a.html#1", "p|", None, "desktop", "z-index", Some("2")).unwrap();
        assert!(css(&doc, "/").unwrap().contains("z-index:2 !important;position:relative}"));
    }

    #[test]
    fn text_edits_only_touch_plain_text() {
        let out = set_text(PAGE, 6, "夏季<新品>\n第二行").unwrap();
        assert!(out.contains(">夏季&lt;新品&gt;<br>第二行</h1>"));
        assert!(set_text(PAGE, 7, "x").is_err());
        assert!(set_text(PAGE, 8, "x").is_err());
        assert!(set_text(PAGE, 99, "x").is_err());
    }
}
