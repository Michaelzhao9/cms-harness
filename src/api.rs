//! HTTP routes. One small function per route; database work runs on the blocking pool.

use crate::auth::{actor_from_key, Scope};
use crate::schema::project;
use crate::{changes, media, query, Shared};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{routing::{get, post}, Json, Router};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

type Params = Query<Vec<(String, String)>>;
type Fail = (StatusCode, String);

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/api/v1/content/{collection}", get(list))
        .route("/api/v1/content/{collection}/{id}", get(one))
        .route("/api/v1/media", post(upload).layer(DefaultBodyLimit::max(21 * 1024 * 1024)))
        .route("/uploads/{*path}", get(serve))
        .with_state(app)
}

/// `{ "ok": false, "error": … }` with the given status.
pub fn error_response(status: StatusCode, msg: String) -> Response {
    (status, Json(json!({ "ok": false, "error": msg }))).into_response()
}

/// The key from an `Authorization: Bearer <key>` header.
pub fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    v.strip_prefix("Bearer ").map(|k| k.trim().to_string())
}

fn db(e: impl std::fmt::Display) -> Fail {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("读取数据库失败：{e}"))
}

async fn list(State(app): State<Shared>, Path(c): Path<String>, Query(p): Params, h: HeaderMap) -> Response {
    read(app, c, None, p, bearer(&h)).await
}

async fn one(State(app): State<Shared>, Path((c, id)): Path<(String, String)>, Query(p): Params, h: HeaderMap) -> Response {
    match id.parse() {
        Ok(id) => read(app, c, Some(id), p, bearer(&h)).await,
        Err(_) => error_response(StatusCode::NOT_FOUND, format!("记录“{id}”不存在：记录编号是整数。")),
    }
}

async fn read(app: Shared, collection: String, id: Option<i64>, params: Vec<(String, String)>, key: Option<String>) -> Response {
    let done = tokio::task::spawn_blocking(move || {
        let conn = app.conn();
        read_blocking(&conn, &collection, id, &params, key.as_deref())
    })
    .await;
    let (public, result) = match done {
        Ok(r) => r,
        Err(e) => (false, Err(db(e))),
    };
    let mut resp = match result {
        Ok(v) => Json(v).into_response(),
        Err((s, m)) => error_response(s, m),
    };
    if public {
        resp.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    }
    resp
}

/// (collection is public, result)
fn read_blocking(conn: &Connection, name: &str, id: Option<i64>, params: &[(String, String)], key: Option<&str>) -> (bool, Result<Value, Fail>) {
    let def = match changes::live_def(conn, name) {
        Ok(d) => d,
        Err(e) => return (false, Err((StatusCode::NOT_FOUND, e))),
    };
    let public = def.access.public_read;
    let authed = key.is_some_and(|k| actor_from_key(conn, k).is_some());
    if !public && !authed {
        return (false, Err((StatusCode::UNAUTHORIZED, format!("集合“{name}”不是公开的：请在 Authorization 头里带上有效的 Bearer 密钥。"))));
    }
    let run = || -> Result<Value, Fail> {
        let (mut q, expand) = query::parse_params(&def, params).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        if let Some(id) = id {
            q.filters.push(("id".into(), query::Op::Eq, json!(id)));
        } else if def.single {
            q.limit = 1;
        }
        let (rows, total) = query::run(conn, &def, &q, true).map_err(db)?;
        let mut items = vec![];
        for r in rows {
            let mut item = r.doc;
            item["id"] = json!(r.id);
            expand_item(conn, &def, &mut item, &expand, authed).map_err(db)?;
            items.push(item);
        }
        let not_found = |m: String| (StatusCode::NOT_FOUND, m);
        if id.is_some() {
            items.pop().ok_or_else(|| not_found(format!("集合“{name}”里没有已发布的记录 {}。", id.unwrap_or_default())))
        } else if def.single {
            items.pop().ok_or_else(|| not_found(format!("单例集合“{name}”还没有已发布的记录。")))
        } else {
            Ok(json!({ "items": items, "total": total }))
        }
    };
    (public, run())
}

