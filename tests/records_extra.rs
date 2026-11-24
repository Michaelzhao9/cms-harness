mod common;
use common::*;
use cms_harness::Shared;
use serde_json::{json, Value};

fn items(q: &Value) -> Vec<String> {
    q["items"].as_array().unwrap().iter().map(|i| i["data"]["name"].as_str().unwrap_or("").to_string()).collect()
}

fn setup() -> Shared {
    let app = app();
    ok(&app, &builder(), "create_collection", json!({ "name": "t", "title": "测试", "note": "n", "fields": [
        { "name": "name", "title": "名", "type": "text", "index": true },
        { "name": "d", "title": "日期", "type": "date", "index": true },
        { "name": "n", "title": "数", "type": "number", "index": true },
        { "name": "flag", "title": "标", "type": "boolean", "index": true },
        { "name": "kind", "title": "类", "type": "select", "options": ["a", "b"], "index": true },
        { "name": "plain", "title": "无索引", "type": "text" },
        { "name": "u", "title": "唯一", "type": "text", "unique": true }
    ] }));
    app
}

fn add(app: &Shared, data: Value) -> i64 {
    ok(app, &editor(), "create_record", json!({ "collection": "t", "note": "n", "data": data }))["id"].as_i64().unwrap()
}

fn q(app: &Shared, args: Value) -> Result<Value, String> {
    cms_harness::tools::call(app, &reader(), "query_records", args)
}

#[test]
fn contains_escapes_wildcards() {
    let app = setup();
    add(&app, json!({ "name": "50%_a\\b" }));
    add(&app, json!({ "name": "50xxaxb" }));
    for needle in ["%", "_", "\\", "50%_a\\b"] {
        let r = q(&app, json!({ "collection": "t", "filter": { "name": { "contains": needle } } })).unwrap();
        assert_eq!(items(&r), vec!["50%_a\\b"], "needle {needle}");
    }
    assert!(q(&app, json!({ "collection": "t", "filter": { "d": { "contains": "2" } } })).is_err());
}

#[test]
fn every_operator_on_date_and_number() {
    let app = setup();
    for (i, d) in ["2026-01-01", "2026-02-01", "2026-03-01"].iter().enumerate() {
        add(&app, json!({ "name": format!("r{i}"), "d": d, "n": (i + 1) * 10 }));
    }
    let cases = [("eq", 1), ("ne", 2), ("gt", 1), ("gte", 2), ("lt", 1), ("lte", 2)];
    for (field, a) in [("d", json!("2026-02-01")), ("n", json!(20))] {
        for (op, want) in cases {
            let r = q(&app, json!({ "collection": "t", "filter": { field: { op: a } } })).unwrap();
            assert_eq!(r["total"], want, "{field} {op}");
        }
    }
    let r = q(&app, json!({ "collection": "t", "filter": { "n": 20 } })).unwrap();
    assert_eq!(items(&r), vec!["r1"]);
}

#[test]
fn bad_filters_and_paging_are_errors() {
    let app = setup();
    add(&app, json!({ "name": "a" }));
    let e = q(&app, json!({ "collection": "t", "filter": { "n": { "like": 1 } } })).unwrap_err();
    assert!(e.contains("like"), "{e}");
    for (k, v) in [("limit", 0), ("limit", 101), ("offset", -1)] {
        let e = q(&app, json!({ "collection": "t", k: v })).unwrap_err();
        assert!(e.contains(k) && (e.contains("1 到 100") || e.contains("0")), "{e}");
    }
    assert!(q(&app, json!({ "collection": "t", "limit": 0 })).unwrap_err().contains("1 到 100"));
    assert!(q(&app, json!({ "collection": "t", "filter": { "plain": "x" } })).unwrap_err().contains("plain"));
    assert!(q(&app, json!({ "collection": "t", "sort": "plain" })).unwrap_err().contains("plain"));
    assert!(q(&app, json!({ "collection": "t", "filter": { "d": {} } })).unwrap_err().contains("d"));
}

#[test]
fn filter_values_match_field_types() {
    let app = setup();
    add(&app, json!({ "name": "a" }));
    for (f, v) in [("n", json!("5")), ("d", json!(5)), ("d", json!("2026-13-45")), ("kind", json!("zzz")), ("flag", json!("true")), ("id", json!("1"))] {
        let e = q(&app, json!({ "collection": "t", "filter": { f: v } })).unwrap_err();
        assert!(e.contains(f), "{e}");
    }
    assert!(q(&app, json!({ "collection": "t", "filter": { "kind": "a" } })).is_ok());
}

#[test]
fn booleans_bind_and_injection_is_inert() {
    let app = setup();
    add(&app, json!({ "name": "yes", "flag": true }));
    add(&app, json!({ "name": "no", "flag": false }));
    let r = q(&app, json!({ "collection": "t", "filter": { "flag": true } })).unwrap();
    assert_eq!(items(&r), vec!["yes"]);
    let r = q(&app, json!({ "collection": "t", "filter": { "flag": false } })).unwrap();
    assert_eq!(items(&r), vec!["no"]);
    let r = q(&app, json!({ "collection": "t", "filter": { "name": "x' OR '1'='1" } })).unwrap();
    assert_eq!(r["total"], 0);
}

#[test]
fn id_sort_and_filter() {
    let app = setup();
    let ids: Vec<i64> = (0..3).map(|i| add(&app, json!({ "name": format!("r{i}") }))).collect();
    let r = q(&app, json!({ "collection": "t", "sort": "-id" })).unwrap();
    assert_eq!(items(&r), vec!["r2", "r1", "r0"]);
    let r = q(&app, json!({ "collection": "t", "sort": "id" })).unwrap();
    assert_eq!(items(&r), vec!["r0", "r1", "r2"]);
    let r = q(&app, json!({ "collection": "t", "filter": { "id": { "gt": ids[0] } } })).unwrap();
    assert_eq!(r["total"], 2);
}

#[test]
fn uniqueness_error_names_field_value_and_other_id() {
    let app = setup();
    let a = add(&app, json!({ "name": "a", "u": "dup" }));
    let e = err(&app, &editor(), "create_record", json!({ "collection": "t", "note": "n", "data": { "u": "dup" } }));
    assert!(e.contains("u") && e.contains("dup") && e.contains(&a.to_string()), "{e}");
    let long = "长".repeat(80);
    add(&app, json!({ "u": long }));
    let e = err(&app, &editor(), "create_record", json!({ "collection": "t", "note": "n", "data": { "u": long } }));
    assert!(e.contains('…') && !e.contains(&"长".repeat(51)), "{e}");
}
