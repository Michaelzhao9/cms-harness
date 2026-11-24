//! The live site as static files, for any static host or CDN: every public page (page files and record pages),
//! every other published site file (scripts transpiled, as the site serves them), the uploads, `sitemap.xml`,
//! `robots.txt` and a `404.html`. Pages are fetched through the server's own router, so what is written is exactly
//! what a visitor gets. Forms, sign-in and the API still need the server; pages that use them are listed.

use crate::Shared;
use axum::http::{Request, StatusCode};
use std::path::{Path, PathBuf};
use tower::ServiceExt;

pub struct Summary {
    pub pages: usize,
    pub files: usize,
    pub uploads: usize,
    /// Pages with a form or sign-in link: on a static host they need the server for that part.
    pub need_server: Vec<String>,
}

/// Where a page's file goes: `/` and `/dir/` as `index.html`, `/about` as `about.html` (static hosts serve both
/// without the extension).
fn page_file(dir: &Path, path: &str) -> PathBuf {
    let rel = path.trim_start_matches('/');
    if rel.is_empty() || rel.ends_with('/') {
        dir.join(rel).join("index.html")
    } else {
        dir.join(format!("{rel}.html"))
    }
}

async fn get(app: &Shared, path: &str) -> Result<(StatusCode, Vec<u8>), String> {
    let uri = crate::routes::encode(path);
    let req = Request::get(uri.as_str()).body(axum::body::Body::empty()).map_err(|e| e.to_string())?;
    // The router never fails: its error type is Infallible.
    let resp = match crate::api::router(app.clone()).oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 64 << 20).await.map_err(|e| format!("读取 {path} 失败：{e}"))?;
    Ok((status, body.to_vec()))
}

fn write(to: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录 {} 失败：{e}", parent.display()))?;
    }
    std::fs::write(to, bytes).map_err(|e| format!("写入 {} 失败：{e}", to.display()))
}

/// Writes the live site into `dir`.
pub async fn export(app: &Shared, dir: &Path) -> Result<Summary, String> {
    let (pages, files) = {
        let conn = app.conn();
        let pages = crate::site::public_paths(&conn)?;
        let files: Vec<String> = conn
            .prepare("SELECT path FROM site_files WHERE published IS NOT NULL AND path NOT LIKE '%.html' ORDER BY path")
            .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
            .map_err(|e| format!("读取站点文件失败：{e}"))?;
        (pages, files)
    };
    let mut summary = Summary { pages: 0, files: 0, uploads: 0, need_server: vec![] };
    for (path, _) in &pages {
        let (status, body) = get(app, path).await?;
        if status != StatusCode::OK {
            continue;
        }
        let text = String::from_utf8_lossy(&body);
        if text.contains("<form") || text.contains("/account/") {
            summary.need_server.push(path.clone());
        }
        write(&page_file(dir, path), &body)?;
        summary.pages += 1;
    }
    for path in files.iter().map(|p| format!("/{p}")).chain(["/sitemap.xml".to_string(), "/robots.txt".to_string()]) {
        let (status, body) = get(app, &path).await?;
        if status == StatusCode::OK {
            write(&dir.join(path.trim_start_matches('/')), &body)?;
            summary.files += 1;
        }
    }
    // The site's own 404 page, for hosts that serve 404.html.
    let (_, body) = get(app, "/__export_missing__").await?;
    write(&dir.join("404.html"), &body)?;
    if let Ok(entries) = std::fs::read_dir(app.data_dir.join("uploads")) {
        for e in entries.flatten() {
            if e.path().is_file() {
                std::fs::copy(e.path(), dir.join("uploads").join(e.file_name()))
                    .or_else(|_| {
                        std::fs::create_dir_all(dir.join("uploads"))?;
                        std::fs::copy(e.path(), dir.join("uploads").join(e.file_name()))
                    })
                    .map_err(|err| format!("复制上传文件失败：{err}"))?;
                summary.uploads += 1;
            }
        }
    }
    Ok(summary)
}
