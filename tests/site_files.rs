mod common;
use axum::http::StatusCode;
use common::*;
use cms_harness::site::transpile;
use cms_harness::Shared;
use serde_json::{json, Value};
use std::io::Write;

fn write(app: &Shared, path: &str, content: &str) -> Value {
    ok(app, &builder(), "write_site_file", json!({ "path": path, "content": content, "note": "改文件" }))
}

fn write_err(app: &Shared, path: &str, content: &str) -> String {
    err(app, &builder(), "write_site_file", json!({ "path": path, "content": content, "note": "改文件" }))
}

fn content(app: &Shared, path: &str) -> Value {
    ok(app, &reader(), "get_site_file", json!({ "path": path }))
}

fn listed(app: &Shared) -> Vec<(String, String)> {
    ok(app, &reader(), "list_site_files", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["path"].as_str().unwrap().to_string(), f["status"].as_str().unwrap().to_string()))
        .collect()
}

fn versions(app: &Shared, target: &str) -> Vec<Value> {
    ok(app, &reader(), "list_versions", json!({ "target": target })).as_array().unwrap().clone()
}

#[test]
fn site_files_follow_draft_publish_history_restore_and_discard() {
    let app = app();
    assert_eq!(write(&app, "index.html", "<h1>v1</h1>")["status"], "draft");
    assert_eq!(listed(&app), vec![("index.html".to_string(), "draft".to_string())]);
    assert_eq!(content(&app, "index.html")["content"], "<h1>v1</h1>");
    publish_all(&app);
    assert_eq!(content(&app, "index.html")["status"], "published");

    // Writing what is already live leaves nothing pending.
    write(&app, "index.html", "<h1>v1</h1>");
    assert_eq!(ok(&app, &reader(), "list_pending", json!({})), json!([]));

    write(&app, "index.html", "<h1>v2</h1>\n<p>more</p>");
    let pending = ok(&app, &reader(), "list_pending", json!({}));
    assert_eq!(pending[0]["target"], "file:index.html");
    assert_eq!(pending[0]["title"], "index.html");
    assert_eq!(pending[0]["lines"], json!([1, 2]));
    ok(&app, &builder(), "discard_draft", json!({ "target": "file:index.html" }));
    assert_eq!(content(&app, "index.html")["content"], "<h1>v1</h1>");
    assert_eq!(content(&app, "index.html")["status"], "published");

    write(&app, "index.html", "<h1>v2</h1>");
    publish_all(&app);
    let history = versions(&app, "file:index.html");
    assert_eq!(history.len(), 2);
    let first = history[1]["id"].as_i64().unwrap();
    ok(&app, &builder(), "restore_version", json!({ "target": "file:index.html", "version_id": first, "note": "回到 v1" }));
    assert_eq!(content(&app, "index.html")["content"], "<h1>v1</h1>");
    assert_eq!(content(&app, "index.html")["status"], "changed");
    publish_all(&app);
    assert_eq!(content(&app, "index.html")["content"], "<h1>v1</h1>");

    assert_eq!(ok(&app, &builder(), "delete_site_file", json!({ "path": "index.html", "note": "不要了" }))["status"], "deleting");
    publish_all(&app);
    assert_eq!(listed(&app), vec![]);
    assert!(err(&app, &reader(), "get_site_file", json!({ "path": "index.html" })).contains("index.html"));
    assert!(err(&app, &builder(), "delete_site_file", json!({ "path": "index.html", "note": "n" })).contains("index.html"));
    // A deleted file comes back from its history.
    ok(&app, &builder(), "restore_version", json!({ "target": "file:index.html", "version_id": first, "note": "找回" }));
    assert_eq!(content(&app, "index.html")["status"], "draft");

    // A file never published disappears when deleted or discarded.
    write(&app, "a.css", "p{}");
    ok(&app, &builder(), "delete_site_file", json!({ "path": "a.css", "note": "n" }));
    write(&app, "b.css", "p{}");
    ok(&app, &builder(), "discard_draft", json!({ "target": "file:b.css" }));
    assert_eq!(listed(&app), vec![("index.html".to_string(), "draft".to_string())]);
}

