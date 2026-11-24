pub mod auth;
pub mod store;
pub mod schema;
pub mod changes;
pub mod query;
pub mod api;
pub mod media;
pub mod tools;
pub mod render;
pub mod site;
pub mod llm;
pub mod chat;
pub mod skills;
pub mod websearch;
pub mod members;
pub mod access;
pub mod backup;
pub mod cache;
pub mod export;
pub mod i18n;
pub mod routes;
pub mod schedule;
pub mod search;
pub mod web;

use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::Instant;

pub type Shared = Arc<App>;

pub struct App {
    pub data_dir: PathBuf,
    pub origin: String,
    pub site_name: String,
    /// The origin is https: cookies get `Secure` and responses get HSTS.
    pub secure_cookies: bool,
    // ponytail: one connection behind a global lock; switch to a pool if write throughput matters.
    conn: Mutex<Connection>,
    /// Admin chat conversations, in memory only.
    pub chat: chat::Store,
    model: RwLock<Option<Arc<dyn llm::Model>>>,
    /// The chat model the server's environment sets (`LLM_*`), set once at startup; 设置 shows it read-only.
    pub env_model: OnceLock<llm::Settings>,
    /// `HC_ADMIN_ORIGIN` from the environment, set once at startup: `Some(origin)` forces 分开网址, `None`
    /// (`off`) forces 同一个网址. Unset, the choice saved in 设置 → 网址 applies.
    pub env_admin_origin: OnceLock<Option<String>>,
    /// After a switch made here, admin requests on other hosts are still answered (GETs redirect) until then.
    pub old_admin_until: Mutex<Option<Instant>>,
    /// The one-time answer `/_hc/ping` gives while a switch checks that the new admin address reaches us.
    pub ping_nonce: Mutex<Option<String>>,
    /// Rendered live pages, kept until something they read is published again.
    pub cache: cache::Cache,
}

impl App {
    /// The site's name: the published `site_name` setting (the draft with `draft`), else `HC_SITE_NAME`.
    pub fn name(&self, conn: &Connection, draft: bool) -> String {
        crate::changes::setting(conn, "site_name", draft).as_str().map_or_else(|| self.site_name.clone(), String::from)
    }

    pub fn new(conn: Connection, data_dir: PathBuf, origin: String, site_name: String) -> Shared {
        Arc::new(App {
            data_dir,
            secure_cookies: origin.starts_with("https://"),
            origin,
            site_name,
            conn: Mutex::new(conn),
            chat: chat::Store::default(),
            model: RwLock::new(None),
            env_model: OnceLock::new(),
            env_admin_origin: OnceLock::new(),
            old_admin_until: Mutex::new(None),
            ping_nonce: Mutex::new(None),
            cache: cache::Cache::default(),
        })
    }

    /// The chat model, when one is configured.
    pub fn model(&self) -> Option<Arc<dyn llm::Model>> {
        self.model.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_model(&self, model: Arc<dyn llm::Model>) {
        *self.model.write().unwrap_or_else(|e| e.into_inner()) = Some(model);
    }

    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }
}
