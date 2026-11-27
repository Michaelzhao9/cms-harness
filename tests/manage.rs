//! Managing any data from the harness itself: find a record by anything written in it, take a collection out
//! as CSV, and bring rows in as drafts.

mod common;

use axum::http::{header, HeaderMap, Request, StatusCode};
use common::*;
use cms_harness::Shared;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// A `products` collection with three records: two published, one still a draft.
fn shop(app: &Shared) {
    ok(app, &builder(), "create_collection", json!({"name": "products", "title": "商品", "note": "x", "fields": [
        {"name": "name", "title": "名称", "type": "text", "required": true},
        {"name": "price", "title": "价格", "type": "number"},
        {"name": "on_sale", "title": "在售", "type": "boolean"},
        {"name": "tags", "title": "标签", "type": "select", "options": ["茶具", "餐具"], "multiple": true},
        {"name": "note", "title": "备注", "type": "text"}]}));
    for (name, price, note) in [("青釉茶杯", 88, "手工, 每只略有不同"), ("白瓷碗", 45, "说“耐用”的那一款")] {
        ok(app, &builder(), "create_record", json!({"collection": "products", "data": {"name": name, "price": price, "on_sale": true, "tags": ["茶具"], "note": note}, "note": "x"}));
    }
    publish_all(app);
    ok(app, &builder(), "create_record", json!({"collection": "products", "data": {"name": "还没上架的壶", "price": 300, "note": "100%_手作"}, "note": "x"}));
}

fn names(v: &Value) -> Vec<&str> {
    v["items"].as_array().unwrap().iter().map(|i| i["data"]["name"].as_str().unwrap()).collect()
}

#[test]
fn search_finds_a_record_by_anything_written_in_it_drafts_too() {
    let app = app();
    shop(&app);
    let find = |q: &str| ok(&app, &reader(), "query_records", json!({"collection": "products", "search": q}));
    assert_eq!(names(&find("茶杯")), vec!["青釉茶杯"]);
    assert_eq!(names(&find("耐用")), vec!["白瓷碗"], "a field that is not indexed");
    assert_eq!(names(&find("还没上架")), vec!["还没上架的壶"], "a draft");
    assert_eq!(names(&find("100%_")), vec!["还没上架的壶"], "% and _ are text, not wildcards");
    assert_eq!(find("没有这个词")["total"], 0);
    assert_eq!(find("")["total"], 3, "an empty search is no search");
}

