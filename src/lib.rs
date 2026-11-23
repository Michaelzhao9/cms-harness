pub mod store;
pub mod schema;

use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

pub type Shared = Arc<App>;

pub struct App {
    pub data_dir: PathBuf,
    pub origin: String,
    pub site_name: String,
    // ponytail: one connection behind a global lock; switch to a pool if write throughput matters.
    conn: Mutex<Connection>,
}

impl App {
    pub fn new(conn: Connection, data_dir: PathBuf, origin: String, site_name: String) -> Shared {
        Arc::new(App { data_dir, origin, site_name, conn: Mutex::new(conn) })
    }

    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }
}
