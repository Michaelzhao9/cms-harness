//! A collection as a spreadsheet: out as CSV, and rows in as drafts. Managing data by hand, in bulk, with the tool
//! people already have.

use super::content::{self, Out};
use super::session::AdminUser;
use crate::schema::{CollectionDef, FieldDef, FieldType};
use crate::{App, Shared};
use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use serde_json::{json, Map, Value};

/// Rows one import may bring.
const MAX_ROWS: usize = 2000;
const IMPORT_NOTE: &str = "后台导入 CSV";
/// What separates the values of a field that holds several.
const MANY: char = '|';

/// A value as one cell. Text a spreadsheet would run as a formula gets a leading apostrophe: records hold what
/// visitors typed.
fn cell(v: &Value) -> String {
    let defuse = |s: &str| if s.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{s}") } else { s.to_string() };
    let one = |v: &Value| match v {
        Value::Null => String::new(),
        Value::String(s) => defuse(s),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    };
    match v {
        Value::Array(a) => a.iter().map(one).collect::<Vec<_>>().join(&MANY.to_string()),
        v => one(v),
    }
}

fn quoted(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) || s.starts_with(' ') || s.ends_with(' ') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Every record of the collection as the admin sees it (drafts included), oldest first.
fn csv_of(app: &App, user: &AdminUser, def: &CollectionDef) -> Result<String, String> {
    let mut out = String::from("\u{feff}");
    let head: Vec<String> = ["id", "status"].iter().map(|s| s.to_string()).chain(def.fields.iter().map(|f| quoted(&f.name))).collect();
    out.push_str(&head.join(","));
    out.push_str("\r\n");
    let mut offset = 0;
    loop {
        let page = content::tool(app, user, "query_records", json!({ "collection": def.name, "limit": 100, "offset": offset, "sort": "id" }))?;
        let items = page["items"].as_array().cloned().unwrap_or_default();
        for r in &items {
            let mut row = vec![r["id"].to_string(), r["status"].as_str().unwrap_or_default().to_string()];
            row.extend(def.fields.iter().map(|f| quoted(&cell(&r["data"][&f.name]))));
            out.push_str(&row.join(","));
            out.push_str("\r\n");
        }
        offset += items.len() as i64;
        if items.len() < 100 || offset >= page["total"].as_i64().unwrap_or(0) {
            return Ok(out);
        }
    }
}

/// `GET /admin/c/<name>/export.csv`
pub async fn export(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Path(name): Path<String>) -> Response {
    let done = tokio::task::spawn_blocking(move || {
        let def = content::collection(&app, &name)?;
        csv_of(&app, &user, &def).map(|csv| (def.name, csv)).map_err(content::internal)
    })
    .await;
    match done {
        Ok(Ok((name, csv))) => {
            let headers = [
                (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}.csv\"")),
                (header::CACHE_CONTROL, "no-store".to_string()),
            ];
            (headers, csv).into_response()
        }
        Ok(Err(out)) => out.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("服务器内部错误：{e}")).into_response(),
    }
}

/// CSV as rows of cells: quoted cells may hold commas, quotes (doubled) and line breaks. Empty lines are skipped.
fn parse(text: &str) -> Vec<Vec<String>> {
    let (mut rows, mut row, mut cur, mut quoted, mut any) = (vec![], vec![], String::new(), false, false);
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => {
                quoted = !quoted;
                any = true;
            }
            ',' if !quoted => {
                row.push(std::mem::take(&mut cur));
                any = true;
            }
            '\r' if !quoted => {}
            '\n' if !quoted => {
                if any || !cur.is_empty() {
                    row.push(std::mem::take(&mut cur));
                    rows.push(std::mem::take(&mut row));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        row.push(cur);
        rows.push(row);
    }
    rows
}

/// A cell as the value its field takes. What does not fit is passed on as text, for the record check to name.
fn typed(f: &FieldDef, raw: &str) -> Value {
    let one = |s: &str| -> Value {
        let s = s.trim();
        match f.kind {
            FieldType::Number | FieldType::Relation => s.parse::<i64>().map(Value::from).or_else(|_| s.parse::<f64>().map(Value::from)).unwrap_or_else(|_| json!(s)),
            FieldType::Boolean => match s.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "是" => json!(true),
                "false" | "0" | "no" | "否" => json!(false),
                _ => json!(s),
            },
            _ => json!(s.strip_prefix('\'').filter(|rest| rest.starts_with(['=', '+', '-', '@'])).unwrap_or(s)),
        }
    };
    if f.multiple {
        Value::Array(raw.split(MANY).filter(|s| !s.trim().is_empty()).map(one).collect())
    } else if f.kind == FieldType::Text || f.kind == FieldType::Richtext {
        // Text keeps its own spaces and line breaks.
        json!(raw.strip_prefix('\'').filter(|rest| rest.starts_with(['=', '+', '-', '@'])).unwrap_or(raw))
    } else {
        one(raw)
    }
}

