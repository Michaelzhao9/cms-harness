use cms_harness::schema::*;
use serde_json::json;

fn def(fields: serde_json::Value) -> CollectionDef {
    serde_json::from_value(json!({ "name": "news", "title": "新闻", "fields": fields })).unwrap()
}

#[test]
fn names_are_strict() {
    for good in ["news", "a", "news_2026", "x1"] { assert!(valid_name(good), "{good}"); }
    let too_long = "a".repeat(41);
    for bad in ["", "News", "1a", "a-b", "a b", "新闻", "a'b", "a\"b", "$.a", "a;drop", "id", "sort", "limit", "offset", "expand",
                too_long.as_str()] {
        assert!(!valid_name(bad), "{bad}");
    }
}

#[test]
fn accepts_a_normal_definition() {
    let d = def(json!([
        { "name": "title", "title": "标题", "type": "text", "required": true },
        { "name": "slug", "title": "网址", "type": "text", "unique": true },
        { "name": "tag", "title": "分类", "type": "select", "options": ["公司", "行业"], "index": true },
        { "name": "author", "title": "作者", "type": "relation", "target": "people" }
    ]));
    assert_eq!(check_def(&d), Ok(()));
}

#[test]
fn rejects_bad_definitions() {
    let cases = [
        json!([{ "name": "Title", "title": "标题", "type": "text" }]),                              // bad field name
        json!([{ "name": "a", "title": "A", "type": "text" }, { "name": "a", "title": "B", "type": "text" }]), // duplicate
        json!([{ "name": "a", "title": "", "type": "text" }]),                                     // empty title
        json!([{ "name": "a", "title": "A", "type": "select" }]),                                  // select without options
        json!([{ "name": "a", "title": "A", "type": "text", "options": ["x"] }]),                  // options on non-select
        json!([{ "name": "a", "title": "A", "type": "relation" }]),                                // relation without target
        json!([{ "name": "a", "title": "A", "type": "text", "multiple": true }]),                  // multiple on text
        json!([{ "name": "a", "title": "A", "type": "richtext", "unique": true }]),                // unique on richtext
        json!([{ "name": "a", "title": "A", "type": "select", "options": ["x"], "multiple": true, "index": true }]), // index on multi
        json!([{ "name": "a", "title": "A", "type": "number", "default": "x" }]),                  // default of wrong type
    ];
    for fields in cases { assert!(check_def(&def(fields.clone())).is_err(), "{fields}"); }
}

#[test]
fn unknown_keys_are_rejected_on_parse() {
    let r: Result<CollectionDef, _> = serde_json::from_value(json!({ "name": "a", "title": "A", "fields": [], "extra": 1 }));
    assert!(r.is_err());
}