#[test]
fn paths_follow_the_rules() {
    let app = app();
    let long = format!("{}.html", "a".repeat(196));
    for bad in ["/a.html", "../a.html", "a/../b.html", "a..b.html", "a b.html", "a", "a.exe", "a//b.html", "dir/", "./a.html",
                ".html", "a:b.html", "中.html", "a.HTML", &long, "admin/x.html", "api/v1.json", "uploads/a.png", "_hc/preview.js", "mcp"] {
        let e = write_err(&app, bad, "x");
        assert!(e.contains("路径"), "{bad}: {e}");
    }
    assert!(write_err(&app, "_hc/x.css", "x").contains("_hc/"));
    for good in ["a.html", "A.html", "assets/index-DiwrgTda.js", ".well-known/security.txt", "sub_dir/site.webmanifest", "x/y/z.map", "feed.xml",
                 "r.md", "i.svg", "t.txt", "m.mjs", "s.ts", "administrator.html", "apis/x.css", &format!("{}.html", "a".repeat(195))] {
        let body = if good.ends_with(".map") || good.ends_with(".webmanifest") { "{}" } else { "x" };
        write(&app, good, body);
    }
    // Paths are case-sensitive: two files.
    write(&app, "a.html", "lower");
    assert_eq!(content(&app, "A.html")["content"], "x");
    assert!(err(&app, &builder(), "discard_draft", json!({ "target": "file:../x.html" })).contains("file:"));
}

#[test]
fn binary_files_are_stored_as_base64() {
    use base64::Engine;
    let app = app();
    let bytes: Vec<u8> = (0..=255u8).cycle().take(3000).collect();
    let enc = base64::engine::general_purpose::STANDARD.encode(&bytes);
    // Whitespace in the base64 (line breaks) is accepted and dropped.
    let wrapped: String = enc.as_bytes().chunks(76).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("\n");
    write(&app, "fonts/Inter.woff2", &wrapped);
    let got = content(&app, "fonts/Inter.woff2");
    assert_eq!((got["content"].as_str(), got["binary"].as_bool()), (Some(enc.as_str()), Some(true)), "{got}");
    for ext in ["png", "jpg", "jpeg", "gif", "webp", "avif", "ico", "woff", "ttf", "otf", "mp4", "webm"] {
        write(&app, &format!("f.{ext}"), "AAAA");
    }
    assert!(write_err(&app, "x.png", "not base64!").contains("base64"));
    assert!(write_err(&app, "x.png", "").contains("x.png"));
    let big = base64::engine::general_purpose::STANDARD.encode(vec![0u8; (5 << 20) + 1]);
    assert!(write_err(&app, "big.mp4", &big).contains("5 MB"));
    let listed_file = ok(&app, &reader(), "list_site_files", json!({})).as_array().unwrap().iter().find(|f| f["path"] == "fonts/Inter.woff2").unwrap().clone();
    assert_eq!((listed_file["bytes"].as_u64(), listed_file["binary"].as_bool()), (Some(3000), Some(true)), "{listed_file}");
    let pending = ok(&app, &reader(), "list_pending", json!({}));
    let p = pending.as_array().unwrap().iter().find(|p| p["target"] == "file:fonts/Inter.woff2").unwrap();
    assert!(p.get("lines").is_none(), "{p}");
    publish_all(&app);
}

#[test]
fn content_is_checked_on_write() {
    let app = app();
    let e = write_err(&app, "index.html", "<p>\n\n{% if x %}\n<p>");
    assert!(e.contains("index.html") && e.contains("第 3 行"), "{e}");
    let e = write_err(&app, "page.html", "a\n{{ x | }}\n");
    assert!(e.contains("第 2 行"), "{e}");
    let e = write_err(&app, "app.tsx", "const a = 1;\nconst b = <div>;\n");
    assert!(e.contains("app.tsx") && e.contains("第 2 行") && e.contains("列"), "{e}");
    let e = write_err(&app, "lib.ts", "let a: number = 1;\nlet b = 2;\n  let = ;\n");
    assert!(e.contains("第 3 行第"), "{e}");
    assert!(write_err(&app, "x.js", "function (").contains("x.js"));
    assert!(write_err(&app, "x.mjs", "export default {").contains("x.mjs"));
    assert!(write_err(&app, "x.jsx", "<a>").contains("x.jsx"));
    let e = write_err(&app, "data.json", "{\n\"a\": 1,\n}");
    assert!(e.contains("data.json") && e.contains("JSON") && e.contains("第 3 行"), "{e}");
    assert!(write_err(&app, "big.css", &"a".repeat((1 << 20) + 1)).contains("1 MB"));
    write(&app, "ok.css", &"a".repeat(1 << 20));
    write(&app, "any.css", "this is { not css but fine");
    write(&app, "page.html", "{% extends \"layout.html\" %}{% block main %}{{ item.title }}{% endblock %}");
    write(&app, "app.tsx", "export const A = (p: { n: number }) => <b>{p.n}</b>;");
    write(&app, "data.json", "{\"a\": [1, 2]}");
    let e = err(&app, &builder(), "write_site_file", json!({ "path": "x.css", "content": "a", "note": " " }));
    assert!(e.contains("note"), "{e}");
}

