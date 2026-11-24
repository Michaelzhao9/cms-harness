mod common;
use axum::http::{header, StatusCode};
use cms_harness::{backup, store, App, Shared};
use common::*;
use serde_json::json;
use std::io::{Cursor, Read, Write};

/// An app on a database file, as a real server has, with one published record and one upload.
fn file_app() -> (Shared, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("hc-backup-{:016x}", rand::random::<u64>()));
    std::fs::create_dir_all(dir.join("uploads")).unwrap();
    let db = dir.join("harness.db");
    let app = App::new(store::open(db.to_str().unwrap()).unwrap(), dir.clone(), "http://127.0.0.1:4200".into(), "测试站".into());
    cms_harness::auth::create_admin(&app.conn(), "owner@test", "builder", Some("secret-pass")).unwrap();
    ok(&app, &builder(), "create_collection", json!({ "name": "notes", "title": "笔记", "read": "public", "note": "建",
        "fields": [{ "name": "title", "title": "标题", "type": "text" }] }));
    ok(&app, &builder(), "create_record", json!({ "collection": "notes", "note": "写", "data": { "title": "备份之前的笔记" } }));
    publish_all(&app);
    std::fs::write(dir.join("uploads").join("ab12cd.png"), b"png bytes").unwrap();
    (app, dir)
}

fn archive(app: &Shared) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    backup::export(&app.conn(), &app.data_dir, &mut out).unwrap();
    out.into_inner()
}

fn titles(app: &Shared) -> Vec<String> {
    let v = ok(app, &reader(), "query_records", json!({ "collection": "notes" }));
    v["items"].as_array().unwrap().iter().map(|r| r["data"]["title"].as_str().unwrap_or_default().to_string()).collect()
}

#[test]
fn a_backup_holds_the_database_and_the_uploads() {
    let (app, _) = file_app();
    let bytes = archive(&app);
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut names: Vec<String> = zip.file_names().map(|n| n.unwrap().to_string()).collect();
    names.sort();
    assert_eq!(names, ["backup.json", "harness.db", "uploads/ab12cd.png"]);
    let mut manifest = String::new();
    zip.by_name("backup.json").unwrap().read_to_string(&mut manifest).unwrap();
    assert!(manifest.contains("\"app\": \"cms-harness\""), "{manifest}");
}

#[test]
fn a_restore_brings_back_the_site_and_keeps_what_it_replaced() {
    let (app, dir) = file_app();
    let bytes = archive(&app);
    ok(&app, &builder(), "create_record", json!({ "collection": "notes", "note": "写", "data": { "title": "备份之后的笔记" } }));
    publish_all(&app);
    std::fs::remove_file(dir.join("uploads").join("ab12cd.png")).unwrap();
    assert_eq!(titles(&app).len(), 2);

    let saved = backup::restore_live(&app, &bytes).unwrap();
    assert_eq!(titles(&app), ["备份之前的笔记"]);
    assert!(dir.join("uploads").join("ab12cd.png").exists(), "uploads come back");
    assert!(saved.starts_with(dir.join("backups")) && saved.exists());
    // The server keeps working on the restored database: writes, publishing, search.
    ok(&app, &builder(), "create_record", json!({ "collection": "notes", "note": "写", "data": { "title": "恢复之后的笔记" } }));
    publish_all(&app);
    assert_eq!(cms_harness::search::search(&app.conn(), "恢复之后", None, cms_harness::access::Viewer::Anon, 5).unwrap().len(), 1);
    // The safety copy is itself a backup that restores.
    backup::restore_live(&app, &std::fs::read(&saved).unwrap()).unwrap();
    assert_eq!(titles(&app).len(), 2);
}

#[test]
fn bad_archives_change_nothing() {
    let (app, dir) = file_app();
    let zip_of = |files: &[(&str, &[u8])]| {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, body) in files {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(body).unwrap();
        }
        w.finish().unwrap().into_inner()
    };
    let good_db = std::fs::read(dir.join("harness.db")).unwrap_or_default();
    for (bytes, needle) in [
        (b"not a zip".to_vec(), "读不出来"),
        (zip_of(&[("backup.json", b"{}")]), "没有 harness.db"),
        (zip_of(&[("harness.db", &good_db), ("../evil.sh", b"x")]), "不认识的文件"),
        (zip_of(&[("harness.db", &good_db), ("uploads/../x.png", b"x")]), "不认识的文件"),
        (zip_of(&[("harness.db", b"garbage, not sqlite")]), "打不开"),
    ] {
        let e = backup::restore_live(&app, &bytes).unwrap_err();
        assert!(e.contains(needle), "{needle}: {e}");
    }
    // A backup without a builder would lock everyone out.
    let empty = std::env::temp_dir().join(format!("hc-nobody-{:016x}", rand::random::<u64>()));
    std::fs::create_dir_all(&empty).unwrap();
    let nobody = App::new(store::open(empty.join("harness.db").to_str().unwrap()).unwrap(), empty, "http://127.0.0.1:4200".into(), "x".into());
    let e = backup::restore_live(&app, &archive(&nobody)).unwrap_err();
    assert!(e.contains("没有搭建者"), "{e}");
    assert_eq!(titles(&app), ["备份之前的笔记"], "the site is untouched");
    assert!(!dir.join("backups").exists(), "no safety copy for a refused restore");
    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()?.file_name().into_string().ok()).filter(|n| n.contains("restore")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[tokio::test]
async fn only_builders_download_and_restore_from_settings() {
    let (app, _) = file_app();
    let s = admin_session(&app, "builder").await;
    let (status, h, _) = post_form(&app, "/admin/settings/backup", Some(&s), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "application/zip");
    assert!(h[header::CONTENT_DISPOSITION].to_str().unwrap().starts_with("attachment; filename=\"cms-harness-backup-"));
    let (status, _, page) = post_form(&app, "/admin/settings/restore", Some(&s), &[("confirm", "yes")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("请先选择一个备份文件"), "{page}");
    let e = admin_session(&app, "editor").await;
    assert_eq!(post_form(&app, "/admin/settings/backup", Some(&e), &[]).await.0, StatusCode::FORBIDDEN);
    assert_eq!(post_form(&app, "/admin/settings/restore", Some(&e), &[]).await.0, StatusCode::FORBIDDEN);
    let (_, _, settings) = get_with(&app, "/admin/settings", Some(&e.0)).await;
    assert!(settings.contains("只有搭建者可以备份和恢复") && !settings.contains("/admin/settings/restore"));
}
