//! Chinese and English. The Chinese text is the message id: every string the admin shows is written in Chinese in
//! the code and the templates, and `tr` looks it up in the English catalog (`i18n/en.tsv`). A catalog entry may
//! hold `{}` placeholders, so messages built with `format!` translate too; the pieces that fill them are
//! translated in turn. Anything not in the catalog shows as written.

use axum::http::{header, HeaderMap};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }

    pub fn parse(s: &str) -> Option<Lang> {
        match s {
            "zh" => Some(Lang::Zh),
            "en" => Some(Lang::En),
            _ => None,
        }
    }

    /// The `lang` attribute for `<html>`.
    pub fn html(self) -> &'static str {
        match self {
            Lang::Zh => "zh-CN",
            Lang::En => "en",
        }
    }
}

/// The cookie that keeps a visitor's pick from the 中文 / EN switch.
pub const COOKIE: &str = "hc_lang";

/// `lang=zh|en` in a query string: the switch's links.
pub fn from_query(query: Option<&str>) -> Option<Lang> {
    query?.split('&').find_map(|kv| kv.strip_prefix("lang=")).and_then(Lang::parse)
}

/// A visitor's language: the switch's link, else its cookie, else the browser's.
pub fn for_visitor(headers: &HeaderMap, query: Option<&str>) -> Lang {
    from_query(query)
        .or_else(|| crate::web::session::cookie_value(headers, COOKIE).as_deref().and_then(Lang::parse))
        .unwrap_or_else(|| from_headers(headers))
}

/// The `Set-Cookie` that remembers a pick for a year.
pub fn remember(lang: Lang) -> String {
    format!("{COOKIE}={}; Path=/; Max-Age=31536000; SameSite=Lax", lang.code())
}

/// The browser's first choice between the two: Chinese for `zh…`, English otherwise.
pub fn from_headers(headers: &HeaderMap) -> Lang {
    let first = headers.get(header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()).and_then(|v| v.split(',').next()).unwrap_or("zh");
    if first.trim().to_ascii_lowercase().starts_with("zh") || first.trim().is_empty() {
        Lang::Zh
    } else {
        Lang::En
    }
}

struct Catalog {
    exact: HashMap<&'static str, &'static str>,
    /// (literal pieces between `{}`, English with `{}`), longest Chinese first.
    patterns: Vec<(Vec<&'static str>, &'static str)>,
}

static EN: LazyLock<Catalog> = LazyLock::new(|| {
    let mut exact = HashMap::new();
    let mut patterns = vec![];
    for line in include_str!("i18n/en.tsv").lines() {
        let Some((zh, en)) = line.split_once('\t') else { continue };
        if zh.is_empty() || zh.starts_with('#') {
            continue;
        }
        if zh.contains("{}") {
            patterns.push((zh.split("{}").collect::<Vec<_>>(), en));
        } else {
            exact.insert(zh, en);
        }
    }
    patterns.sort_by_key(|(parts, _)| std::cmp::Reverse(parts.iter().map(|p| p.len()).sum::<usize>()));
    Catalog { exact, patterns }
});

/// Matches `s` against literal pieces with a gap between each pair; returns the gaps.
fn captures<'a>(parts: &[&str], s: &'a str) -> Option<Vec<&'a str>> {
    let (first, rest) = parts.split_first()?;
    let mut at = s.strip_prefix(first)?;
    let mut out = vec![];
    for (i, lit) in rest.iter().enumerate() {
        let last = i == rest.len() - 1;
        let pos = if last {
            if !at.ends_with(lit) || at.len() < lit.len() {
                return None;
            }
            at.len() - lit.len()
        } else if lit.is_empty() {
            return None; // two placeholders in a row cannot be split
        } else {
            at.find(lit)?
        };
        out.push(&at[..pos]);
        at = &at[pos + lit.len()..];
    }
    if parts.len() == 1 && !at.is_empty() {
        return None;
    }
    Some(out)
}

/// The `t` template filter: the text in the language the context's `lang` names (`zh` when it names none).
pub fn filter(state: &minijinja::State, value: minijinja::Value) -> String {
    let lang = state.lookup("lang").and_then(|l| l.as_str().and_then(Lang::parse)).unwrap_or(Lang::Zh);
    match value.as_str() {
        Some(s) => tr(lang, s).into_owned(),
        None => value.to_string(),
    }
}

impl Lang {
    /// The admin's saved choice, Chinese when there is none.
    pub fn of_prefs(prefs: &serde_json::Value) -> Lang {
        prefs["lang"].as_str().and_then(Lang::parse).unwrap_or(Lang::Zh)
    }
}

/// `s` in `lang`.
pub fn tr(lang: Lang, s: &str) -> Cow<'_, str> {
    if lang == Lang::Zh || s.is_empty() || !s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c) || "，。：；“”（）「」、？！".contains(c)) {
        return Cow::Borrowed(s);
    }
    if let Some(en) = EN.exact.get(s) {
        return Cow::Borrowed(en);
    }
    // Surrounding spaces (template text nodes) are kept as they are.
    let trimmed = s.trim();
    if trimmed.len() != s.len() {
        if let Some(en) = EN.exact.get(trimmed) {
            return Cow::Owned(s.replacen(trimmed, en, 1));
        }
    }
    for (parts, en) in &EN.patterns {
        if let Some(gaps) = captures(parts, s) {
            let mut out = String::new();
            let mut pieces = en.split("{}");
            out.push_str(pieces.next().unwrap_or_default());
            for (gap, piece) in gaps.iter().zip(pieces) {
                out.push_str(&tr(lang, gap));
                out.push_str(piece);
            }
            return Cow::Owned(out);
        }
    }
    Cow::Borrowed(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_patterns_and_nesting() {
        assert_eq!(tr(Lang::Zh, "发布"), "发布");
        assert_eq!(tr(Lang::En, "发布"), "Publish");
        assert_eq!(tr(Lang::En, "  发布 "), "  Publish ");
        assert_eq!(tr(Lang::En, "共 3 人"), "3 people");
        assert_eq!(tr(Lang::En, "not chinese"), "not chinese");
        assert_eq!(tr(Lang::En, "没有对应的东西xyz"), "没有对应的东西xyz");
        assert_eq!(captures(&["a", "b", "c"], "a1b2c"), Some(vec!["1", "2"]));
        assert_eq!(captures(&["a", ""], "axyz"), Some(vec!["xyz"]));
        assert_eq!(captures(&["", "c"], "xyzc"), Some(vec!["xyz"]));
        assert_eq!(captures(&["a", "b"], "a1"), None);
    }
}
