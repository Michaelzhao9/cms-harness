//! Site files: the built-in website's text sources. Path rules, the checks a file passes on write, the script
//! transpiler and the page render that publish verification and the site server share.

use minijinja::{Environment, UndefinedBehavior};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Arc;

/// Extensions a site file may have. Images and fonts are binary: they go through media uploads.
pub const EXTENSIONS: [&str; 14] = ["html", "css", "js", "mjs", "jsx", "ts", "tsx", "json", "svg", "txt", "md", "xml", "webmanifest", "map"];
pub const MAX_PATH: usize = 200;
pub const MAX_FILE_BYTES: usize = 1 << 20;
pub const MAX_FILES: usize = 2_000;
/// Uncompressed total of one bundle.
pub const MAX_BUNDLE_BYTES: usize = 50 << 20;

fn ext(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, e)| e)
}

fn is_script(path: &str) -> bool {
    matches!(ext(path), "js" | "mjs" | "jsx" | "ts" | "tsx")
}

/// Relative, `[a-z0-9._/-]`, no `..`, no empty or `.` segment, at most 200 characters, an allowed extension.
pub fn check_path(path: &str) -> Result<(), String> {
    let name = path.rsplit('/').next().unwrap_or_default();
    let why = if path.is_empty() {
        "不能为空".to_string()
    } else if path.len() > MAX_PATH {
        format!("最长 {MAX_PATH} 个字符，现在有 {} 个", path.len())
    } else if path.starts_with('/') {
        "必须是相对路径，不能以 / 开头".into()
    } else if !path.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._/-".contains(&b)) {
        "只能用小写字母、数字和 . _ / - 这几个符号".into()
    } else if path.contains("..") {
        "不能包含 ..".into()
    } else if path.split('/').any(|s| s.is_empty() || s == ".") {
        "不能有空的路径段或只有 . 的路径段，也不能以 / 结尾".into()
    } else if !matches!(name.rsplit_once('.'), Some((stem, e)) if !stem.is_empty() && EXTENSIONS.contains(&e)) {
        format!("扩展名必须是 {} 之一（图片、字体等二进制文件请用 upload_media 上传）", EXTENSIONS.join("、"))
    } else {
        return Ok(());
    };
    Err(format!("站点文件路径“{path}”不合法：{why}。"))
}

/// The checks every stored file passes: its path, its size, and that it parses as what its extension says.
pub fn check_file(path: &str, content: &str) -> Result<(), String> {
    check_path(path)?;
    if content.len() > MAX_FILE_BYTES {
        return Err(format!("站点文件“{path}”太大：每个文件最多 1 MB（{MAX_FILE_BYTES} 字节），现在有 {} 字节。", content.len()));
    }
    match ext(path) {
        "html" => environment(Arc::default()).template_from_named_str(path, content).map(|_| ()).map_err(|e| format!("站点文件“{path}”的模板写错了：{}", template_error(&e))),
        "json" | "webmanifest" | "map" => serde_json::from_str::<serde::de::IgnoredAny>(content)
            .map(|_| ())
            .map_err(|e| format!("站点文件“{path}”不是有效的 JSON：第 {} 行第 {} 列，{e}。", e.line(), e.column())),
        _ if is_script(path) => transpile(path, content).map(|_| ()),
        _ => Ok(()),
    }
}

/// 1-based line and column (in characters) of a byte offset.
fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let before = source.get(..offset.min(source.len())).unwrap_or(source);
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    (before.matches('\n').count() + 1, before[line_start..].chars().count() + 1)
}

fn script_error(path: &str, source: &str, errors: &[oxc::diagnostics::OxcDiagnostic]) -> String {
    let first = &errors[0];
    let at = first.labels.first().map(|l| line_col(source, l.offset() as usize));
    let at = at.map_or_else(String::new, |(l, c)| format!("第 {l} 行第 {c} 列，"));
    let more = if errors.len() > 1 { format!("（另有 {} 处错误）", errors.len() - 1) } else { String::new() };
    format!("站点文件“{path}”的脚本写错了：{at}{}{more}。", first.message)
}

