//! 防护 has no page in the admin: it is on, at the level the code sets (`shield::Config::default`). The one thing
//! served here is the script of the challenge page.

use crate::shield;
use axum::http::header;
use axum::response::{IntoResponse, Response};

/// `GET /_hc/shield.js`: the challenge page's script.
pub async fn script() -> Response {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")], shield::SCRIPT).into_response()
}
