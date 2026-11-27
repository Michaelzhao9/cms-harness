//! minijinja environments. The admin templates are embedded in the binary; `.html` names autoescape.

use minijinja::Environment;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

pub const ADMIN_CSS: &str = include_str!("web/admin.css");
pub const ADMIN_JS: &str = include_str!("web/admin.js");

/// Short content hash of the stylesheet: pages link `admin.css?v=<this>`, so the long cache never serves a stale copy.
pub static CSS_VERSION: LazyLock<String> = LazyLock::new(|| hex::encode(Sha256::digest(ADMIN_CSS))[..12].to_string());
pub static JS_VERSION: LazyLock<String> = LazyLock::new(|| hex::encode(Sha256::digest(ADMIN_JS))[..12].to_string());

static ADMIN: LazyLock<Environment<'static>> = LazyLock::new(|| {
    let mut env = Environment::new();
    for (name, source) in [
        ("login.html", include_str!("web/templates/login.html")),
        ("icons.html", include_str!("web/templates/icons.html")),
        ("base.html", include_str!("web/templates/base.html")),
        ("nav.html", include_str!("web/templates/nav.html")),
        ("home.html", include_str!("web/templates/home.html")),
        ("chat.html", include_str!("web/templates/chat.html")),
        ("chat_log.html", include_str!("web/templates/chat_log.html")),
        ("collection.html", include_str!("web/templates/collection.html")),
        ("record.html", include_str!("web/templates/record.html")),
        ("media.html", include_str!("web/templates/media.html")),
        ("confirm.html", include_str!("web/templates/confirm.html")),
        ("publish.html", include_str!("web/templates/publish.html")),
        ("releases.html", include_str!("web/templates/releases.html")),
        ("env.html", include_str!("web/templates/env.html")),
        ("settings.html", include_str!("web/templates/settings.html")),
        ("members.html", include_str!("web/templates/members.html")),
    ] {
        env.add_template(name, source).expect("embedded admin template parses");
    }
    env.add_filter("t", crate::i18n::filter);
    env.add_global("css_v", CSS_VERSION.as_str());
    env.add_global("js_v", JS_VERSION.as_str());
    env
});

/// Renders one embedded admin template.
pub fn admin_page(name: &str, ctx: Value) -> Result<String, String> {
    ADMIN
        .get_template(name)
        .and_then(|t| t.render(ctx))
        .map_err(|e| {
            // The innermost cause says what went wrong (an include wraps the real error).
            let mut why = e.to_string();
            let mut src = std::error::Error::source(&e);
            while let Some(s) = src {
                why = format!("{why}：{s}");
                src = s.source();
            }
            format!("渲染后台页面“{name}”失败：{why}")
        })
}
