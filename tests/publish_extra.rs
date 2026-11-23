mod common;
use common::*;
use serde_json::json;

fn one(app: &harness_cms::Shared, title: &str, slug: &str) -> i64 {
    ok(app, &editor(), "create_record", json!({ "collection": "news", "note": "新建", "data": { "title": title, "slug": slug } }))["id"]
        .as_i64()
        .unwrap()
}

fn count(app: &harness_cms::Shared, table: &str) -> i64 {
    app.conn().query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap()
}

#[test]
fn a_blocked_publish_changes_nothing() {
    let app = app();
    news(&app);
    let a = one(&app, "A", "a");
    publish_all(&app);
    let (releases, versions) = (count(&app, "releases"), count(&app, "versions"));
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "改", "data": { "title": "A2" } }));
    // A second writer slips a duplicate slug past the draft-time check.
    app.conn().execute("INSERT INTO records(collection, draft) VALUES('news', '{\"title\":\"B\",\"slug\":\"a\"}')", []).unwrap();
    let pending_before = ok(&app, &reader(), "list_pending", json!({}));

    let e = err(&app, &builder(), "publish", json!({ "note": "一起发" }));
    assert!(e.contains("什么都没有上线"), "{e}");
    assert_eq!(ok(&app, &reader(), "list_pending", json!({})), pending_before);
    assert_eq!((count(&app, "releases"), count(&app, "versions")), (releases, versions));
    let live: String = app.conn().query_row("SELECT published FROM records WHERE id = ?1", [a], |r| r.get(0)).unwrap();
    assert!(live.contains("\"A\""), "{live}"); // the valid change did not go live on its own
}

#[test]
fn pending_names_what_changed_and_who_did_it() {
    let app = app();
    news(&app);
    let a = one(&app, "A", "a");
    publish_all(&app);
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "换标题和日期", "data": { "title": "A2", "date": "2026-12-06" } }));
    let b = one(&app, "B", "b");
    let pending = ok(&app, &reader(), "list_pending", json!({}));
    assert_eq!(pending.as_array().unwrap().len(), 2);
    assert_eq!(pending[0], json!({ "target": format!("record:{a}"), "kind": "changed", "title": "A2",
        "fields": ["date", "title"], "actor": "editor@test", "note": "换标题和日期", "affected": 1 }));
    assert_eq!((pending[1]["target"].as_str().unwrap(), pending[1]["kind"].as_str()), (format!("record:{b}").as_str(), Some("created")));
    assert_eq!(pending[1]["fields"], json!([]));

    ok(&app, &editor(), "delete_record", json!({ "id": a, "note": "不要了" }));
    assert_eq!(ok(&app, &reader(), "list_pending", json!({}))[0]["kind"], "deleted");
}

#[test]
fn history_records_the_drafter_and_their_note() {
    let app = app();
    news(&app);
    let a = one(&app, "A", "a");
    let release = ok(&app, &builder(), "publish", json!({ "note": "首发" }));
    let v = ok(&app, &reader(), "list_versions", json!({ "target": format!("record:{a}") }));
    assert_eq!((v[0]["actor"].as_str(), v[0]["note"].as_str()), (Some("editor@test"), Some("新建")));
    assert_eq!(v[0]["release_id"], release["release_id"]);
    assert_eq!(v[0]["deleted"], false);
    let (actor, note): (String, String) =
        app.conn().query_row("SELECT actor, note FROM releases WHERE id = ?1", [release["release_id"].as_i64()], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((actor.as_str(), note.as_str()), ("builder@test", "首发"));
}

#[test]
fn targets_are_checked_before_anything_happens() {
    let app = app();
    news(&app);
    let a = one(&app, "A", "a");
    let b = builder();
    assert!(err(&app, &b, "publish", json!({ "note": "n", "targets": ["nonsense"] })).contains("nonsense"));
    assert!(err(&app, &b, "publish", json!({ "note": "n", "targets": ["record:999"] })).contains("record:999"));
    assert!(err(&app, &b, "publish", json!({ "note": "", "targets": ["collection:news"] })).contains("note"));
    publish_all(&app);
    // Nothing is pending for this record any more.
    assert!(err(&app, &b, "publish", json!({ "note": "n", "targets": [format!("record:{a}")] })).contains("没有待发布"));
}

#[test]
fn restore_and_discard_respect_scope_and_bad_input() {
    let app = app();
    news(&app);
    let a = one(&app, "A", "a");
    publish_all(&app);
    let target = format!("record:{a}");
    let first = ok(&app, &reader(), "list_versions", json!({ "target": target }))[0]["id"].clone();

    assert!(err(&app, &editor(), "restore_version", json!({ "target": target, "version_id": 9999, "note": "n" })).contains("9999"));
    assert!(err(&app, &reader(), "restore_version", json!({ "target": target, "version_id": first, "note": "n" })).contains("write"));
    // The editor may restore a record but not a collection definition.
    let coll = ok(&app, &reader(), "list_versions", json!({ "target": "collection:news" }))[0]["id"].clone();
    assert!(err(&app, &editor(), "restore_version", json!({ "target": "collection:news", "version_id": coll, "note": "n" })).contains("structure"));
    assert!(err(&app, &editor(), "discard_draft", json!({ "target": "collection:news" })).contains("structure"));

    ok(&app, &editor(), "delete_record", json!({ "id": a, "note": "删" }));
    publish_all(&app);
    let deletion = ok(&app, &reader(), "list_versions", json!({ "target": target }))[0]["id"].clone();
    assert!(err(&app, &editor(), "restore_version", json!({ "target": target, "version_id": deletion, "note": "n" })).contains("删除"));
}

#[test]
fn deleting_a_record_keeps_its_row_and_id() {
    let app = app();
    news(&app);
    let a = one(&app, "A", "a");
    publish_all(&app);
    ok(&app, &editor(), "delete_record", json!({ "id": a, "note": "删" }));
    assert_eq!(ok(&app, &editor(), "discard_draft", json!({ "target": format!("record:{a}") }))["status"], "published");
    ok(&app, &editor(), "delete_record", json!({ "id": a, "note": "删" }));
    publish_all(&app);
    let (published, draft): (Option<String>, Option<String>) =
        app.conn().query_row("SELECT published, draft FROM records WHERE id = ?1", [a], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((published, draft), (None, None)); // a tombstone, so the id is never reused
    // Its slug is free again.
    one(&app, "A again", "a");
}
