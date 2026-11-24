//! Skills: folders with a SKILL.md (front matter `name`, `description`) plus reference files, installed by a
//! builder and stored in the database. The chat sees each enabled skill's name and description and reads the
//! rest on demand. Nothing in a skill is ever executed: scripts are text the model may read.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;

pub const MAX_DOWNLOAD: usize = 20 * 1024 * 1024;
pub const MAX_SKILL_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_FILES: usize = 500;
pub const MAX_FILE: usize = 1024 * 1024;

fn ensure_tables(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS skills (name TEXT PRIMARY KEY, description TEXT NOT NULL, source TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1, installed_by TEXT NOT NULL, installed_at TEXT NOT NULL DEFAULT (datetime('now')));
         CREATE TABLE IF NOT EXISTS skill_files (skill TEXT NOT NULL REFERENCES skills(name) ON DELETE CASCADE, path TEXT NOT NULL,
            content TEXT NOT NULL, PRIMARY KEY (skill, path));",
    )
    .map_err(|e| format!("准备技能表失败：{e}"))
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub source: String,
    pub enabled: bool,
    pub installed_at: String,
    pub files: usize,
}

/// `name` and `description` from the YAML front matter at the top of SKILL.md, and the body after it.
pub fn parse_skill_md(text: &str) -> Result<(String, String, String), String> {
    let text = text.trim_start_matches('\u{feff}');
    let rest = text.strip_prefix("---").ok_or("SKILL.md 开头要有 --- 包起来的说明（name 和 description）。")?;
    let end = rest.find("\n---").ok_or("SKILL.md 开头的说明没有用 --- 结束。")?;
    let (head, body) = (&rest[..end], &rest[end + 4..]);
    let field = |key: &str| {
        head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == key).then(|| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        })
    };
    let name = field("name").filter(|n| !n.is_empty()).ok_or("SKILL.md 的说明里缺少 name。")?;
    if name.len() > 64 || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(format!("技能名“{name}”不合规：只能用小写字母、数字和 -，最多 64 个字符。"));
    }
    let description = field("description").filter(|d| !d.is_empty()).ok_or("SKILL.md 的说明里缺少 description。")?;
    Ok((name, description.chars().take(1024).collect(), body.trim_start_matches(['\r', '\n']).to_string()))
}

/// Text files of one skill from a zip: the folder holding SKILL.md (the shallowest one, or the one under `subdir`).
pub fn files_from_zip(bytes: &[u8], subdir: Option<&str>) -> Result<Vec<(String, String)>, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("压缩包打不开：{e}"))?;
    let names: Vec<String> = (0..zip.len()).filter_map(|i| zip.by_index_raw(i).ok().and_then(|f| f.name().ok().map(|n| n.into_owned()))).collect();
    let root = names
        .iter()
        .filter(|n| n.ends_with("SKILL.md") && (n.as_str() == "SKILL.md" || n.ends_with("/SKILL.md")))
        .map(|n| n.trim_end_matches("SKILL.md").to_string())
        .filter(|r| subdir.is_none_or(|s| r.trim_end_matches('/').ends_with(s.trim_matches('/'))))
        .min_by_key(|r| r.matches('/').count())
        .ok_or("压缩包里没有找到 SKILL.md。")?;
    let mut out = vec![];
    let mut total = 0usize;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| format!("压缩包损坏：{e}"))?;
        let name = f.name().map_err(|e| format!("压缩包损坏：{e}"))?.into_owned();
        let Some(rel) = name.strip_prefix(root.as_str()).map(str::to_string) else { continue };
        if f.is_dir() || rel.is_empty() || rel.contains("..") || rel.starts_with('/') || rel.contains("__MACOSX") || rel.ends_with(".DS_Store") {
            continue;
        }
        if f.size() as usize > MAX_FILE {
            return Err(format!("技能里的“{rel}”超过 1 MB。"));
        }
        let mut buf = Vec::new();
        f.by_ref().take(MAX_FILE as u64 + 1).read_to_end(&mut buf).map_err(|e| format!("读取“{rel}”失败：{e}"))?;
        total += buf.len();
        if buf.len() > MAX_FILE || total > MAX_SKILL_BYTES {
            return Err("技能太大：解压后最多 10 MB。".into());
        }
        // Binary files (images, fonts) are skipped: the model can only use text.
        if let Ok(text) = String::from_utf8(buf) {
            out.push((rel, text));
        }
        if out.len() > MAX_FILES {
            return Err(format!("技能里的文件太多：最多 {MAX_FILES} 个。"));
        }
    }
    Ok(out)
}

