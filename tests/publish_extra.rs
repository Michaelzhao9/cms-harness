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

#[test]
fn a_deleted_record_stays_restorable_whatever_is_done_to_its_tombstone() {
    let app = app();
    news(&app);
    let a = one(&app, "A", "a");
    publish_all(&app);
    ok(&app, &editor(), "delete_record", json!({ "id": a, "note": "删" }));
    publish_all(&app);
    let target = format!("record:{a}");
    let live = |app: &harness_cms::Shared| ok(app, &reader(), "list_versions", json!({ "target": format!("record:{a}") }))[1]["id"].clone();
    let rows = |app: &harness_cms::Shared| -> i64 { app.conn().query_row("SELECT count(*) FROM records WHERE id = ?1", [a], |r| r.get(0)).unwrap() };

    // Discarding on a bare tombstone keeps the row.
    assert_eq!(ok(&app, &editor(), "discard_draft", json!({ "target": target }))["status"], "deleted");
    assert_eq!(rows(&app), 1);
    // Restore, then discard the restored draft: still a tombstone.
    ok(&app, &editor(), "restore_version", json!({ "target": target, "version_id": live(&app), "note": "找回" }));
    assert_eq!(ok(&app, &editor(), "discard_draft", json!({ "target": target }))["status"], "deleted");
    assert_eq!(rows(&app), 1);
    // Restore, then delete the restored draft: still a tombstone.
    ok(&app, &editor(), "restore_version", json!({ "target": target, "version_id": live(&app), "note": "找回" }));
    assert_eq!(ok(&app, &editor(), "delete_record", json!({ "id": a, "note": "又不要了" }))["status"], "deleted");
    assert_eq!(rows(&app), 1);
    // And it can still come back under the same id.
    ok(&app, &editor(), "restore_version", json!({ "target": target, "version_id": live(&app), "note": "找回" }));
    publish_all(&app);
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": a }))["data"]["title"], "A");
}

#[test]
fn history_holds_what_went_live_after_a_definition_change() {
    let app = app();
    news(&app);
    let a = ok(&app, &editor(), "create_record", json!({ "collection": "news", "note": "n", "data": { "title": "A", "slug": "a", "tag": "公司" } }))["id"].as_i64().unwrap();
    let b = ok(&app, &editor(), "create_record", json!({ "collection": "news", "note": "n", "data": { "title": "B", "slug": "b", "tag": "行业" } }))["id"].as_i64().unwrap();
    publish_all(&app);
    // tag becomes multi-valued, and record A is edited in the same release.
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "改标题", "data": { "title": "A2" } }));
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "note": "标签可多选", "change_fields": [{ "name": "tag", "multiple": true, "index": false }] }));
    publish_all(&app);

    let stored = |id: i64| -> (serde_json::Value, serde_json::Value) {
        let conn = app.conn();
        let live: String = conn.query_row("SELECT published FROM records WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        let doc: String = conn.query_row("SELECT doc FROM versions WHERE target = ?1 ORDER BY id DESC LIMIT 1", [format!("record:{id}")], |r| r.get(0)).unwrap();
        (serde_json::from_str(&live).unwrap(), serde_json::from_str(&doc).unwrap())
    };
    let (live_a, version_a) = stored(a);
    assert_eq!(live_a["tag"], json!(["公司"]));
    assert_eq!(version_a, live_a); // the release's own record
    let (live_b, version_b) = stored(b);
    assert_eq!(live_b["tag"], json!(["行业"]));
    assert_eq!(version_b, live_b); // a record rewritten only because the definition changed
    let v = ok(&app, &reader(), "list_versions", json!({ "target": format!("record:{b}") }));
    assert!(v[0]["note"].as_str().unwrap().contains("自动转换"), "{v}");
}

#[test]
fn a_document_that_cannot_follow_blocks_the_definition_change_at_publish() {
    let app = app();
    news(&app);
    let a = ok(&app, &editor(), "create_record", json!({ "collection": "news", "note": "n", "data": { "title": "A", "slug": "a", "tag": "公司" } }))["id"].as_i64().unwrap();
    publish_all(&app);
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "note": "n", "change_fields": [{ "name": "tag", "multiple": true, "index": false }] }));
    // Another writer leaves a value behind that the new definition cannot take.
    app.conn().execute("UPDATE records SET published = '{\"title\":\"A\",\"slug\":\"a\",\"tag\":\"不存在的选项\"}' WHERE id = ?1", [a]).unwrap();
    let e = err(&app, &builder(), "publish", json!({ "note": "n" }));
    assert!(e.contains(&format!("record:{a}")) && e.contains("tag"), "{e}");
    assert_eq!(ok(&app, &reader(), "get_collection", json!({ "name": "news" }))["status"], "changed");
}

#[test]
fn drafts_left_in_a_deleted_collection_do_not_block_later_releases() {
    let app = app();
    news(&app);
    let a = one(&app, "A", "a");
    publish_all(&app);
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "改到一半", "data": { "title": "A2" } }));
    ok(&app, &builder(), "delete_collection", json!({ "name": "news", "note": "不要了", "confirm": 1 }));
    // Naming the draft together with the deletion explains what to do.
    assert!(err(&app, &builder(), "publish", json!({ "note": "n", "targets": ["collection:news", format!("record:{a}")] })).contains("discard_draft"));
    // Publishing everything deletes the collection and leaves the draft alone.
    let release = publish_all(&app);
    assert_eq!(release["published"], json!(["collection:news"]));
    assert_eq!(release["skipped"], json!([format!("record:{a}")]));
    // Later releases are not held up by it.
    ok(&app, &builder(), "create_collection", json!({ "name": "pages", "title": "页面", "fields": [], "note": "n" }));
    assert_eq!(publish_all(&app)["published"], json!(["collection:pages"]));
    // With nothing else pending, the message says why nothing can be published.
    let e = err(&app, &builder(), "publish", json!({ "note": "n" }));
    assert!(e.contains(&format!("record:{a}")) && e.contains("已删除"), "{e}");
}

