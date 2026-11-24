use harness_cms::skills::*;
use std::io::Write;

const MD: &str = "---\nname: brand-voice\ndescription: \"Write copy in the house style\"\n---\n\n# Brand voice\n\nUse short sentences.\n";

fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in files {
        w.start_file(*name, opts).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

#[test]
fn front_matter_gives_name_description_and_body() {
    let (name, description, body) = parse_skill_md(MD).unwrap();
    assert_eq!((name.as_str(), description.as_str()), ("brand-voice", "Write copy in the house style"));
    assert!(body.starts_with("# Brand voice"));
    assert!(parse_skill_md("# no front matter").is_err());
    assert!(parse_skill_md("---\nname: Bad Name\ndescription: x\n---\n").unwrap_err().contains("不合规"));
    assert!(parse_skill_md("---\nname: ok\n---\n").unwrap_err().contains("description"));
}

#[test]
fn a_zip_yields_the_skill_folder_text_files_only() {
    let z = zip_of(&[
        ("repo-main/README.md", b"repo readme"),
        ("repo-main/skills/brand-voice/SKILL.md", MD.as_bytes()),
        ("repo-main/skills/brand-voice/reference/tone.md", b"tone guide"),
        ("repo-main/skills/brand-voice/logo.png", &[0x89, 0x50, 0xff, 0xfe]),
        ("__MACOSX/repo-main/skills/brand-voice/._SKILL.md", b"junk"),
        ("repo-main/skills/other/SKILL.md", b"---\nname: other\ndescription: o\n---\n"),
    ]);
    let mut files = files_from_zip(&z, Some("skills/brand-voice")).unwrap();
    files.sort();
    assert_eq!(files.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), vec!["SKILL.md", "reference/tone.md"]);
    assert!(files_from_zip(&zip_of(&[("a.txt", b"x")]), None).unwrap_err().contains("SKILL.md"));
}

#[test]
fn install_list_read_disable_and_remove() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let s = install(&conn, vec![("SKILL.md".into(), MD.into()), ("reference/tone.md".into(), "tone guide".into())], "upload", "b@test").unwrap();
    assert_eq!((s.name.as_str(), s.enabled, s.files), ("brand-voice", true, 2));
    let top = read(&conn, "brand-voice", "SKILL.md").unwrap();
    assert!(top["instructions"].as_str().unwrap().starts_with("# Brand voice"));
    assert_eq!(top["files"], serde_json::json!(["reference/tone.md"]));
    assert_eq!(read(&conn, "brand-voice", "reference/tone.md").unwrap()["content"], "tone guide");
    assert!(read(&conn, "brand-voice", "../etc/passwd").is_err());
    assert_eq!(enabled(&conn), vec![("brand-voice".to_string(), "Write copy in the house style".to_string())]);

    // Reinstalling replaces the files.
    install(&conn, vec![("SKILL.md".into(), MD.into())], "upload", "b@test").unwrap();
    assert_eq!(list(&conn).unwrap()[0].files, 1);

    set_enabled(&conn, "brand-voice", false).unwrap();
    assert!(enabled(&conn).is_empty());
    assert!(read(&conn, "brand-voice", "SKILL.md").is_err(), "disabled skills are not readable by the chat");
    remove(&conn, "brand-voice").unwrap();
    assert!(list(&conn).unwrap().is_empty());
    assert!(remove(&conn, "brand-voice").is_err());
}

#[test]
fn github_links_resolve_to_a_zip_and_a_folder() {
    assert_eq!(resolve_url("https://github.com/acme/skills").unwrap(), ("https://codeload.github.com/acme/skills/zip/HEAD".into(), None));
    assert_eq!(
        resolve_url("https://github.com/acme/skills/tree/main/skills/pdf").unwrap(),
        ("https://codeload.github.com/acme/skills/zip/main".into(), Some("skills/pdf".into()))
    );
    assert!(resolve_url("http://example.com/s.zip").unwrap_err().contains("https"));
}

#[tokio::test]
async fn private_hosts_are_refused() {
    let err = download("https://127.0.0.1/skill.zip").await.unwrap_err();
    assert!(err.contains("内网"), "{err}");
}