#[test]
fn at_most_two_thousand_files() {
    let app = app();
    for i in 0..2000 {
        write(&app, &format!("f/{i}.txt"), "x");
    }
    assert!(write_err(&app, "one-more.txt", "x").contains("2000"));
    // Rewriting an existing file is fine.
    write(&app, "f/7.txt", "y");
    ok(&app, &builder(), "delete_site_file", json!({ "path": "f/7.txt", "note": "n" }));
    write(&app, "one-more.txt", "x");
    // A live file rewritten and then deleted frees its slot too.
    publish_all(&app);
    write(&app, "f/8.txt", "changed");
    ok(&app, &builder(), "delete_site_file", json!({ "path": "f/8.txt", "note": "n" }));
    write(&app, "two-more.txt", "x");
}

#[test]
fn transpile_strips_types_and_uses_the_automatic_jsx_runtime() {
    let src = "import { useState } from 'react';\ntype P = { n: number };\ninterface Q { m: string }\nexport function App(p: P) {\n  const [c] = useState<number>(0);\n  return <div class=\"x\">{p.n + c}</div>;\n}\n";
    let out = transpile("app.tsx", src).unwrap();
    assert!(out.contains("react/jsx-runtime"), "{out}");
    assert!(!out.contains("type P") && !out.contains("interface") && !out.contains(": P") && !out.contains("<number>"), "{out}");
    assert!(!out.contains("<div"), "{out}");
    // The output is plain JavaScript.
    transpile("out.js", &out).unwrap();
    let jsx = transpile("a.jsx", "export default () => <p>hi</p>;").unwrap();
    assert!(jsx.contains("react/jsx-runtime") && !jsx.contains("<p>"), "{jsx}");
    let ts = transpile("a.ts", "export const n: number = 1;").unwrap();
    assert!(!ts.contains(": number"), "{ts}");
    // Plain modules pass through untouched.
    let js = "import x from 'https://esm.sh/three';\nexport const y = x;   // keep\n";
    assert_eq!(transpile("a.js", js).unwrap(), js);
    assert_eq!(transpile("a.mjs", js).unwrap(), js);
    let e = transpile("bad.tsx", "\n\n  let x = <div>;").unwrap_err();
    assert!(e.contains("第 3 行"), "{e}");
    // Classic scripts (vendor files) are accepted as .js.
    transpile("old.js", "var await = 1;\nwith (Math) { var x = PI; }\n").unwrap();
    // Modern syntax is kept as written: no lowering, no helper imports the browser could not resolve.
    let out = transpile("m.ts", "class A { #p = 1; f: number = 2; get(o?: { a?: number }) { return o?.a ?? this.#p; } }\nexport { A };").unwrap();
    assert!(out.contains("?.") && out.contains("??") && out.contains("#p") && !out.contains("import"), "{out}");
}

#[test]
fn pages_render_with_their_layout_and_escape_values() {
    let app = app();
    write(&app, "layout.html", "<main>LAYOUT {% block body %}{% endblock %}</main>");
    write(&app, "index.html", "{% extends \"layout.html\" %}{% block body %}{{ request.path }} {{ request.query.q }}{% endblock %}");
    publish_all(&app);
    let ctx = json!({ "request": { "path": "/", "query": { "q": "<b>" } } });
    let out = cms_harness::site::render(&app.conn(), &app.data_dir.join("uploads"), cms_harness::site::Mode::Live, "index.html", ctx).unwrap();
    assert_eq!(out.html, "<main>LAYOUT &#x2f; &lt;b&gt;</main>");
}

