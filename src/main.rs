//! harness-cms: one binary, one SQLite file.
//!
//!   harness-cms                                         serve on 127.0.0.1
//!   harness-cms admin <email> [builder|editor]          create an admin or reset its password
//!   harness-cms key <name> [write] [structure] [publish] print a new API key once
//!
//! Settings come from the environment: HC_PORT (4200), HC_DATA (data), HC_DB (<HC_DATA>/harness.db),
//! HC_ORIGIN (http://127.0.0.1:<port>), HC_SITE_NAME (harness-cms), HC_ADMIN_PASSWORD (for `admin`),
//! HC_TRUST_PROXY (set to 1 behind a reverse proxy so login rate limits use X-Forwarded-For).

use harness_cms::{api, auth, changes, store, App};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "用法：
  harness-cms                                          启动服务
  harness-cms admin <邮箱> [builder|editor]            新建管理员，或重置他的密码
  harness-cms key <名字> [write] [structure] [publish]  生成一把接口密钥（只显示一次）";

fn env(name: &str, default: &str) -> String {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| default.to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Check the command before anything is created on disk.
    let known = match args.first().map(String::as_str) {
        None => true,
        Some("admin" | "key") => args.len() >= 2,
        Some(_) => false,
    };
    if !known {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let port = env("HC_PORT", "4200");
    let data = PathBuf::from(env("HC_DATA", "data"));
    let db = env("HC_DB", &data.join("harness.db").to_string_lossy());
    if let Err(e) = std::fs::create_dir_all(&data) {
        eprintln!("无法创建数据目录 {}：{e}", data.display());
        return ExitCode::FAILURE;
    }
    let conn = match store::open(&db) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("无法打开数据库 {db}：{e}");
            return ExitCode::FAILURE;
        }
    };

    match args.first().map(String::as_str) {
        None => serve(conn, data, port),
        Some("admin") => {
            let (Some(email), role) = (args.get(1), args.get(2).map(String::as_str).unwrap_or("builder")) else {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            };
            let chosen = std::env::var("HC_ADMIN_PASSWORD").ok().filter(|p| !p.is_empty());
            match auth::create_admin(&conn, email, role, chosen.as_deref()) {
                Ok(password) if chosen.is_none() => {
                    println!("管理员 {email}（{role}）的密码：{password}");
                    println!("它只显示这一次，请马上保存。");
                    ExitCode::SUCCESS
                }
                Ok(_) => {
                    println!("管理员 {email}（{role}）已设置为 HC_ADMIN_PASSWORD 里的密码。");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("key") => {
            let Some(name) = args.get(1) else {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            };
            let mut scopes = vec![];
            for s in &args[2..] {
                match auth::Scope::parse(s) {
                    Some(scope) => scopes.push(scope),
                    None => {
                        eprintln!("不认识的权限“{s}”：可选 write、structure、publish（read 总是包含）。");
                        return ExitCode::from(2);
                    }
                }
            }
            match auth::create_key(&conn, name, &scopes) {
                Ok(key) => {
                    println!("{key}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(_) => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn serve(conn: rusqlite::Connection, data: PathBuf, port: String) -> ExitCode {
    if let Err(e) = changes::sync_all_indexes(&conn) {
        eprintln!("重建字段索引失败：{e}");
        return ExitCode::FAILURE;
    }
    let origin = env("HC_ORIGIN", &format!("http://127.0.0.1:{port}"));
    let app = App::new(conn, data, origin.trim_end_matches('/').to_string(), env("HC_SITE_NAME", "harness-cms"));
    if let Some(env) = harness_cms::llm::Settings::from_env() {
        let _ = app.env_model.set(env);
    }
    let model = harness_cms::llm::effective(&app.conn());
    if let Some(settings) = model {
        app.set_model(settings.client());
    }
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("无法启动运行时：{e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async move {
        let addr = format!("127.0.0.1:{port}");
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("无法监听 {addr}：{e}");
                return ExitCode::FAILURE;
            }
        };
        println!("harness-cms 已启动：http://{addr}");
        // Stop on Ctrl-C, or on SIGTERM from a process manager such as systemd.
        let stop = async {
            #[cfg(unix)]
            {
                let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = async { match term.as_mut() { Some(t) => { t.recv().await; } None => std::future::pending::<()>().await } } => {}
                }
            }
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
        };
        match axum::serve(listener, api::router(app).into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(stop).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("服务出错：{e}");
                ExitCode::FAILURE
            }
        }
    })
}
