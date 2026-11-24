//! minijinja environments. The admin templates are embedded in the binary; `.html` names autoescape.

use minijinja::Environment;
use serde_json::Value;
use std::sync::LazyLock;

static ADMIN: LazyLock<Environment<'static>> = LazyLock::new(|| {
    let mut env = Environment::new();
    for (name, source) in [
        ("login.html", include_str!("web/templates/login.html")),
        ("home.html", include_str!("web/templates/home.html")),
    ] {
        env.add_template(name, source).expect("embedded admin template parses");
    }
    env
});

/// Renders one embedded admin template.
pub fn admin_page(name: &str, ctx: Value) -> Result<String, String> {
    ADMIN
        .get_template(name)
        .and_then(|t| t.render(ctx))
        .map_err(|e| format!("渲染后台页面“{name}”失败：{e}"))
}
