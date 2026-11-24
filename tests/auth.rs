use cms_harness::{auth::*, store};

#[test]
fn roles_map_to_scopes() {
    assert_eq!(role_scopes("builder"), [Scope::Read, Scope::Write, Scope::Structure, Scope::Publish]);
    assert_eq!(role_scopes("editor"), [Scope::Read, Scope::Write, Scope::Publish]);
    assert!(role_scopes("root").is_empty());
}

#[test]
fn passwords_hash_and_verify() {
    let h = hash_password("correct horse");
    assert!(h.starts_with("$argon2"));
    assert!(verify_password(&h, "correct horse"));
    assert!(!verify_password(&h, "wrong"));
    assert!(!verify_password("not a hash", "x"));
}

#[test]
fn admin_is_created_then_reset() {
    let conn = store::open(":memory:").unwrap();
    let first = create_admin(&conn, "a@example.com", "builder", None).unwrap();
    assert_eq!(first.len(), 20);
    assert!(first.chars().all(|c| c.is_ascii_alphanumeric()));
    assert_eq!(create_admin(&conn, "a@example.com", "editor", Some("chosen-password")).unwrap(), "chosen-password");
    let (count, role): (i64, String) = conn.query_row("SELECT count(*), max(role) FROM admins", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((count, role.as_str()), (1, "editor"));
    let hash: String = conn.query_row("SELECT password_hash FROM admins", [], |r| r.get(0)).unwrap();
    assert!(verify_password(&hash, "chosen-password"));
    assert!(create_admin(&conn, "b@example.com", "root", None).is_err());
    assert!(create_admin(&conn, "", "editor", None).is_err());
}

#[test]
fn keys_authenticate_until_revoked() {
    let conn = store::open(":memory:").unwrap();
    let key = create_key(&conn, "ci", &[Scope::Write]).unwrap();
    assert!(key.starts_with("hc_") && key.len() == 43);
    assert!(key[3..].chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
    let stored: String = conn.query_row("SELECT key_hash FROM api_keys", [], |r| r.get(0)).unwrap();
    assert!(!stored.contains(&key[3..]));
    let actor = actor_from_key(&conn, &key).unwrap();
    assert_eq!(actor.name, "key:ci");
    assert!(actor.can(Scope::Read) && actor.can(Scope::Write) && !actor.can(Scope::Publish) && !actor.admin);
    assert!(actor_from_key(&conn, "hc_0000").is_none());
    assert!(revoke_key(&conn, 99).is_err());
    revoke_key(&conn, 1).unwrap();
    assert!(actor_from_key(&conn, &key).is_none());
}

#[test]
fn scope_parse_and_serde() {
    assert_eq!(Scope::parse("publish"), Some(Scope::Publish));
    assert_eq!(Scope::parse("nope"), None);
    assert_eq!(serde_json::to_string(&Scope::Structure).unwrap(), "\"structure\"");
}
