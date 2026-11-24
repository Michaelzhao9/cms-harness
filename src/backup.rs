//! The whole site as one zip: `harness.db` (a consistent copy made with `VACUUM INTO`), every file under
//! `uploads/`, and `backup.json` saying when and from what. Restoring checks the archive first, keeps a backup
//! of what it replaces under `<data>/backups/`, then swaps the database in.

use rusqlite::Connection;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

/// The largest archive a restore reads, and the most it may unpack to.
pub const MAX_ARCHIVE: usize = 1 << 30;
const MAX_UNPACKED: u64 = 8 << 30;

/// Writes the backup of `conn` and `data_dir/uploads` to `out`.
pub fn export(conn: &Connection, data_dir: &Path, out: impl Write + Seek) -> Result<(), String> {
    let tmp = data_dir.join(format!(".backup-{}.db", rand_suffix()));
    let _ = std::fs::remove_file(&tmp);
    conn.execute("VACUUM INTO ?1", [tmp.to_string_lossy()]).map_err(|e| format!("复制数据库失败：{e}"))?;
    let result = write_zip(&tmp, &data_dir.join("uploads"), out);
    let _ = std::fs::remove_file(&tmp);
    result
}

fn write_zip(db: &Path, uploads: &Path, out: impl Write + Seek) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("写备份失败：{e}");
    let mut zip = zip::ZipWriter::new(out);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true);
    let manifest = serde_json::json!({ "format": 1, "app": "cms-harness", "version": env!("CARGO_PKG_VERSION"),
        "created_at": chrono_now() });
    zip.start_file("backup.json", opts).map_err(|e| fail(&e))?;
    zip.write_all(serde_json::to_string_pretty(&manifest).unwrap_or_default().as_bytes()).map_err(|e| fail(&e))?;
    zip.start_file("harness.db", opts).map_err(|e| fail(&e))?;
    std::io::copy(&mut std::fs::File::open(db).map_err(|e| fail(&e))?, &mut zip).map_err(|e| fail(&e))?;
    // Uploads are already compressed images and PDFs: stored as they are.
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).large_file(true);
    if let Ok(entries) = std::fs::read_dir(uploads) {
        let mut names: Vec<String> = entries.filter_map(|e| e.ok()?.file_name().into_string().ok()).filter(|n| upload_name_ok(n)).collect();
        names.sort();
        for name in names {
            zip.start_file(format!("uploads/{name}"), stored).map_err(|e| fail(&e))?;
            std::io::copy(&mut std::fs::File::open(uploads.join(&name)).map_err(|e| fail(&e))?, &mut zip).map_err(|e| fail(&e))?;
        }
    }
    zip.finish().map_err(|e| fail(&e))?;
    Ok(())
}

