//! 端: the environment holds any number of them. The site at `/` is the first; every other one is a named folder
//! of site files (`teacher/…`, served under `/teacher/`) with a title and a rule for who may come in. The rule is
//! the kernel's: a page inside the folder never has to check for itself.
//!
//! The list lives in the drafted option `ends`, so making, changing and removing an 端 are drafts like everything
//! else, and the live site changes only on a publish.

use crate::auth::Actor;
use crate::changes::{self, Target};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The drafted option holding the list.
pub const KEY: &str = "ends";

/// Who may open an 端 when nothing else is said: anyone.
fn open_door() -> String {
    "public".into()
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct End {
    /// Its folder of site files and its address: `teacher` holds `teacher/…` and answers at `/teacher/`.
    pub name: String,
    pub title: String,
    /// Who may open its pages and call its functions: `public`, `members` (anyone logged in), or a condition on
    /// the member such as `member.kind == 'teacher'` (see `rules`).
    #[serde(default = "open_door")]
    pub enter: String,
}

/// What an 端 answers someone at its door.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Door {
    Open,
    /// A visitor: log in first.
    Login,
    /// A member the door's condition does not let in.
    Refused,
}

/// Top-level paths the server or the site's own pages already answer on.
const TAKEN: [&str; 6] = ["admin", "api", "uploads", "mcp", "account", "sitemap"];

/// The 端 of the live site, or of the drafts (`preview`).
// ponytail: read from the database on every site request, like the languages; keep them on the App behind the
// `ends` epoch if this ever shows up in a profile.
pub fn load(conn: &Connection, preview: bool) -> Vec<End> {
    serde_json::from_value(changes::setting(conn, KEY, preview)).unwrap_or_default()
}

/// The 端 a site path (without its leading slash) is in, when it is in one.
pub fn of<'a>(ends: &'a [End], rel: &str) -> Option<&'a End> {
    let first = rel.split('/').next().unwrap_or_default();
    ends.iter().find(|e| e.name == first)
}

/// Whether a live path is inside an 端 that is not open to everyone.
pub fn closed(ends: &[End], rel: &str) -> bool {
    of(ends, rel).is_some_and(|e| e.enter != "public")
}

/// What `member` (as `members::member_json` gives them; none: a visitor) meets at a live path. A condition that
/// cannot be judged keeps the door shut.
pub fn door(conn: &Connection, ends: &[End], rel: &str, member: Option<&Value>) -> Door {
    let Some(end) = of(ends, rel).filter(|e| e.enter != "public") else { return Door::Open };
    match (crate::rules::member_allows(conn, &end.enter, member), member) {
        (Ok(true), _) => Door::Open,
        (_, None) => Door::Login,
        (_, Some(_)) => Door::Refused,
    }
}

fn check_name(conn: &Connection, name: &str) -> Result<(), String> {
    if !crate::schema::valid_name(name) {
        return Err(format!("端的名字（name）“{name}”不合法：它也是网址里的一段，只能用小写字母开头，后接小写字母、数字或下划线，最长 40 个字符。"));
    }
    if TAKEN.contains(&name) {
        return Err(format!("端的名字（name）不能是“{name}”：这个网址已经另有用处。"));
    }
    if crate::langs::load(conn, true).is_some_and(|l| l.list.iter().any(|l| l.code == name)) {
        return Err(format!("端的名字（name）不能是“{name}”：它是网站的一种语言，/{name}/ 已经是那种语言的网址。"));
    }
    Ok(())
}

fn save(conn: &Connection, actor: &Actor, ends: &[End], note: &str) -> Result<(), String> {
    let target = Target::Setting(KEY.into());
    if !ends.is_empty() {
        changes::save_draft(conn, actor, &target, &json!(ends), note)
    } else if changes::target_status(conn, &target)? != "deleted" {
        changes::mark_deleted(conn, actor, &target, note)
    } else {
        Ok(())
    }
}

/// The site files of an 端 (`name` empty: the site itself, everything outside the others' folders), drafts
/// included: (how many there are, whether any has a change waiting to be published).
fn files(conn: &Connection, name: &str, others: &[End]) -> Result<(i64, bool), String> {
    let fail = |e: rusqlite::Error| format!("读取站点文件失败：{e}");
    let mut stmt = conn.prepare("SELECT path, draft IS NOT NULL OR draft_deleted, draft_deleted FROM site_files WHERE draft IS NOT NULL OR published IS NOT NULL").map_err(fail)?;
    let rows: Vec<(String, bool, bool)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
    let mine = |path: &str| match name {
        "" => of(others, path).is_none(),
        n => path.strip_prefix(n).is_some_and(|rest| rest.starts_with('/')),
    };
    let own: Vec<&(String, bool, bool)> = rows.iter().filter(|(p, _, _)| mine(p)).collect();
    Ok((own.iter().filter(|(_, _, deleting)| !deleting).count() as i64, own.iter().any(|(_, pending, _)| *pending)))
}