/// Saves a skill from its files, replacing any skill with the same name.
pub fn install(conn: &Connection, files: Vec<(String, String)>, source: &str, by: &str) -> Result<Skill, String> {
    ensure_tables(conn)?;
    let md = files.iter().find(|(p, _)| p == "SKILL.md").ok_or("技能里没有 SKILL.md。")?;
    let (name, description, _) = parse_skill_md(&md.1)?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM skill_files WHERE skill = ?1", [&name]).map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO skills (name, description, source, installed_by) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(name) DO UPDATE SET description = ?2, source = ?3, installed_by = ?4, installed_at = datetime('now')",
        params![name, description, source, by],
    )
    .map_err(|e| format!("保存技能失败：{e}"))?;
    for (path, content) in &files {
        tx.execute("INSERT INTO skill_files (skill, path, content) VALUES (?1, ?2, ?3)", params![name, path, content]).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    list(conn)?.into_iter().find(|s| s.name == name).ok_or_else(|| "技能保存后没找到。".into())
}

pub fn list(conn: &Connection) -> Result<Vec<Skill>, String> {
    ensure_tables(conn)?;
    let mut st = conn
        .prepare(
            "SELECT s.name, s.description, s.source, s.enabled, s.installed_at, (SELECT count(*) FROM skill_files f WHERE f.skill = s.name)
             FROM skills s ORDER BY s.name",
        )
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| {
            Ok(Skill { name: r.get(0)?, description: r.get(1)?, source: r.get(2)?, enabled: r.get(3)?, installed_at: r.get(4)?, files: r.get::<_, i64>(5)? as usize })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| format!("读取技能失败：{e}"))
}

pub fn set_enabled(conn: &Connection, name: &str, enabled: bool) -> Result<(), String> {
    ensure_tables(conn)?;
    match conn.execute("UPDATE skills SET enabled = ?2 WHERE name = ?1", params![name, enabled]).map_err(|e| e.to_string())? {
        0 => Err(format!("没有名为“{name}”的技能。")),
        _ => Ok(()),
    }
}

pub fn remove(conn: &Connection, name: &str) -> Result<(), String> {
    ensure_tables(conn)?;
    conn.execute("DELETE FROM skill_files WHERE skill = ?1", [name]).map_err(|e| e.to_string())?;
    match conn.execute("DELETE FROM skills WHERE name = ?1", [name]).map_err(|e| e.to_string())? {
        0 => Err(format!("没有名为“{name}”的技能。")),
        _ => Ok(()),
    }
}

/// One file of an enabled skill. `SKILL.md` returns the instructions without the front matter, plus the file list.
pub fn read(conn: &Connection, name: &str, path: &str) -> Result<Value, String> {
    ensure_tables(conn)?;
    let enabled: Option<bool> = conn.query_row("SELECT enabled FROM skills WHERE name = ?1", [name], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
    if enabled != Some(true) {
        return Err(format!("没有启用名为“{name}”的技能。"));
    }
    let content: Option<String> = conn
        .query_row("SELECT content FROM skill_files WHERE skill = ?1 AND path = ?2", [name, path], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    let content = content.ok_or_else(|| format!("技能“{name}”里没有“{path}”。"))?;
    if path == "SKILL.md" {
        let (_, _, body) = parse_skill_md(&content)?;
        let mut st = conn.prepare("SELECT path FROM skill_files WHERE skill = ?1 AND path != 'SKILL.md' ORDER BY path").map_err(|e| e.to_string())?;
        let others: Vec<String> = st.query_map([name], |r| r.get(0)).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
        return Ok(json!({"name": name, "instructions": body, "files": others}));
    }
    Ok(json!({"name": name, "path": path, "content": content}))
}

/// Enabled skills for the chat's system prompt: (name, description).
pub fn enabled(conn: &Connection) -> Vec<(String, String)> {
    list(conn).unwrap_or_default().into_iter().filter(|s| s.enabled).map(|s| (s.name, s.description)).collect()
}

// ---------- Download ----------

pub(crate) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_private() || v.is_loopback() || v.is_link_local() || v.is_broadcast() || v.is_multicast() || v.is_documentation()
                || o[0] == 0 || (o[0] == 100 && (64..128).contains(&o[1])) || (o[0] == 198 && (18..20).contains(&o[1])) || o[0] >= 240)
        }
        IpAddr::V6(v) => match v.to_ipv4_mapped() {
            Some(v4) => public_ip(IpAddr::V4(v4)),
            None => !(v.is_loopback() || v.is_unspecified() || v.is_multicast() || (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80),
        },
    }
}