#[test]
fn a_page_that_fails_at_render_time_blocks_publish() {
    let app = app();
    write(&app, "index.html", "<main>{% include \"parts/missing.html\" %}</main>");
    let e = err(&app, &builder(), "publish", json!({ "note": "上线" }));
    assert!(e.contains("file:index.html") && e.contains("parts/missing.html"), "{e}");
    assert_eq!(listed(&app), vec![("index.html".to_string(), "draft".to_string())]);
    write(&app, "parts/missing.html", "<p>{{ request.path }}</p>");
    publish_all(&app);

    // A runtime error in a template's logic blocks too.
    write(&app, "math.html", "{{ 1 + \"a\" }}");
    let e = err(&app, &builder(), "publish", json!({ "note": "上线" }));
    assert!(e.contains("file:math.html"), "{e}");
}

#[test]
fn a_missing_variable_does_not_block_publish() {
    let app = app();
    write(&app, "index.html", "<h1>{{ page.title }}</h1>{{ item.author.name }}{% for x in items %}{{ x }}{% endfor %}{{ request.path }}");
    publish_all(&app);
}

#[test]
fn deleting_a_layout_that_a_live_page_extends_blocks_publish() {
    let app = app();
    write(&app, "layout.html", "<html>{% block main %}{% endblock %}</html>");
    write(&app, "index.html", "{% extends \"layout.html\" %}{% block main %}hi{% endblock %}");
    publish_all(&app);
    ok(&app, &builder(), "delete_site_file", json!({ "path": "layout.html", "note": "n" }));
    let e = err(&app, &builder(), "publish", json!({ "note": "n" }));
    assert!(e.contains("file:index.html") && e.contains("layout.html"), "{e}");
}

#[test]
fn a_page_cannot_go_live_without_the_draft_layout_it_extends() {
    let app = app();
    write(&app, "layout.html", "<html>{% block main %}{% endblock %}</html>");
    write(&app, "index.html", "{% extends \"layout.html\" %}{% block main %}hi{% endblock %}");
    let e = err(&app, &builder(), "publish", json!({ "targets": ["file:index.html"], "note": "n" }));
    assert!(e.contains("file:index.html"), "{e}");
    let done = ok(&app, &builder(), "publish", json!({ "targets": ["file:index.html", "file:layout.html"], "note": "n" }));
    assert_eq!(done["published"], json!(["file:index.html", "file:layout.html"]));
}

#[test]
fn a_live_script_that_no_longer_transpiles_blocks_publish() {
    let app = app();
    write(&app, "app.tsx", "export const a = <b>x</b>;");
    publish_all(&app);
    // Sneak a broken copy past the write check, as an older version could have stored.
    app.conn().execute("UPDATE site_files SET published = ?1 WHERE path = 'app.tsx'", [json!("export const = ;").to_string()]).unwrap();
    write(&app, "x.css", "p{}");
    let e = err(&app, &builder(), "publish", json!({ "note": "n" }));
    assert!(e.contains("file:app.tsx") && e.contains("第 1 行"), "{e}");
}

#[test]
fn editors_cannot_change_site_files() {
    let app = app();
    let e = editor();
    assert!(err(&app, &e, "write_site_file", json!({ "path": "a.html", "content": "x", "note": "n" })).contains("structure"));
    write(&app, "a.html", "x");
    assert!(err(&app, &e, "delete_site_file", json!({ "path": "a.html", "note": "n" })).contains("structure"));
    assert!(err(&app, &e, "upload_site_bundle", json!({ "zip_base64": "", "note": "n" })).contains("structure"));
    assert!(err(&app, &e, "discard_draft", json!({ "target": "file:a.html" })).contains("structure"));
    assert!(err(&app, &e, "publish", json!({ "targets": ["file:a.html"], "note": "n" })).contains("structure"));
    assert!(err(&app, &e, "publish", json!({ "note": "n" })).contains("file:a.html"));
    publish_all(&app);
    let v = versions(&app, "file:a.html")[0]["id"].as_i64().unwrap();
    assert!(err(&app, &e, "restore_version", json!({ "target": "file:a.html", "version_id": v, "note": "n" })).contains("structure"));
    // Reading is open to everyone.
    assert_eq!(ok(&app, &reader(), "get_site_file", json!({ "path": "a.html" }))["content"], "x");
    assert_eq!(ok(&app, &reader(), "list_site_files", json!({}))[0]["path"], "a.html");
}

