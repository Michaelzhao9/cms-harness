mod common;
use axum::http::StatusCode;
use cms_harness::schedule;
use common::*;
use serde_json::json;

fn sql_time(app: &cms_harness::Shared, modifier: &str) -> String {
    app.conn().query_row("SELECT datetime('now', ?1)", [modifier], |r| r.get(0)).unwrap()
}

fn draft(app: &cms_harness::Shared) -> i64 {
    news(app);
    publish_all(app);
    ok(app, &builder(), "create_record", json!({ "collection": "news", "note": "写", "data": { "title": "到点上线" } }))["id"].as_i64().unwrap()
}

fn status(app: &cms_harness::Shared, id: i64) -> (String, Option<String>, Option<i64>) {
    app.conn().query_row("SELECT status, result, release_id FROM schedules WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap()
}

#[test]
fn times_are_read_in_the_given_zone_and_kept_in_utc() {
    let app = app();
    let local = sql_time(&app, "+1 day");
    let utc = schedule::normalize(&app.conn(), &local.replace(' ', "T")[..16], 480).unwrap();
    let want: String = app.conn().query_row("SELECT datetime(?1, '-480 minutes')", [&local[..16]], |r| r.get(0)).unwrap();
    assert_eq!(utc, want);
    let (past, far) = (sql_time(&app, "-1 hour"), sql_time(&app, "+400 days"));
    assert!(schedule::normalize(&app.conn(), &past, 0).unwrap_err().contains("将来"));
    assert!(schedule::normalize(&app.conn(), &far, 0).unwrap_err().contains("一年"));
    assert!(schedule::normalize(&app.conn(), "下周一", 0).unwrap_err().contains("看不懂"));
}

#[test]
fn a_due_schedule_publishes_as_the_person_who_set_it() {
    let app = app();
    let id = draft(&app);
    cms_harness::auth::create_admin(&app.conn(), "ed@test", "editor", Some("secret-pass")).unwrap();
    let ed = cms_harness::auth::Actor { name: "ed@test".into(), scopes: cms_harness::auth::role_scopes("editor"), admin: true };
    let hour = sql_time(&app, "+1 hour");
    let later = schedule::create(&app.conn(), &ed, &hour, Some(vec![format!("record:{id}")]), "周一上线").unwrap();
    assert_eq!(schedule::run_due(&app.conn()).unwrap(), 0, "not yet");
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": id }))["status"], "draft");

    app.conn().execute("UPDATE schedules SET at = datetime('now', '-1 second') WHERE id = ?1", [later]).unwrap();
    assert_eq!(schedule::run_due(&app.conn()).unwrap(), 1);
    let (st, result, release) = status(&app, later);
    assert_eq!((st.as_str(), result), ("done", None));
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": id }))["status"], "published");
    let (actor, note): (String, String) = app.conn().query_row("SELECT actor, note FROM releases WHERE id = ?1", [release.unwrap()], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((actor.as_str(), note.as_str()), ("ed@test", "周一上线"));
    assert_eq!(schedule::run_due(&app.conn()).unwrap(), 0, "runs once");
}

#[test]
fn failures_and_cancellations_publish_nothing() {
    let app = app();
    let id = draft(&app);
    let hour = sql_time(&app, "+1 hour");
    assert!(schedule::create(&app.conn(), &reader(), &hour, None, "x").unwrap_err().contains("没有发布权限"));
    cms_harness::auth::create_admin(&app.conn(), "gone@test", "builder", Some("secret-pass")).unwrap();
    let gone = cms_harness::auth::Actor { name: "gone@test".into(), scopes: cms_harness::auth::role_scopes("builder"), admin: true };
    let a = schedule::create(&app.conn(), &gone, &hour, None, "x").unwrap();
    let b = schedule::create(&app.conn(), &builder(), &hour, None, "x").unwrap();
    schedule::cancel(&app.conn(), b).unwrap();
    assert!(schedule::cancel(&app.conn(), b).is_err(), "only once");
    app.conn().execute("DELETE FROM admins WHERE email = 'gone@test'", []).unwrap();
    app.conn().execute("UPDATE schedules SET at = datetime('now', '-1 second')", []).unwrap();
    assert_eq!(schedule::run_due(&app.conn()).unwrap(), 1);
    let (st, result, _) = status(&app, a);
    assert_eq!(st, "failed");
    assert!(result.unwrap().contains("gone@test"));
    assert_eq!(status(&app, b).0, "cancelled");
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": id }))["status"], "draft");
}

#[tokio::test]
async fn the_publish_page_schedules_lists_and_cancels() {
    let app = app();
    let id = draft(&app);
    let s = admin_session(&app, "builder").await;
    let at = sql_time(&app, "+2 hours").replace(' ', "T")[..16].to_string();
    let target = format!("record:{id}");
    let (code, h, body) = post_form(&app, "/admin/publish/schedule", Some(&s), &[("target", &target), ("at", &at), ("tz", "0"), ("note", "晚点上线")]).await;
    assert_eq!(code, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(h["location"], "/admin/publish?scheduled=1");
    let (_, _, page) = get_with(&app, "/admin/publish", Some(&s.0)).await;
    assert!(page.contains("data-utc") && page.contains("晚点上线") && page.contains("等待中"), "{page}");
    let (code, _, body) = post_form(&app, "/admin/publish/schedule", Some(&s), &[("target", &target), ("at", "2000-01-01T00:00"), ("tz", "0")]).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert!(body.contains("要在将来"), "{body}");
    let sid: i64 = app.conn().query_row("SELECT id FROM schedules", [], |r| r.get(0)).unwrap();
    let (code, _, _) = post_form(&app, &format!("/admin/publish/schedule/{sid}/cancel"), Some(&s), &[]).await;
    assert_eq!(code, StatusCode::SEE_OTHER);
    assert_eq!(status(&app, sid).0, "cancelled");
    // A second click (or the ticker got there first) is an error on the page, not a stuck server.
    let (code, _, body) = post_form(&app, &format!("/admin/publish/schedule/{sid}/cancel"), Some(&s), &[]).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert!(body.contains("没有等待中的定时发布"), "{body}");
    // The chat never gets scheduling: it is publishing.
    let names: Vec<String> = cms_harness::chat::chat_tools(&builder()).into_iter().map(|t| t.name).collect();
    assert!(!names.iter().any(|n| n == "schedule_publish" || n == "cancel_schedule"), "{names:?}");
    assert!(names.iter().any(|n| n == "list_schedules"));
    // Agents with a publish key can.
    let r = ok(&app, &builder(), "schedule_publish", json!({ "at": at, "note": "agent", "tz_offset_minutes": 0 }));
    assert!(r["id"].as_i64().is_some());
}
