//! Access rules: one small expression language, checked when a collection is saved and decided by the database.

mod common;

use cms_harness::access::{self, Submitter};
use cms_harness::members::Member;
use cms_harness::schema::CollectionDef;
use cms_harness::{changes, rules, Shared};
use common::*;
use serde_json::{json, Value};

struct Site {
    app: Shared,
    def: CollectionDef,
    a: Member,
    b: Member,
    /// A's own note: text "A 的", stars 3, done true.
    mine: i64,
    /// A note written in the admin: no owner, stars 5, done false.
    admins: i64,
}

fn member(app: &Shared, email: &str) -> Member {
    let conn = app.conn();
    conn.execute("INSERT INTO members(email, password_hash, name) VALUES (?1, 'x', ?1)", [email]).unwrap();
    Member { id: conn.last_insert_rowid(), email: email.into(), name: email.into(), data: json!({}) }
}

fn site() -> Site {
    let app = app();
    ok(&app, &builder(), "set_member_login", json!({"enabled": true, "note": "登录"}));
    ok(&app, &builder(), "create_collection", json!({"name": "notes", "title": "留言", "read": "public", "create": "members", "note": "x", "fields": [
        {"name": "text", "title": "内容", "type": "text"}, {"name": "stars", "title": "评分", "type": "number"}, {"name": "done", "title": "完成", "type": "boolean"}]}));
    let admins = ok(&app, &builder(), "create_record", json!({"collection": "notes", "data": {"text": "后台写的", "stars": 5, "done": false}, "note": "x"}))["id"].as_i64().unwrap();
    publish_all(&app);
    let (a, b) = (member(&app, "a@example.com"), member(&app, "b@example.com"));
    let mine = access::submit(&app.conn(), "notes", &json!({"text": "A 的", "stars": 3, "done": true}), &Submitter::Member(a.id)).unwrap();
    let def = changes::live_def(&app.conn(), "notes").unwrap();
    Site { app, def, a, b, mine, admins }
}

impl Site {
    fn allows(&self, rule: &str, who: Option<&Member>, id: i64) -> bool {
        self.with(rule, who, id, None)
    }
    fn with(&self, rule: &str, who: Option<&Member>, id: i64, data: Option<&Value>) -> bool {
        rules::allows(&self.app.conn(), &self.def, rule, who, id, data).unwrap_or_else(|e| panic!("{rule}: {e}"))
    }
}

#[test]
fn the_keywords_mean_what_they_say() {
    let s = site();
    for (rule, owner, other, anon) in [("owner", true, false, false), ("members", true, true, false), ("public", true, true, true), ("none", false, false, false)] {
        assert_eq!(s.allows(rule, Some(&s.a), s.mine), owner, "{rule}: the owner");
        assert_eq!(s.allows(rule, Some(&s.b), s.mine), other, "{rule}: another member");
        assert_eq!(s.allows(rule, None, s.mine), anon, "{rule}: a visitor");
    }
}

#[test]
fn a_visitor_is_not_the_owner_of_a_record_nobody_owns() {
    let s = site();
    assert!(!s.allows("record.owner == member.id", None, s.admins), "no owner and no member are not equal");
    assert!(!s.allows("owner", None, s.admins));
    assert!(!s.allows("record.owner != member.id", None, s.admins), "unknown is not allowed either");
    assert!(!s.allows("not (record.owner == member.id)", None, s.admins));
}

#[test]
fn a_rule_reads_the_record_and_the_member() {
    let s = site();
    assert!(s.allows("record.stars >= 3 and record.text != '别的'", None, s.mine));
    assert!(!s.allows("record.stars > 3", None, s.mine));
    assert!(s.allows("record.stars > 3 or record.done", None, s.mine));
    assert!(!s.allows("record.done", None, s.admins), "false is not allowed");
    assert!(s.allows("not record.done", None, s.admins));
    assert!(s.allows("record.id == member.id or member.email == 'a@example.com'", Some(&s.a), s.mine));
    assert!(!s.allows("member.email == 'a@example.com'", Some(&s.b), s.mine));
    assert!(s.allows("(record.stars == 3 or record.stars == 5) and member", Some(&s.b), s.admins));
    assert!(s.allows("record.owner == null", None, s.admins));
    assert!(s.allows("record.owner != null && !(record.stars < 3)", None, s.mine), "&& || ! read the same");
    assert!(s.allows("true", None, s.mine) && !s.allows("false", None, s.mine));
}

#[test]
fn data_is_what_the_request_would_write() {
    let s = site();
    let rule = "data.stars == record.stars";
    assert!(s.with(rule, Some(&s.a), s.mine, Some(&json!({"text": "新的", "stars": 3}))));
    assert!(!s.with(rule, Some(&s.a), s.mine, Some(&json!({"text": "新的", "stars": 4}))));
    assert!(!s.with(rule, Some(&s.a), s.mine, None), "nothing written: unknown");
}

#[test]
fn text_in_a_rule_is_only_ever_text() {
    let s = site();
    assert!(!s.allows("record.text == \"x' OR 1=1 --\"", None, s.mine));
    assert!(!s.allows("record.text == 'x\" OR 1=1 --'", None, s.mine));
    let sly = Member { id: s.b.id, email: "' OR 1=1 --".into(), name: String::new(), data: json!({"kind": "' OR 1=1 --"}) };
    assert!(!s.allows("record.text == member.email", Some(&sly), s.mine));
}

#[test]
fn a_rule_is_checked_before_it_is_kept() {
    let s = site();
    for good in ["owner", "members", "public", "none", "record.stars >= 1 and (member or record.done)", "data.text != record.text"] {
        rules::check(&s.def, good).unwrap_or_else(|e| panic!("{good}: {e}"));
    }
    for (bad, names) in [
        ("record.nope == 1", "nope"),
        ("data.nope == 1", "nope"),
        ("visitor.id == 1", "visitor"),
        ("member.password == 'x'", "password"),
        ("record.stars ==", "=="),
        ("(record.stars == 1", ")"),
        ("record.stars == 1 record.done", "record"),
        ("record.text == 'open", "'"),
        ("record.stars @ 1", "@"),
        ("", "空"),
    ] {
        let e = rules::check(&s.def, bad).expect_err(bad);
        assert!(e.contains(names), "{bad}: {e}");
    }
    let long = format!("record.stars == 1{}", " or record.stars == 1".repeat(60));
    assert!(rules::check(&s.def, &long).is_err(), "a rule has a length limit");
}