#[tokio::test]
async fn the_admin_list_has_a_search_box_that_survives_paging_links() {
    let app = app();
    shop(&app);
    let (cookie, _) = admin_session(&app, "editor").await;
    let (s, _, page) = get_with(&app, "/admin/c/products?q=%E8%8C%B6%E6%9D%AF", Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(page.contains("青釉茶杯") && !page.contains("白瓷碗"), "{page}");
    assert!(page.contains("name=\"q\"") && page.contains("value=\"茶杯\""), "the box keeps what was typed");
    assert!(page.contains("status=draft&q=%E8%8C%B6%E6%9D%AF") || page.contains("status=draft&amp;q=%E8%8C%B6%E6%9D%AF"), "tabs keep the search: {page}");
}

async fn download(app: &Shared, path: &str, cookie: Option<&str>) -> (StatusCode, HeaderMap, String) {
    get_with(app, path, cookie).await
}

#[tokio::test]
async fn a_collection_comes_out_as_csv_a_spreadsheet_opens_safely() {
    let app = app();
    shop(&app);
    ok(&app, &builder(), "create_record", json!({"collection": "products", "data": {"name": "=HYPERLINK(\"http://evil\")", "note": "第一行\n第二行"}, "note": "x"}));
    let (cookie, _) = admin_session(&app, "editor").await;
    let (s, h, csv) = download(&app, "/admin/c/products/export.csv", Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/csv"));
    assert!(h[header::CONTENT_DISPOSITION].to_str().unwrap().contains("products"));
    let csv = csv.trim_start_matches('\u{feff}');
    let lines: Vec<&str> = csv.split("\r\n").collect();
    assert_eq!(lines[0], "id,status,name,price,on_sale,tags,note");
    assert!(csv.contains("1,published,青釉茶杯,88,true,茶具,\"手工, 每只略有不同\""), "a comma is quoted: {csv}");
    assert!(csv.contains("\"说“耐用”的那一款\"") || csv.contains("说“耐用”的那一款"), "{csv}");
    assert!(csv.contains(",draft,还没上架的壶,300,"), "drafts come too, marked: {csv}");
    assert!(csv.contains("\"'=HYPERLINK(\"\"http://evil\"\")\""), "a cell a spreadsheet would run is defused: {csv}");
    assert!(csv.contains("\"第一行\n第二行\""), "a line break stays inside its cell: {csv}");
    assert_ne!(download(&app, "/admin/c/products/export.csv", None).await.0, StatusCode::OK, "not without a session");
}

/// A multipart POST with the session cookie and CSRF token; `file` is the CSV.
async fn import(app: &Shared, session: &(String, String), csv: &str) -> (StatusCode, HeaderMap, String) {
    let boundary = "XBOUNDARYX";
    let mut body = vec![];
    for (disposition, bytes) in [("form-data; name=\"csrf\"".to_string(), session.1.as_bytes()), ("form-data; name=\"file\"; filename=\"rows.csv\"\r\nContent-Type: text/csv".to_string(), csv.as_bytes())] {
        body.extend(format!("--{boundary}\r\nContent-Disposition: {disposition}\r\n\r\n").as_bytes());
        body.extend(bytes);
        body.extend(b"\r\n");
    }
    body.extend(format!("--{boundary}--\r\n").as_bytes());
    let req = Request::post("/admin/c/products/import").header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}")).header(header::COOKIE, session.0.as_str());
    let resp = cms_harness::api::router(app.clone()).oneshot(req.body(axum::body::Body::from(body)).unwrap()).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn rows_come_in_from_csv_as_drafts() {
    let app = app();
    shop(&app);
    let session = admin_session(&app, "editor").await;
    // Headers may be field names or their titles; id and status columns of an export are ignored.
    let csv = "\u{feff}id,status,名称,price,on_sale,tags,note\r\n,,竹茶盘,120,true,茶具|餐具,\"宽 30cm, 可定制\"\r\n99,published,粗陶罐,60,否,,\r\n";
    let (s, h, body) = import(&app, &session, csv).await;
    assert_eq!(s, StatusCode::SEE_OTHER, "{body}");
    assert!(h[header::LOCATION].to_str().unwrap().contains("imported=2"));
    let found = ok(&app, &reader(), "query_records", json!({"collection": "products", "search": "竹茶盘"}));
    let rec = &found["items"][0];
    assert_eq!(rec["status"], "draft", "nothing is live until someone publishes");
    assert_eq!((rec["data"]["price"].as_i64(), rec["data"]["on_sale"].as_bool(), rec["data"]["note"].as_str()), (Some(120), Some(true), Some("宽 30cm, 可定制")));
    assert_eq!(rec["data"]["tags"], json!(["茶具", "餐具"]));
    let jar = ok(&app, &reader(), "query_records", json!({"collection": "products", "search": "粗陶罐"}));
    assert_eq!((jar["items"][0]["data"]["on_sale"].as_bool(), jar["items"][0]["id"].as_i64() == Some(99)), (Some(false), false), "a new record, not record 99");
}

#[tokio::test]
async fn one_bad_row_and_nothing_comes_in() {
    let app = app();
    shop(&app);
    let session = admin_session(&app, "editor").await;
    let before = ok(&app, &reader(), "query_records", json!({"collection": "products"}))["total"].clone();
    let (s, _, body) = import(&app, &session, "name,price\r\n好的一行,10\r\n,20\r\n第三行,很贵\r\n").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(body.contains("第 3 行") && body.contains("第 4 行"), "each bad row is named by its line in the file: {body}");
    assert_eq!(ok(&app, &reader(), "query_records", json!({"collection": "products"}))["total"], before);

    let (s, _, body) = import(&app, &session, "name,颜色\r\n红杯,红\r\n").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(body.contains("颜色"), "a column that is no field: {body}");
    assert_eq!(import(&app, &session, "").await.0, StatusCode::BAD_REQUEST);
}