// ---- bundles ----

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        if name.ends_with('/') {
            w.add_directory(*name, opts).unwrap();
        } else {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
    }
    w.finish().unwrap().into_inner()
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn upload(app: &Shared, zip: &[u8]) -> Result<Value, String> {
    cms_harness::tools::call(app, &builder(), "upload_site_bundle", json!({ "zip_base64": b64(zip), "note": "整包上传" }))
}

#[test]
fn a_bundle_carries_binary_assets_and_can_replace_the_site() {
    let app = app();
    write(&app, "old.html", "<p>old</p>");
    write(&app, "index.html", "<p>v1</p>");
    write(&app, "draft-only.css", "p{}");
    ok(&app, &builder(), "publish", json!({ "targets": ["file:old.html", "file:index.html"], "note": "n" }));
    let png = b"\x89PNG\r\n\x1a\n\x00\xff";
    let zip = zip_of(&[("dist/index.html", b"<p>v2</p>"), ("dist/assets/Logo-AbC.png", png), ("dist/assets/index-DiwrgTda.js", b"export {};")]);
    // Without replace, files outside the bundle stay.
    let r = upload(&app, &zip).unwrap();
    assert_eq!((r["written"].as_i64(), r["deleted"].as_i64()), (Some(3), Some(0)), "{r}");
    assert_eq!(content(&app, "old.html")["status"], "published");
    assert_eq!(content(&app, "assets/Logo-AbC.png")["content"], b64(png));
    // With replace, live files outside the bundle get a pending deletion in the same step; drafts stay.
    let r = cms_harness::tools::call(&app, &builder(), "upload_site_bundle", json!({ "zip_base64": b64(&zip), "note": "替换", "replace": true })).unwrap();
    assert_eq!((r["written"].as_i64(), r["deleted"].as_i64()), (Some(0), Some(1)), "{r}");
    assert_eq!(content(&app, "old.html")["status"], "deleting");
    assert_eq!(content(&app, "draft-only.css")["status"], "draft");
    publish_all(&app);
    assert!(err(&app, &reader(), "get_site_file", json!({ "path": "old.html" })).contains("old.html"));
    // A binary entry has its own size limit.
    let e = upload(&app, &zip_of(&[("v.mp4", &vec![0u8; (5 << 20) + 1])])).unwrap_err();
    assert!(e.contains("v.mp4") && e.contains("5 MB"), "{e}");
}

#[test]
fn a_bundle_becomes_drafts_in_one_go() {
    let app = app();
    write(&app, "keep.txt", "untouched");
    let zip = zip_of(&[
        ("dist/", b""),
        ("dist/index.html", b"<script type=\"module\" src=\"/assets/app.js\"></script>"),
        ("dist/assets/", b""),
        ("dist/assets/app.tsx", b"export const A = () => <b>hi</b>;"),
        ("dist/.DS_Store", b"\0\0junk"),
        ("__MACOSX/dist/._index.html", b"\0\0junk"),
    ]);
    let r = upload(&app, &zip).unwrap();
    assert_eq!(r["written"], 2, "{r}");
    let mut files = listed(&app);
    files.sort();
    assert_eq!(files, vec![
        ("assets/app.tsx".to_string(), "draft".to_string()),
        ("index.html".to_string(), "draft".to_string()),
        ("keep.txt".to_string(), "draft".to_string()),
    ]);
    publish_all(&app);
    // The same bundle again changes nothing.
    assert_eq!(upload(&app, &zip).unwrap()["written"], 0);
    assert_eq!(ok(&app, &reader(), "list_pending", json!({})), json!([]));
}

