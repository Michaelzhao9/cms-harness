//! Totals over a collection: how many, the sum and average of a number field, by group. For the admin's tools
//! and for server functions.

mod common;

use common::*;
use serde_json::{json, Value};

/// Five orders: three paid (100, 200, 50), one refunded (80), one still a draft (999, paid).
fn orders(app: &cms_harness::Shared) {
    ok(app, &builder(), "create_collection", json!({"name": "orders", "title": "订单", "note": "x", "fields": [
        {"name": "item", "title": "商品", "type": "text"},
        {"name": "amount", "title": "金额", "type": "number"},
        {"name": "state", "title": "状态", "type": "select", "options": ["paid", "refunded"], "index": true}]}));
    for (amount, state) in [(100, "paid"), (200, "paid"), (50, "paid"), (80, "refunded")] {
        ok(app, &builder(), "create_record", json!({"collection": "orders", "data": {"item": "杯", "amount": amount, "state": state}, "note": "x"}));
    }
    publish_all(app);
    ok(app, &builder(), "create_record", json!({"collection": "orders", "data": {"item": "壶", "amount": 999, "state": "paid"}, "note": "x"}));
}

fn total(app: &cms_harness::Shared, args: Value) -> Value {
    let mut a = json!({"collection": "orders"});
    a.as_object_mut().unwrap().extend(args.as_object().cloned().unwrap());
    ok(app, &reader(), "aggregate_records", a)
}

#[test]
fn counts_sums_and_averages_with_the_same_filters_as_a_list() {
    let app = app();
    orders(&app);
    let live = total(&app, json!({"status": ["published"], "sum": ["amount"], "avg": ["amount"], "min": ["amount"], "max": ["amount"]}));
    assert_eq!((live["count"].as_i64(), live["sum"]["amount"].as_f64(), live["avg"]["amount"].as_f64()), (Some(4), Some(430.0), Some(107.5)));
    assert_eq!((live["min"]["amount"].as_f64(), live["max"]["amount"].as_f64()), (Some(50.0), Some(200.0)));
    let paid = total(&app, json!({"status": ["published"], "filter": {"state": "paid"}, "sum": ["amount"]}));
    assert_eq!((paid["count"].as_i64(), paid["sum"]["amount"].as_f64()), (Some(3), Some(350.0)));
    assert_eq!(total(&app, json!({}))["count"], 5, "drafts count unless only published ones are asked for");
    assert_eq!(total(&app, json!({"search": "壶", "sum": ["amount"]}))["sum"]["amount"], 999.0);
    let none = total(&app, json!({"filter": {"state": "refunded"}, "status": ["draft"], "sum": ["amount"], "avg": ["amount"]}));
    assert_eq!((none["count"].as_i64(), &none["sum"]["amount"], &none["avg"]["amount"]), (Some(0), &json!(0.0), &Value::Null), "nothing to add up is zero; nothing to average is nothing");
}

#[test]
fn by_group() {
    let app = app();
    orders(&app);
    let by = total(&app, json!({"status": ["published"], "group_by": "state", "sum": ["amount"]}));
    assert_eq!(by["count"], 4);
    assert_eq!(by["groups"], json!([{"value": "paid", "count": 3, "sum": {"amount": 350.0}}, {"value": "refunded", "count": 1, "sum": {"amount": 80.0}}]), "largest group first");
}

#[test]
fn only_what_the_collection_has_and_only_numbers_add_up() {
    let app = app();
    orders(&app);
    for (args, names) in [(json!({"sum": ["nope"]}), "nope"), (json!({"sum": ["item"]}), "item"), (json!({"group_by": "nope"}), "nope"), (json!({"avg": ["state"]}), "state")] {
        let e = err(&app, &reader(), "aggregate_records", { let mut a = json!({"collection": "orders"}); a.as_object_mut().unwrap().extend(args.as_object().cloned().unwrap()); a });
        assert!(e.contains(names), "{args}: {e}");
    }
}

#[tokio::test]
async fn a_function_adds_up_live_data() {
    let app = app();
    orders(&app);
    ok(&app, &builder(), "write_site_file", json!({"path": "_fn/stats.js", "note": "x",
        "content": "export default () => ({ all: db.aggregate('orders', { sum: ['amount'] }), by: db.aggregate('orders', { group_by: 'state', sum: ['amount'], filter: { state: 'paid' } }) })"}));
    let r = ok(&app, &reader(), "run_function", json!({"path": "/_fn/stats"}));
    assert_eq!(r["status"], 200, "{r}");
    assert_eq!((r["body"]["all"]["count"].as_i64(), r["body"]["all"]["sum"]["amount"].as_f64()), (Some(4), Some(430.0)), "live records only: {r}");
    // Through the script a whole number comes back without its ".0": compare what it is worth.
    let groups = r["body"]["by"]["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!((groups[0]["value"].as_str(), groups[0]["count"].as_i64(), groups[0]["sum"]["amount"].as_f64()), (Some("paid"), Some(3), Some(350.0)));
}