/// A script as the browser runs it: TypeScript types stripped, JSX compiled for the automatic runtime
/// (imports from `react/jsx-runtime`). `.js` and `.mjs` come back unchanged once they parse.
pub fn transpile(path: &str, source: &str) -> Result<String, String> {
    use oxc::{allocator::Allocator, codegen::Codegen, diagnostics::Severity, parser::Parser, semantic::SemanticBuilder, span::SourceType};
    use oxc::transformer::{TransformOptions, Transformer};
    let source_type = SourceType::from_path(path).map_err(|_| format!("“{path}”不是脚本文件。"))?;
    // A `.js` file may be a classic script (a vendor library); everything else is served as a module.
    let source_type = if ext(path) == "js" { source_type.with_unambiguous(true) } else { source_type.with_module(true) };
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(script_error(path, source, &parsed.diagnostics));
    }
    if matches!(ext(path), "js" | "mjs") {
        return Ok(source.to_string());
    }
    let mut program = parsed.program;
    let scoping = SemanticBuilder::new().build(&program).semantic.into_scoping();
    let done = Transformer::new(&allocator, std::path::Path::new(path), &TransformOptions::default()).build_with_scoping(scoping, &mut program);
    let errors: Vec<_> = done.diagnostics.into_iter().filter(|d| d.severity == Severity::Error).collect();
    if !errors.is_empty() {
        return Err(script_error(path, source, &errors));
    }
    Ok(Codegen::new().build(&program).code)
}

// ---- pages ----

/// Site files by path: what a render can see (the live set, or the draft set for a preview).
pub type Files = Arc<BTreeMap<String, String>>;

/// The site's template environment over one set of files; `{% extends %}` and `{% include %}` name other files
/// by path. `.html` output is escaped.
fn environment(files: Files) -> Environment<'static> {
    let mut env = Environment::new();
    // A page is rendered with whatever context its route has: a value it does not get renders empty.
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    env.set_loader(move |name| Ok(files.get(name).cloned()));
    env
}

fn template_error(e: &minijinja::Error) -> String {
    let at = match (e.name(), e.line()) {
        (Some(n), Some(l)) => format!("{n} 第 {l} 行，"),
        (None, Some(l)) => format!("第 {l} 行，"),
        _ => String::new(),
    };
    let detail = e.detail().map_or_else(|| e.kind().to_string(), String::from);
    format!("{at}{detail}。")
}

/// What every page sees. `request` is `{path, query}`. The site's data arrives here (Task 6); `conn` reads
/// through the caller's connection, so a render inside the publish transaction sees what is about to go live.
fn page_context(_conn: &Connection, request: Value) -> Value {
    json!({ "request": request })
}

/// Renders the page at `path` from `files`.
pub fn render(conn: &Connection, files: &Files, path: &str, request: Value) -> Result<String, String> {
    let env = environment(files.clone());
    env.get_template(path).and_then(|t| t.render(page_context(conn, request))).map_err(|e| template_error(&e))
}

/// Every live file, by path.
pub fn live_files(conn: &Connection) -> Result<Files, String> {
    let fail = |e: rusqlite::Error| format!("读取站点文件失败：{e}");
    let mut stmt = conn.prepare("SELECT path, published FROM site_files WHERE published IS NOT NULL").map_err(fail)?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(fail)?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (path, json) = row.map_err(fail)?;
        let content: String = serde_json::from_str(&json).map_err(|e| format!("站点文件“{path}”已损坏：{e}"))?;
        out.insert(path, content);
    }
    Ok(Arc::new(out))
}

/// After a release, every live page renders and every live script transpiles, whether or not the release touched it.
// ponytail: renders and transpiles every live file on each publish and each review-page dry run; cache results by
// content hash if sites grow to hundreds of large files.
pub fn check_live(conn: &Connection, problems: &mut Vec<String>) -> Result<(), String> {
    let files = live_files(conn)?;
    for (path, content) in files.iter() {
        let result = if ext(path) == "html" {
            render(conn, &files, path, json!({ "path": format!("/{path}"), "query": {} })).map(|_| ())
        } else if is_script(path) {
            transpile(path, content).map(|_| ())
        } else {
            continue;
        };
        if let Err(e) = result {
            problems.push(format!("file:{path} 上线后会出错：{}", e.trim_end_matches('。')));
        }
    }
    Ok(())
}