/// The records a CSV holds for `def`, or every problem found, each with its row in the file.
fn rows_of(def: &CollectionDef, text: &str) -> Result<Vec<Value>, Vec<String>> {
    let mut rows = parse(text).into_iter();
    let Some(head) = rows.next() else { return Err(vec!["文件是空的：第一行要是列名（字段名或字段的标题）。".into()]) };
    let mut columns: Vec<Option<&FieldDef>> = vec![];
    let mut problems = vec![];
    for h in &head {
        let h = h.trim();
        match def.fields.iter().find(|f| f.name == h || f.title == h) {
            Some(f) => columns.push(Some(f)),
            // What an export adds: a new record gets its own id and starts as a draft.
            None if h == "id" || h == "status" || h.is_empty() => columns.push(None),
            None => problems.push(format!("第 1 行：集合“{}”没有叫“{h}”的字段。可用的列名：{}。", def.title, def.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>().join("、"))),
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let mut out = vec![];
    for (i, row) in rows.enumerate() {
        let line = i + 2;
        if out.len() >= MAX_ROWS {
            return Err(vec![format!("一次最多导入 {MAX_ROWS} 行：把文件分成几份。")]);
        }
        let mut data = Map::new();
        for (col, raw) in columns.iter().zip(&row) {
            if let (Some(f), false) = (col, raw.trim().is_empty()) {
                data.insert(f.name.clone(), typed(f, raw));
            }
        }
        let data = Value::Object(data);
        match crate::schema::normalize(def, &json!({}), &data) {
            Ok(_) => out.push(data),
            Err(e) => problems.push(format!("第 {line} 行：{e}")),
        }
    }
    if problems.is_empty() { Ok(out) } else { Err(problems) }
}

/// `POST /admin/c/<name>/import`: a CSV file's rows become draft records. Every row is checked first: one that
/// does not fit and nothing comes in.
pub async fn import(State(app): State<Shared>, Extension(user): Extension<AdminUser>, Path(name): Path<String>, req: Request) -> Response {
    let posted = match content::checked(&user, req).await {
        Ok(p) => p,
        Err(o) => return o.into_response(),
    };
    content::blocking(move || {
        let refuse = |lines: Vec<String>| Err(Out::Fail(StatusCode::BAD_REQUEST, format!("没有导入任何内容。\n{}", lines.into_iter().take(20).collect::<Vec<_>>().join("\n"))));
        let def = content::collection(&app, &name)?;
        let Some((_, _, bytes)) = posted.files.iter().find(|(input, _, _)| input == "file") else { return refuse(vec!["请选择一个 CSV 文件。".into()]) };
        let Ok(text) = std::str::from_utf8(bytes) else { return refuse(vec!["文件不是 UTF-8 编码：在表格软件里另存为“CSV UTF-8”。".into()]) };
        let rows = match rows_of(&def, text) {
            Ok(rows) if rows.is_empty() => return refuse(vec!["文件里只有列名，没有数据行。".into()]),
            Ok(rows) => rows,
            Err(problems) => return refuse(problems),
        };
        for (i, data) in rows.iter().enumerate() {
            // What only the database can tell (a value that must be unique, a relation that leads nowhere).
            if let Err(e) = content::tool(&app, &user, "create_record", json!({ "collection": def.name, "data": data, "note": IMPORT_NOTE })) {
                return Err(Out::Fail(StatusCode::BAD_REQUEST, format!("已导入前 {i} 行（都是草稿），第 {} 行没有导入，后面的也没有：{e}", i + 2)));
            }
        }
        Ok(Out::Go(format!("/admin/c/{}?imported={}", def.name, rows.len())))
    })
    .await
}
