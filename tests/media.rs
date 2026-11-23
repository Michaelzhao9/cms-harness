mod common;
use axum::http::{header, Request, StatusCode};
use base64::Engine;
use common::*;
use harness_cms::auth::Scope;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbImage::new(width, height).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}
fn b64(bytes: &[u8]) -> String { base64::engine::general_purpose::STANDARD.encode(bytes) }

#[test]
fn stores_by_content_and_ignores_the_claimed_name() {
    let app = app();
    let bytes = png(4, 4);
    let a = ok(&app, &editor(), "upload_media", json!({ "filename": "photo.jpg", "data_base64": b64(&bytes) }))["path"].as_str().unwrap().to_string();
    assert!(a.starts_with("/uploads/") && a.ends_with(".png"), "{a}"); // real format wins over the name
    let again = ok(&app, &editor(), "upload_media", json!({ "filename": "../../x.png", "data_base64": b64(&bytes) }));
    assert_eq!(again["path"], a.as_str());
    let (served, content_type) = harness_cms::media::read(&app, a.trim_start_matches("/uploads/")).unwrap();
    assert_eq!((served, content_type), (bytes, "image/png"));
    assert!(harness_cms::media::read(&app, "../harness.db").is_none());
    for bad in ["", "A.png", "x/y.png", "..", &a["/uploads/".len()..].to_uppercase()] {
        assert!(harness_cms::media::read(&app, bad).is_none(), "{bad}");
    }
}

#[test]
fn rejects_what_it_should() {
    let app = app();
    let e = editor();
    let html = b"<html><script>alert(1)</script></html>";
    assert!(err(&app, &e, "upload_media", json!({ "filename": "a.png", "data_base64": b64(html) })).contains("格式"));
    assert!(err(&app, &e, "upload_media", json!({ "filename": "a.svg", "data_base64": b64(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>") })).contains("格式"));
    assert!(err(&app, &e, "upload_media", json!({ "filename": "a.png", "data_base64": "***" })).contains("base64"));
    assert!(err(&app, &e, "upload_media", json!({ "filename": "a.png", "data_base64": "" })).contains("空"));
    assert!(err(&app, &reader(), "upload_media", json!({ "filename": "a.png", "data_base64": b64(&png(1, 1)) })).contains("write"));
    assert!(harness_cms::media::check_pixels(8000, 6000).unwrap_err().contains("像素")); // 48 million
    assert!(harness_cms::media::check_pixels(6000, 6000).is_ok());                        // 36 million
    let big = [b"%PDF-".as_slice(), &vec![0u8; 5 * 1024 * 1024]].concat();
    assert!(err(&app, &e, "upload_media", json!({ "filename": "a.pdf", "data_base64": b64(&big) })).contains("5 MB"));
    // a truncated PNG header names the file
    assert!(err(&app, &e, "upload_media", json!({ "filename": "broken.png", "data_base64": b64(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) })).contains("broken.png"));
}

#[tokio::test]
async fn files_are_served_with_safe_headers() {
    let app = app();
    let path = ok(&app, &editor(), "upload_media", json!({ "filename": "a.png", "data_base64": b64(&png(2, 2)) }))["path"].as_str().unwrap().to_string();
    let (status, headers, _) = get(&app, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert!(headers["cache-control"].to_str().unwrap().contains("immutable"));
    assert!(headers.get("content-disposition").is_none());
    assert_eq!(get(&app, "/uploads/missing.png", None).await.0, StatusCode::NOT_FOUND);
    let pdf = ok(&app, &editor(), "upload_media", json!({ "filename": "a.pdf", "data_base64": b64(b"%PDF-1.4 hi") }))["path"].as_str().unwrap().to_string();
    let (_, headers, _) = get(&app, &pdf, None).await;
    assert_eq!(headers["content-type"], "application/pdf");
    assert_eq!(headers["content-disposition"], "attachment");
}

fn multipart(parts: &[(&str, &[u8])]) -> (String, Vec<u8>) {
    let boundary = "XBOUNDARYX";
    let mut body = vec![];
    for (name, bytes) in parts {
        body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"a.png\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
        body.extend(*bytes);
        body.extend(b"\r\n");
    }
    body.extend(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

async fn upload(app: &harness_cms::Shared, key: Option<&str>, parts: &[(&str, &[u8])]) -> (StatusCode, Value) {
    let (ct, body) = multipart(parts);
    let mut req = Request::post("/api/v1/media").header(header::CONTENT_TYPE, ct);
    if let Some(k) = key {
        req = req.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    let resp = harness_cms::api::router(app.clone()).oneshot(req.body(axum::body::Body::from(body)).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn multipart_route() {
    let app = app();
    let w = key(&app, &[Scope::Write]);
    let bytes = png(3, 3);
    let (s, v) = upload(&app, Some(&w), &[("file", &bytes)]).await;
    assert_eq!((s, v["ok"].clone()), (StatusCode::OK, json!(true)), "{v}");
    let path = v["result"]["path"].as_str().unwrap().to_string();
    assert_eq!(get(&app, &path, None).await.0, StatusCode::OK);
    assert_eq!(upload(&app, None, &[("file", &bytes)]).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(upload(&app, Some(&key(&app, &[Scope::Read])), &[("file", &bytes)]).await.0, StatusCode::FORBIDDEN);
    assert_eq!(upload(&app, Some(&w), &[("other", &bytes)]).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(upload(&app, Some(&w), &[]).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(upload(&app, Some(&w), &[("file", &bytes), ("file", &bytes)]).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(upload(&app, Some(&w), &[("file", b"<html>")]).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn oversize_body_is_413() {
    let app = app();
    let w = key(&app, &[Scope::Write]);
    let big = vec![0u8; 22 * 1024 * 1024];
    assert_eq!(upload(&app, Some(&w), &[("file", &big)]).await.0, StatusCode::PAYLOAD_TOO_LARGE);
}