#[test]
fn an_empty_target_list_is_called_out() {
    let app = app();
    news(&app);
    assert!(err(&app, &builder(), "publish", json!({ "note": "n", "targets": [] })).contains("targets"));
}

#[test]
fn a_record_deletion_is_never_held_back_by_its_collection() {
    let app = app();
    news(&app);
    let (a, b) = (one(&app, "A", "a"), one(&app, "B", "b"));
    publish_all(&app);
    ok(&app, &editor(), "delete_record", json!({ "id": a, "note": "删" }));
    ok(&app, &builder(), "delete_collection", json!({ "name": "news", "note": "删", "confirm": 2 }));
    let release = publish_all(&app);
    assert_eq!(release["skipped"], json!([]));
    assert_eq!(release["published"].as_array().unwrap().len(), 2);
    // Bring the collection back: the deleted record must stay deleted.
    let v = ok(&app, &reader(), "list_versions", json!({ "target": "collection:news" }));
    ok(&app, &builder(), "restore_version", json!({ "target": "collection:news", "version_id": v[1]["id"], "note": "恢复" }));
    publish_all(&app);
    let q = ok(&app, &reader(), "query_records", json!({ "collection": "news" }));
    assert_eq!((q["total"].as_i64(), q["items"][0]["id"].as_i64()), (Some(1), Some(b)));
}

#[test]
fn a_restored_collection_and_its_record_drafts_go_out_together() {
    let app = app();
    news(&app);
    let a = one(&app, "A", "a");
    publish_all(&app);
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "改", "data": { "title": "A2" } }));
    ok(&app, &builder(), "delete_collection", json!({ "name": "news", "note": "删", "confirm": 1 }));
    publish_all(&app); // the deletion goes out, the draft is held
    let v = ok(&app, &reader(), "list_versions", json!({ "target": "collection:news" }));
    ok(&app, &builder(), "restore_version", json!({ "target": "collection:news", "version_id": v[1]["id"], "note": "恢复" }));
    // The collection is visible again while its return is pending.
    let listed = ok(&app, &reader(), "list_collections", json!({}));
    assert_eq!((listed[0]["name"].as_str(), listed[0]["status"].as_str()), (Some("news"), Some("changed")));
    let release = publish_all(&app);
    assert_eq!(release["published"], json!(["collection:news", format!("record:{a}")]));
    assert_eq!(ok(&app, &reader(), "get_record", json!({ "id": a }))["data"]["title"], "A2");
}

#[test]
fn a_collection_only_marked_for_deletion_does_not_hold_drafts_back() {
    let app = app();
    news(&app);
    let a = one(&app, "A", "a");
    publish_all(&app);
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "改", "data": { "title": "A2" } }));
    ok(&app, &builder(), "delete_collection", json!({ "name": "news", "note": "删", "confirm": 1 }));
    // The editor cannot publish the deletion, so the collection stays live and the edit goes out.
    let release = ok(&app, &editor(), "publish", json!({ "note": "n" }));
    assert_eq!(release["published"], json!([format!("record:{a}")]));
    assert_eq!(release["skipped"], json!(["collection:news"]));
}

#[test]
fn a_draft_that_cannot_follow_is_reported_by_the_definition_change_itself() {
    let app = app();
    news(&app);
    publish_all(&app);
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "note": "n", "change_fields": [{ "name": "tag", "multiple": true, "index": false }] }));
    // A draft written behind the tools' back, not part of the release.
    app.conn().execute("INSERT INTO records(collection, draft) VALUES('news', '{\"title\":\"x\",\"slug\":\"q\",\"tag\":\"不存在的选项\"}')", []).unwrap();
    let id = app.conn().last_insert_rowid();
    let e = err(&app, &builder(), "publish", json!({ "note": "n", "targets": ["collection:news"] }));
    assert!(e.contains(&format!("record:{id}")) && e.contains("tag"), "{e}");
}

#[test]
fn a_record_deleted_in_the_release_cannot_block_its_collections_change() {
    let app = app();
    news(&app);
    let a = ok(&app, &editor(), "create_record", json!({ "collection": "news", "note": "n", "data": { "title": "A", "slug": "a", "tag": "公司" } }))["id"].as_i64().unwrap();
    publish_all(&app);
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "note": "n", "change_fields": [{ "name": "tag", "multiple": true, "index": false }] }));
    // Its live copy can no longer follow, but the record is being deleted in the same release.
    app.conn().execute("UPDATE records SET published = '{\"title\":\"A\",\"slug\":\"a\",\"tag\":\"不存在的选项\"}' WHERE id = ?1", [a]).unwrap();
    ok(&app, &editor(), "delete_record", json!({ "id": a, "note": "删" }));
    assert_eq!(publish_all(&app)["published"].as_array().unwrap().len(), 2);
}