// ---- bundles ----

/// macOS archive metadata, dropped from bundles without comment.
fn is_junk(name: &str) -> bool {
    name.starts_with("__MACOSX/") || name.rsplit('/').next() == Some(".DS_Store")
}

/// The files of a zip bundle, each checked like a single write; any problem rejects the bundle, all of them
/// listed. Directory entries and macOS metadata are skipped; when the site sits in one top folder (it holds
/// `index.html` and nothing lies outside it), that folder is stripped.
pub fn read_bundle(bytes: &[u8]) -> Result<Vec<(String, String)>, String> {
    let bad_zip = |e: zip::result::ZipError| format!("压缩包读不出来：{e}。请上传 zip 格式的压缩包。");
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(bad_zip)?;
    // Headers first: names and declared sizes, nothing decompressed yet.
    let mut entries = vec![];
    let mut declared = 0u64;
    for i in 0..zip.len() {
        let f = zip.by_index_raw(i).map_err(bad_zip)?;
        let name = f.name().map_err(bad_zip)?.into_owned();
        if f.is_dir() || is_junk(&name) {
            continue;
        }
        declared += f.size();
        entries.push((i, name, f.size()));
    }
    if entries.len() > MAX_FILES {
        return Err(format!("压缩包里有 {} 个文件，站点最多 {MAX_FILES} 个文件。", entries.len()));
    }
    if declared > MAX_BUNDLE_BYTES as u64 {
        return Err(format!("压缩包解压后有 {} MB，一次最多 50 MB。", declared >> 20));
    }
    let top = entries.first().and_then(|(_, n, _)| n.split_once('/')).map(|(t, _)| format!("{t}/"));
    let strip = top.filter(|t| entries.iter().all(|(_, n, _)| n.starts_with(t.as_str())) && entries.iter().any(|(_, n, _)| *n == format!("{t}index.html")));

    let mut problems = vec![];
    let mut files: Vec<(String, String)> = vec![];
    let mut total = 0usize;
    for (i, name, size) in entries {
        let path = strip.as_deref().and_then(|t| name.strip_prefix(t)).unwrap_or(&name).to_string();
        if let Err(e) = check_path(&path) {
            problems.push(e);
            continue;
        }
        if size > MAX_FILE_BYTES as u64 {
            problems.push(format!("“{path}”太大：每个文件最多 1 MB。"));
            continue;
        }
        // Declared sizes can lie: read at most one byte past the limit.
        let mut data = vec![];
        let read = zip.by_index(i).and_then(|f| Ok(f.take(MAX_FILE_BYTES as u64 + 1).read_to_end(&mut data)?));
        if let Err(e) = read {
            problems.push(format!("“{path}”读不出来：{e}。"));
            continue;
        }
        total += data.len();
        if total > MAX_BUNDLE_BYTES {
            return Err("压缩包解压后超过 50 MB，一次最多 50 MB。".into());
        }
        if data.len() > MAX_FILE_BYTES {
            problems.push(format!("“{path}”太大：每个文件最多 1 MB。"));
            continue;
        }
        let Ok(content) = String::from_utf8(data) else {
            problems.push(format!("“{path}”不是 UTF-8 文本：图片、字体等二进制文件请用 upload_media 上传。"));
            continue;
        };
        if files.iter().any(|(p, _)| *p == path) {
            problems.push(format!("“{path}”在压缩包里出现了不止一次。"));
            continue;
        }
        match check_file(&path, &content) {
            Ok(()) => files.push((path, content)),
            Err(e) => problems.push(e),
        }
    }
    if problems.is_empty() {
        return Ok(files);
    }
    let n = problems.len();
    let more = if n > 20 { format!("（另有 {} 个问题未列出）", n - 20) } else { String::new() };
    problems.truncate(20);
    Err(format!("压缩包里有 {n} 个问题，什么都没有写入：{}{more}", problems.join("；")))
}