/// Names the media store gives files: hex, a dot, a short extension. Anything else is not ours.
fn upload_name_ok(name: &str) -> bool {
    let Some((stem, ext)) = name.split_once('.') else { return false };
    !stem.is_empty() && stem.len() <= 64 && stem.bytes().all(|b| b.is_ascii_hexdigit()) && (1..=5).contains(&ext.len()) && ext.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn rand_suffix() -> String {
    format!("{:016x}", rand::random::<u64>())
}

/// Now as `YYYY-MM-DDTHH:MM:SSZ`, from SQLite so no date crate is needed.
fn chrono_now() -> String {
    Connection::open_in_memory()
        .and_then(|c| c.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')", [], |r| r.get(0)))
        .unwrap_or_default()
}

/// A checked archive, unpacked next to the database: the database to swap in, and the uploads it brings.
pub struct Unpacked {
    db: PathBuf,
}

/// Checks `bytes` and unpacks it: the database to a temporary file beside `db_path` (opened once, so it is a
/// working CMS Harness database with at least one builder), uploads into `data_dir/uploads` (they are named by
/// content, so existing files are never overwritten with something else).
pub fn unpack(bytes: &[u8], db_path: &Path, data_dir: &Path) -> Result<Unpacked, String> {
    let bad = |e: &dyn std::fmt::Display| format!("备份文件读不出来：{e}。请选择 CMS Harness 导出的 zip 备份。");
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| bad(&e))?;
    let mut total = 0u64;
    let mut has_db = false;
    let mut upload_names: Vec<String> = vec![];
    for i in 0..zip.len() {
        let f = zip.by_index_raw(i).map_err(|e| bad(&e))?;
        total = total.saturating_add(f.size());
        let name = f.name().map_err(|e| bad(&e))?.to_string();
        let name = name.as_str();
        let ok = name == "harness.db" || name == "backup.json" || name == "uploads/" || name.strip_prefix("uploads/").is_some_and(upload_name_ok);
        if !ok {
            return Err(format!("备份里有不认识的文件“{name}”：这不是 CMS Harness 导出的备份。"));
        }
        has_db |= name == "harness.db";
        if let Some(u) = name.strip_prefix("uploads/").filter(|u| upload_name_ok(u)) {
            upload_names.push(u.to_string());
        }
    }
    if !has_db {
        return Err("备份里没有 harness.db：这不是 CMS Harness 导出的备份。".into());
    }
    if total > MAX_UNPACKED {
        return Err("备份解压后太大：最多 8 GB。".into());
    }
    let tmp = db_path.with_extension(format!("restore-{}.db", rand_suffix()));
    let write = |zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str, to: &Path| -> Result<(), String> {
        let mut f = zip.by_name(name).map_err(|e| bad(&e))?;
        let mut out = std::fs::File::create(to).map_err(|e| format!("写入“{}”失败：{e}", to.display()))?;
        // `take` bounds what a lying header could unpack to.
        std::io::copy(&mut (&mut f).take(MAX_UNPACKED), &mut out).map_err(|e| bad(&e))?;
        Ok(())
    };
    write(&mut zip, "harness.db", &tmp)?;
    let checked = (|| -> Result<(), String> {
        let conn = crate::store::open(&tmp.to_string_lossy()).map_err(|e| format!("备份里的数据库打不开：{e}"))?;
        let builders: i64 = conn
            .query_row("SELECT count(*) FROM admins WHERE role = 'builder'", [], |r| r.get(0))
            .map_err(|e| format!("备份里的数据库不完整：{e}"))?;
        if builders == 0 {
            return Err("备份里没有搭建者账号，恢复后就没人能管理后台了：没有恢复。".into());
        }
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(e) = checked {
        remove_db(&tmp);
        return Err(e);
    }
    let uploads = data_dir.join("uploads");
    std::fs::create_dir_all(&uploads).map_err(|e| format!("创建上传目录失败：{e}"))?;
    for name in upload_names {
        let to = uploads.join(&name);
        if !to.exists() {
            write(&mut zip, &format!("uploads/{name}"), &to)?;
        }
    }
    Ok(Unpacked { db: tmp })
}

fn remove_db(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

/// Puts the unpacked database at `db_path`. The caller makes sure no connection to `db_path` is open.
/// The rename replaces the old file in one step, so a failure leaves the old database where it was.
pub fn swap_in(unpacked: Unpacked, db_path: &Path) -> Result<(), String> {
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db_path.display()));
    }
    let moved = std::fs::rename(&unpacked.db, db_path).map_err(|e| format!("换上备份的数据库失败：{e}"));
    remove_db(&unpacked.db);
    moved
}

/// Where the automatic backup before a restore goes: `<data>/backups/before-restore-<time>.zip`.
pub fn safety_copy(conn: &Connection, data_dir: &Path) -> Result<PathBuf, String> {
    let dir = data_dir.join("backups");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建备份目录失败：{e}"))?;
    let path = dir.join(format!("before-restore-{}.zip", chrono_now().replace(':', "-")));
    let file = std::fs::File::create(&path).map_err(|e| format!("写备份失败：{e}"))?;
    export(conn, data_dir, file)?;
    Ok(path)
}

/// Restores a running server: unpacks and checks the archive, saves a backup of the current site, then swaps the
/// database under the connection lock and reopens it. Returns where the backup of the old site went.
pub fn restore_live(app: &crate::App, bytes: &[u8]) -> Result<PathBuf, String> {
    let db_path = {
        let conn = app.conn();
        PathBuf::from(conn.path().filter(|p| !p.is_empty()).ok_or("这个数据库在内存里，不能恢复。")?)
    };
    let unpacked = unpack(bytes, &db_path, &app.data_dir)?;
    let mut conn = app.conn();
    let saved = match safety_copy(&conn, &app.data_dir) {
        Ok(p) => p,
        Err(e) => {
            remove_db(&unpacked.db);
            return Err(e);
        }
    };
    // Close the old connection before its files are replaced.
    *conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
    // Whatever happened, a database is open again: the backup's, or the old one if the swap failed.
    let swapped = swap_in(unpacked, &db_path);
    match crate::store::open(&db_path.to_string_lossy()) {
        Ok(c) => *conn = c,
        Err(e) => return Err(format!("打开恢复后的数据库失败：{e}。恢复前的整站备份在 {}。", saved.display())),
    }
    swapped?;
    app.cache.clear();
    crate::changes::sync_all_indexes(&conn)?;
    if let Some(settings) = crate::llm::effective(&conn) {
        app.set_model(settings.client());
    }
    Ok(saved)
}
