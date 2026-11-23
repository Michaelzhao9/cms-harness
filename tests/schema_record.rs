use harness_cms::schema::*;
use serde_json::{json, Value};

fn news() -> CollectionDef {
    serde_json::from_value(json!({ "name": "news", "title": "新闻", "fields": [
        { "name": "title", "title": "标题", "type": "text", "required": true },
        { "name": "body", "title": "正文", "type": "richtext" },
        { "name": "date", "title": "日期", "type": "date" },
        { "name": "views", "title": "浏览", "type": "number", "default": 0 },
        { "name": "tag", "title": "分类", "type": "select", "options": ["公司", "行业"] },
        { "name": "cover", "title": "封面", "type": "media" },
        { "name": "related", "title": "相关", "type": "relation", "target": "news", "multiple": true }
    ]})).unwrap()
}

#[test]
fn creates_and_merges() {
    let d = news();
    let doc = normalize(&d, &json!({}), &json!({ "title": "开业", "tag": "公司" })).unwrap();
    assert_eq!(doc, json!({ "title": "开业", "tag": "公司" }));
    let doc = normalize(&d, &doc, &json!({ "date": "2026-11-23" })).unwrap();
    assert_eq!(doc["title"], "开业");
    assert_eq!(doc["date"], "2026-11-23");
}

#[test]
fn keeps_retired_keys_but_rejects_unknown_input() {
    let d = news();
    let base = json!({ "title": "旧", "old_field": "保留" });
    let doc = normalize(&d, &base, &json!({ "title": "新" })).unwrap();
    assert_eq!(doc["old_field"], "保留");
    assert!(normalize(&d, &base, &json!({ "nope": 1 })).unwrap_err().contains("nope"));
    assert_eq!(project(&d, &doc).get("old_field"), None);
    assert_eq!(project(&d, &doc)["views"], 0); // default applied on read
}

#[test]
fn validates_each_type() {
    let d = news();
    let bad = [
        json!({}),                                             // required title missing
        json!({ "title": "" }),                                // required but empty
        json!({ "title": 1 }),                                 // text must be a string
        json!({ "title": "a", "date": "2026-02-30" }),         // not a real date
        json!({ "title": "a", "date": "23/11/2026" }),
        json!({ "title": "a", "views": "3" }),
        json!({ "title": "a", "tag": "其他" }),                 // not an option
        json!({ "title": "a", "cover": "javascript:alert(1)" }),
        json!({ "title": "a", "cover": "/etc/passwd" }),
        json!({ "title": "a", "related": 3 }),                 // multiple wants an array
        json!({ "title": "a", "related": [3, 3] }),            // duplicates
        json!({ "title": "x".repeat(MAX_TEXT + 1) }),
    ];
    for c in bad { assert!(normalize(&d, &json!({}), &c).is_err(), "{c}"); }
    let good = json!({ "title": "a", "date": "2026-11-23T08:30:00Z", "views": 3.5, "cover": "/uploads/2026/11/ab.png",
                       "related": [1, 2], "tag": null });
    assert!(normalize(&d, &json!({}), &good).is_ok());
}

#[test]
fn richtext_is_sanitised() {
    let d = news();
    let doc = normalize(&d, &json!({}), &json!({ "title": "a",
        "body": "<p onclick=\"x()\" style=\"color:red\">好<script>alert(1)</script><a href=\"javascript:x\">k</a><a href=\"/about\">ok</a></p>" })).unwrap();
    let body = doc["body"].as_str().unwrap();
    for gone in ["script", "onclick", "style", "javascript:"] { assert!(!body.contains(gone), "{gone} in {body}"); }
    assert!(body.contains("href=\"/about\""));
}

#[test]
fn converts_between_types() {
    let f = |v: Value| -> FieldDef { serde_json::from_value(v).unwrap() };
    let text = f(json!({ "name": "a", "title": "A", "type": "text" }));
    let number = f(json!({ "name": "a", "title": "A", "type": "number" }));
    let multi = f(json!({ "name": "a", "title": "A", "type": "select", "options": ["x", "y"], "multiple": true }));
    let single = f(json!({ "name": "a", "title": "A", "type": "select", "options": ["x", "y"] }));
    assert_eq!(convert(&json!("12.5"), &text, &number), Some(json!(12.5)));
    assert_eq!(convert(&json!("abc"), &text, &number), None);
    assert_eq!(convert(&json!(7), &number, &text), Some(json!("7")));
    assert_eq!(convert(&json!("x"), &single, &multi), Some(json!(["x"])));
    assert_eq!(convert(&json!(["x"]), &multi, &single), Some(json!("x")));
    assert_eq!(convert(&json!(["x", "y"]), &multi, &single), None);
    assert_eq!(convert(&Value::Null, &text, &number), Some(Value::Null));
}

#[test]
fn migrate_reports_what_fails() {
    let old = news();
    let mut new = news();
    new.fields[3].kind = FieldType::Text;      // views: number -> text
    new.fields[3].default = None;
    assert_eq!(migrate(&old, &new, &json!({ "title": "a", "views": 3 })).unwrap()["views"], "3");
    new.fields[1].required = true;             // body now required
    assert!(migrate(&old, &new, &json!({ "title": "a" })).unwrap_err().contains("body"));
}

#[test]
fn dates_leap_years_and_extra_conversions() {
    let d = news();
    let ok = |s: &str| normalize(&d, &json!({}), &json!({ "title": "a", "date": s })).is_ok();
    assert!(ok("2024-02-29") && !ok("2026-02-29") && !ok("2100-02-29") && ok("2000-02-29"));
    assert!(!ok("2026-11-23T24:00:00Z") && !ok("2026-11-23T08:60:00Z") && !ok("2026-13-01") && !ok("2026-11-23T08:30:00"));
    let f = |v: Value| -> FieldDef { serde_json::from_value(v).unwrap() };
    let text = f(json!({ "name": "a", "title": "A", "type": "text" }));
    let rich = f(json!({ "name": "a", "title": "A", "type": "richtext" }));
    let number = f(json!({ "name": "a", "title": "A", "type": "number" }));
    assert_eq!(convert(&json!(12.5), &number, &text), Some(json!("12.5")));
    assert_eq!(convert(&json!("<p>a &amp; b</p>"), &rich, &text), Some(json!("a & b")));
    assert_eq!(convert(&json!("a<b"), &text, &rich), Some(json!("a&lt;b")));
    // a default of the right JSON type but invalid content is rejected
    let bad: CollectionDef = serde_json::from_value(json!({ "name": "c", "title": "C", "fields": [
        { "name": "d", "title": "D", "type": "date", "default": "2026-02-30" }] })).unwrap();
    assert!(check_def(&bad).unwrap_err().contains("d"));
}