#[test]
fn one_bad_file_rejects_the_whole_bundle_with_every_problem() {
    let app = app();
    let zip = zip_of(&[
        ("index.html", b"<p>fine</p>"),
        ("Bad.HTML", b"x"),
        ("app.tsx", b"const = ;"),
        ("blob.txt", &[0xff, 0xfe, 0x00]),
        ("../evil.html", b"x"),
        ("api/x.json", b"{}"),
        ("logo.png", b""),
    ]);
    let e = upload(&app, &zip).unwrap_err();
    for needle in ["Bad.HTML", "app.tsx", "blob.txt", "../evil.html", "api/x.json", "logo.png"] {
        assert!(e.contains(needle), "{needle}: {e}");
    }
    assert_eq!(listed(&app), vec![]);
}

#[test]
fn bundles_are_size_limited() {
    let app = app();
    // One entry over 1 MB (it compresses to almost nothing).
    let big = vec![b'a'; (1 << 20) + 1];
    let e = upload(&app, &zip_of(&[("a.txt", &big)])).unwrap_err();
    assert!(e.contains("a.txt") && e.contains("1 MB"), "{e}");

    // Over 50 MB in all, every entry within its own limit.
    let mb = vec![b'a'; 1 << 20];
    let names: Vec<String> = (0..51).map(|i| format!("f{i}.txt")).collect();
    let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &mb[..])).collect();
    let e = upload(&app, &zip_of(&entries)).unwrap_err();
    assert!(e.contains("50 MB"), "{e}");

    // More than 2000 files.
    let names: Vec<String> = (0..2001).map(|i| format!("f{i}.txt")).collect();
    let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
    let e = upload(&app, &zip_of(&entries)).unwrap_err();
    assert!(e.contains("2000"), "{e}");

    // A header that understates the size is caught while reading.
    let mut zip = zip_of(&[("liar.txt", &vec![b'a'; 3 << 20])]);
    let n = zip.len();
    for sig in [[0x50u8, 0x4b, 0x03, 0x04], [0x50, 0x4b, 0x01, 0x02]] {
        let at = (0..n - 4).find(|&i| zip[i..i + 4] == sig).unwrap();
        let field = if sig[2] == 3 { at + 22 } else { at + 24 };
        zip[field..field + 4].copy_from_slice(&10u32.to_le_bytes());
    }
    let e = upload(&app, &zip).unwrap_err();
    assert!(e.contains("liar.txt"), "{e}");

    assert!(upload(&app, b"not a zip").unwrap_err().contains("zip"));
    assert!(cms_harness::tools::call(&app, &builder(), "upload_site_bundle", json!({ "zip_base64": "***", "note": "n" })).unwrap_err().contains("base64"));
    assert_eq!(listed(&app), vec![]);
}

#[test]
fn a_bundle_counts_toward_the_file_limit() {
    let app = app();
    for i in 0..1999 {
        write(&app, &format!("f/{i}.txt"), "x");
    }
    let e = upload(&app, &zip_of(&[("a.txt", b"x"), ("b.txt", b"x")])).unwrap_err();
    assert!(e.contains("2000"), "{e}");
    upload(&app, &zip_of(&[("a.txt", b"x"), ("f/1.txt", b"y")])).unwrap();
}

// ---- the publish review ----

#[tokio::test]
async fn the_publish_review_shows_paths_and_line_counts_not_content() {
    let app = app();
    write(&app, "old.html", "a\nb\nc");
    write(&app, "gone.css", "p{}\nq{}");
    publish_all(&app);
    write(&app, "old.html", "a\nb\nc\nd\ne SECRET_LINE");
    write(&app, "new.js", "export const x = 'SECRET_NEW';\n// two");
    ok(&app, &builder(), "delete_site_file", json!({ "path": "gone.css", "note": "删样式" }));
    let session = admin_session(&app, "builder").await;
    let (status, _, page) = get_with(&app, "/admin/publish", Some(&session.0)).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    for needle in ["站点文件", "old.html", "new.js", "gone.css", "3 行 → 5 行（+2）", "共 2 行", "原有 2 行", "检查通过"] {
        assert!(page.contains(needle), "{needle}: {page}");
    }
    assert!(!page.contains("SECRET"), "{page}");

    let (status, _, _) = post_form(&app, "/admin/publish", Some(&session), &[("target", "file:old.html"), ("note", "发布")]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, _, history) = get_with(&app, "/admin/releases", Some(&session.0)).await;
    assert!(history.contains("old.html") && !history.contains("SECRET"), "{history}");
}