/// Replaces relation values with the target's live record, one level deep.
/// Targets in a private collection are only expanded for a request that carries a valid key.
fn expand_item(conn: &Connection, def: &crate::schema::CollectionDef, item: &mut Value, expand: &[String], authed: bool) -> Result<(), rusqlite::Error> {
    for name in expand {
        let Some(target) = def.field(name).and_then(|f| f.target.as_deref()) else { continue };
        let Ok(tdef) = changes::live_def(conn, target) else { continue };
        if !tdef.access.public_read && !authed {
            continue;
        }
        let mut fetch = |v: &Value| -> Result<Value, rusqlite::Error> {
            let Some(id) = v.as_i64() else { return Ok(v.clone()) };
            let doc: Option<String> = conn
                .query_row("SELECT published FROM records WHERE id = ?1 AND collection = ?2 AND published IS NOT NULL", rusqlite::params![id, target], |r| r.get(0))
                .optional()?;
            let Some(doc) = doc.and_then(|d| serde_json::from_str::<Value>(&d).ok()) else { return Ok(v.clone()) };
            let mut out = project(&tdef, &doc);
            out["id"] = json!(id);
            Ok(out)
        };
        item[name] = match &item[name] {
            Value::Array(a) => Value::Array(a.iter().map(&mut fetch).collect::<Result<_, _>>()?),
            v => fetch(v)?,
        };
    }
    Ok(())
}

async fn serve(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    let found = tokio::task::spawn_blocking(move || media::read(&app, &name)).await.ok().flatten();
    let Some((bytes, content_type)) = found else {
        return error_response(StatusCode::NOT_FOUND, "文件不存在：请检查 /uploads/ 后面的文件名。".into());
    };
    let mut resp = bytes.into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if content_type == "application/pdf" {
        h.insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment"));
    }
    resp
}

/// `POST /api/v1/media`: one multipart field named `file`.
async fn upload(State(app): State<Shared>, headers: HeaderMap, mut form: Multipart) -> Response {
    let key = bearer(&headers);
    let a = app.clone();
    let authed = tokio::task::spawn_blocking(move || key.and_then(|k| actor_from_key(&a.conn(), &k))).await.ok().flatten();
    let Some(actor) = authed else {
        return error_response(StatusCode::UNAUTHORIZED, "上传需要密钥：请在 Authorization 头里带上有效的 Bearer 密钥。".into());
    };
    if !actor.can(Scope::Write) {
        return error_response(StatusCode::FORBIDDEN, format!("“{}”没有权限上传：需要 write 权限。", actor.name));
    }
    let bad = |e: axum::extract::multipart::MultipartError| error_response(e.status(), format!("读取上传内容失败：{}。", e.body_text()));
    let mut file: Option<(String, Vec<u8>)> = None;
    loop {
        let field = match form.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return bad(e),
        };
        if field.name() != Some("file") {
            continue;
        }
        if file.is_some() {
            return error_response(StatusCode::BAD_REQUEST, "一次只能上传一个文件：请只带一个名为 file 的字段。".into());
        }
        let name = field.file_name().unwrap_or("upload").to_string();
        match field.bytes().await {
            Ok(b) => file = Some((name, b.to_vec())),
            Err(e) => return bad(e),
        }
    }
    let Some((name, bytes)) = file else {
        return error_response(StatusCode::BAD_REQUEST, "没有收到文件：请用 multipart 表单，把文件放在名为 file 的字段里。".into());
    };
    match tokio::task::spawn_blocking(move || media::store_upload(&app, &name, &bytes)).await {
        Ok(Ok(path)) => Json(json!({ "ok": true, "result": { "path": path } })).into_response(),
        Ok(Err(e)) => error_response(StatusCode::BAD_REQUEST, e),
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("保存文件失败：{e}")),
    }
}