/// Every 端 with its address, who may enter, how far along it is and how many files it holds; the site first.
pub fn list(conn: &Connection, site_name: &str) -> Result<Value, String> {
    let (live, now) = (load(conn, false), load(conn, true));
    let mut out = vec![];
    let (n, pending) = files(conn, "", &now)?;
    out.push(json!({ "name": "", "title": site_name, "path": "/", "enter": "public", "status": "published", "files": n, "pending": pending }));
    for e in &now {
        let status = match live.iter().find(|l| l.name == e.name) {
            None => "draft",
            Some(l) if l != e => "changed",
            Some(_) => "published",
        };
        let (n, pending) = files(conn, &e.name, &now)?;
        out.push(json!({ "name": e.name, "title": e.title, "path": format!("/{}/", e.name), "enter": e.enter, "status": status, "files": n, "pending": pending || status != "published" }));
    }
    for e in live.iter().filter(|l| !now.iter().any(|e| e.name == l.name)) {
        let (n, _) = files(conn, &e.name, &live)?;
        out.push(json!({ "name": e.name, "title": e.title, "path": format!("/{}/", e.name), "enter": e.enter, "status": "deleting", "files": n, "pending": true }));
    }
    Ok(Value::Array(out))
}

/// Makes an 端 or changes one: a draft.
pub fn set(conn: &Connection, actor: &Actor, name: &str, title: Option<String>, enter: Option<String>, note: &str) -> Result<Value, String> {
    check_name(conn, name)?;
    let mut ends = load(conn, true);
    let title = title.map(|t| t.trim().to_string());
    if title.as_deref().is_some_and(|t| t.is_empty() || t.chars().count() > 40) {
        return Err("端的标题（title）要是 1 到 40 个字。".into());
    }
    let at = match ends.iter().position(|e| e.name == name) {
        Some(i) => i,
        None => {
            let title = title.clone().ok_or_else(|| format!("新建端“{name}”要给它一个标题（title），比如“老师端”。"))?;
            ends.push(End { name: name.into(), title, enter: open_door() });
            ends.len() - 1
        }
    };
    if let Some(t) = title {
        ends[at].title = t;
    }
    if let Some(e) = enter {
        let e = e.trim().to_string();
        crate::rules::check_member(&e).map_err(|why| format!("端的进门条件（enter）“{e}”写得不对：{why}"))?;
        ends[at].enter = e;
    }
    if ends[at].enter != "public" && !crate::members::login_enabled(conn, true) {
        return Err("不是所有人都能进的端要先有会员账号：先用 set_member_login 打开会员注册和登录。".into());
    }
    save(conn, actor, &ends, note)?;
    let all = list(conn, "")?;
    Ok(all.as_array().into_iter().flatten().find(|e| e["name"] == name).cloned().unwrap_or(Value::Null))
}

/// Removes an 端 and, with it, every file in its folder: drafts, gone once published. Taking only the door away
/// would leave a members' pages open to everyone.
pub fn delete(conn: &Connection, actor: &Actor, name: &str, note: &str) -> Result<Value, String> {
    let mut ends = load(conn, true);
    let Some(at) = ends.iter().position(|e| e.name == name) else {
        let known: Vec<&str> = ends.iter().map(|e| e.name.as_str()).collect();
        return Err(format!("没有叫“{name}”的端。现有的端：{}。", if known.is_empty() { "只有网站本身".to_string() } else { known.join("、") }));
    };
    ends.remove(at);
    let fail = |e: rusqlite::Error| format!("读取站点文件失败：{e}");
    let mut stmt = conn
        .prepare("SELECT path FROM site_files WHERE path LIKE ?1 ESCAPE '\\' AND draft_deleted = 0 AND (draft IS NOT NULL OR published IS NOT NULL) ORDER BY path")
        .map_err(fail)?;
    // `_` is a wildcard to LIKE and a legal character of a name.
    let like = format!("{}/%", name.replace('_', "\\_"));
    let paths: Vec<String> = stmt.query_map([like], |r| r.get(0)).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
    for p in &paths {
        changes::mark_deleted(conn, actor, &Target::File(p.clone()), note)?;
    }
    save(conn, actor, &ends, note)?;
    Ok(json!({ "name": name, "files": paths.len(), "status": "deleting" }))
}