/// Every address `host` resolves to is public (or private hosts are allowed for testing).
fn host_is_public(host: &str, port: u16) -> bool {
    if std::env::var("HC_SKILLS_ALLOW_PRIVATE").as_deref() == Ok("1") {
        return true;
    }
    use std::net::ToSocketAddrs;
    (host, port).to_socket_addrs().is_ok_and(|mut a| a.all(|s| public_ip(s.ip())))
}

/// Where to fetch a skill from: a GitHub folder link becomes the repository's zip plus the folder inside it.
pub fn resolve_url(url: &str) -> Result<(String, Option<String>), String> {
    let url = url.trim();
    if !url.starts_with("https://") {
        return Err("技能地址要以 https:// 开头。".into());
    }
    if let Some(rest) = url.strip_prefix("https://github.com/") {
        let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
        match parts.as_slice() {
            [owner, repo] => return Ok((format!("https://codeload.github.com/{owner}/{repo}/zip/HEAD"), None)),
            [owner, repo, "tree", branch, sub @ ..] => {
                return Ok((format!("https://codeload.github.com/{owner}/{repo}/zip/{branch}"), (!sub.is_empty()).then(|| sub.join("/"))))
            }
            _ => return Err("请给出 GitHub 仓库或其中某个文件夹的地址（…/tree/分支/路径）。".into()),
        }
    }
    Ok((url.to_string(), None))
}

/// Downloads a skill: a zip, a GitHub folder, or a single SKILL.md. Only public https hosts.
pub async fn download(url: &str) -> Result<Vec<(String, String)>, String> {
    let (fetch, subdir) = resolve_url(url)?;
    let parsed = reqwest::Url::parse(&fetch).map_err(|_| "技能地址不正确。".to_string())?;
    let host = parsed.host_str().ok_or("技能地址没有主机名。")?.to_string();
    let addrs = tokio::net::lookup_host((host.as_str(), 443)).await.map_err(|e| format!("找不到“{host}”：{e}"))?;
    // ponytail: checks the first resolution only; DNS rebinding between this and the request is not covered.
    if addrs.into_iter().any(|a| !public_ip(a.ip())) && std::env::var("HC_SKILLS_ALLOW_PRIVATE").as_deref() != Ok("1") {
        return Err("不能从内网地址下载技能。".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        // Each hop is checked like the first: a public page must not redirect into the private network.
        .redirect(reqwest::redirect::Policy::custom(|a| {
            let ok = a.url().scheme() == "https" && a.previous().len() < 5 && a.url().host_str().is_some_and(|h| host_is_public(h, a.url().port_or_known_default().unwrap_or(443)));
            if ok { a.follow() } else { a.stop() }
        }))
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client.get(parsed).send().await.map_err(|e| format!("下载技能失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载技能失败：对方返回 {}。", resp.status().as_u16()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("下载中断：{e}"))? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > MAX_DOWNLOAD {
            return Err("技能太大：下载最多 20 MB。".into());
        }
    }
    if bytes.starts_with(b"PK") {
        return files_from_zip(&bytes, subdir.as_deref());
    }
    let text = String::from_utf8(bytes).map_err(|_| "下载的内容既不是压缩包也不是 SKILL.md。".to_string())?;
    parse_skill_md(&text)?;
    Ok(vec![("SKILL.md".into(), text)])
}
