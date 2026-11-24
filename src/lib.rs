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
pub mod web;

use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

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
}

impl App {
    pub fn new(conn: Connection, data_dir: PathBuf, origin: String, site_name: String) -> Shared {
        Arc::new(App {
            data_dir,
            secure_cookies: origin.starts_with("https://"),
            origin,
            site_name,
            conn: Mutex::new(conn),
            chat: chat::Store::default(),
            model: RwLock::new(None),
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
