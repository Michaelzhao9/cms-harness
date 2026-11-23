mod common;
use common::*;
use serde_json::json;

fn add(app: &harness_cms::Shared, title: &str, slug: &str, tag: Option<&str>) -> i64 {
    let mut data = json!({ "title": title, "slug": slug });
    if let Some(tag) = tag {
        data["tag"] = json!(tag); // left out entirely when there is none: an explicit null would count as "cleared"
    }
    ok(app, &editor(), "create_record", json!({ "collection": "news", "note": "n", "data": data }))["id"].as_i64().unwrap()
}

#[test]
fn drafts_are_checked_too() {
    let app = app();
    news(&app); // never published: its records exist only as drafts
    add(&app, "A", "a", None);
    let e = err(&app, &builder(), "update_collection", json!({ "name": "news", "note": "n",
        "change_fields": [{ "name": "tag", "required": true }] }));
    assert!(e.contains('1') && e.contains("tag"), "{e}");
    assert!(!e.contains("。。"), "{e}");
    // With a default the same change is fine: the record never had a tag, so the default applies.
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "note": "n",
        "change_fields": [{ "name": "tag", "required": true, "default": "公司" }] }));
    // A record whose tag was cleared on purpose (explicit null) still blocks it.
    ok(&app, &builder(), "discard_draft", json!({ "target": "collection:news" }));
    news(&app);
    let b = add(&app, "B", "b", Some("行业"));
    ok(&app, &editor(), "update_record", json!({ "id": b, "note": "n", "data": { "tag": null } }));
    assert!(err(&app, &builder(), "update_collection", json!({ "name": "news", "note": "n",
        "change_fields": [{ "name": "tag", "required": true, "default": "公司" }] })).contains("tag"));
}

#[test]
fn a_record_does_not_collide_with_its_own_draft() {
    let app = app();
    news(&app);
    let a = add(&app, "同名", "a", Some("公司"));
    add(&app, "另一条", "b", Some("行业"));
    publish_all(&app);
    // Record a now has a published copy and a draft copy with the same title.
    ok(&app, &editor(), "update_record", json!({ "id": a, "note": "n", "data": { "tag": "行业" } }));
    ok(&app, &builder(), "update_collection", json!({ "name": "news", "note": "n", "change_fields": [{ "name": "title", "unique": true }] }));

    // A real duplicate between two records is still caught, and both are counted.
    ok(&app, &builder(), "discard_draft", json!({ "target": "collection:news" }));
    add(&app, "同名", "c", None);
    let e = err(&app, &builder(), "update_collection", json!({ "name": "news", "note": "n", "change_fields": [{ "name": "title", "unique": true }] }));
    assert!(e.contains("有 2 条") && e.contains("title"), "{e}");
}

#[test]
fn removal_counts_records_holding_any_of_the_fields() {
    let app = app();
    news(&app);
    add(&app, "A", "a", Some("公司")); // holds tag and slug
    add(&app, "B", "b", None); // holds slug only
    publish_all(&app);
    let b = builder();
    let e = err(&app, &b, "update_collection", json!({ "name": "news", "note": "n", "remove_fields": ["tag"] }));
    assert!(e.contains("confirm: 1"), "{e}");
    let e = err(&app, &b, "update_collection", json!({ "name": "news", "note": "n", "remove_fields": ["tag", "slug"] }));
    assert!(e.contains("confirm: 2"), "{e}");
    // A stale count from before another edit is refused.
    assert!(err(&app, &b, "update_collection", json!({ "name": "news", "note": "n", "remove_fields": ["tag", "slug"], "confirm": 1 })).contains("confirm: 2"));
    ok(&app, &b, "update_collection", json!({ "name": "news", "note": "n", "remove_fields": ["tag", "slug"], "confirm": 2 }));
}

#[test]
fn a_name_pending_deletion_is_simply_taken() {
    let app = app();
    news(&app);
    publish_all(&app);
    ok(&app, &builder(), "delete_collection", json!({ "name": "news", "note": "n" })); // no records: no confirm needed
    let e = err(&app, &builder(), "create_collection", json!({ "name": "news", "title": "新的", "fields": [], "note": "n" }));
    assert!(e.contains("已存在") && !e.contains("restore_version"), "{e}");
}
