//! Many languages, one piece of content. The site lists its languages in the drafted setting `languages`, the
//! first being the default. A record keeps its text in the default language as usual and its translations under
//! `_i18n.<code>` in the same document, so translations are drafted, published, versioned and restored with the
//! record. Only text and rich text fields are translated (a field can opt out with `translate: false`); images,
//! dates, prices and relations are shared. Each translated field remembers a fingerprint of the source it was made
//! from (`_i18n.<code>._src.<field>`), so a translation whose source changed since is known to be out of date.

use crate::schema::{CollectionDef, FieldDef, FieldType};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub const KEY: &str = "_i18n";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Language {
    /// A language tag: `zh`, `en`, `ja`, `zh-tw`, `pt-br`…
    pub code: String,
    /// How the language calls itself: 中文, English, 日本語.
    pub name: String,
    /// A host that serves this language at its root (`example.jp`); the `/<code>` prefix works too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Which languages to show, in order, where this one has no translation; the default language comes last anyway.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Languages {
    /// The site's languages, the default first.
    pub list: Vec<Language>,
    /// Keep translations in step: the chat translates what it changes into every language, as drafts.
    #[serde(default)]
    pub sync: bool,
}

impl Languages {
    pub fn default_code(&self) -> Option<&str> {
        self.list.first().map(|l| l.code.as_str())
    }

    pub fn get(&self, code: &str) -> Option<&Language> {
        self.list.iter().find(|l| l.code == code)
    }

    pub fn is_default(&self, code: &str) -> bool {
        self.default_code() == Some(code)
    }

    /// Where to look for text in `code`: itself, its fallbacks, then the default language.
    pub fn chain(&self, code: &str) -> Vec<String> {
        let mut out = vec![code.to_string()];
        if let Some(l) = self.get(code) {
            out.extend(l.fallback.iter().cloned());
        }
        if let Some(d) = self.default_code() {
            out.push(d.to_string());
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|c| seen.insert(c.clone()));
        out
    }

    /// The `/<code>` prefix of a language (empty for the default).
    pub fn prefix(&self, code: &str) -> String {
        if self.is_default(code) { String::new() } else { format!("/{code}") }
    }
}

/// The language a request is served in: its code, where to look for text, the default language, and the path
/// prefix its addresses carry here (`/en`, or empty on its own domain and for the default).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Current {
    pub code: String,
    pub chain: Vec<String>,
    pub default: String,
    pub prefix: String,
}

impl Current {
    pub fn is_default(&self) -> bool {
        self.code == self.default
    }

    pub fn new(langs: &Languages, code: &str, on_own_domain: bool) -> Current {
        Current {
            code: code.to_string(),
            chain: langs.chain(code),
            default: langs.default_code().unwrap_or(code).to_string(),
            prefix: if on_own_domain { String::new() } else { langs.prefix(code) },
        }
    }
}

/// A language tag: lowercase letters, then optional `-` parts (`zh`, `pt-br`).
pub fn valid_code(code: &str) -> bool {
    let mut parts = code.split('-');
    parts.next().is_some_and(|p| (2..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_lowercase()))
        && parts.all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
        && code.len() <= 16
}

/// Checks a languages setting before it is saved.
pub fn check(l: &Languages) -> Result<(), String> {
    if l.list.is_empty() {
        return Err("至少要有一种语言（第一种是默认语言）。".into());
    }
    let mut seen = std::collections::HashSet::new();
    for lang in &l.list {
        if !valid_code(&lang.code) {
            return Err(format!("语言代码“{}”不合法：用小写的语言代码，比如 zh、en、ja、zh-tw、pt-br。", lang.code));
        }
        if !seen.insert(lang.code.clone()) {
            return Err(format!("语言“{}”重复了。", lang.code));
        }
        if lang.name.trim().is_empty() {
            return Err(format!("语言“{}”要有一个名字，比如 English。", lang.code));
        }
        if let Some(d) = &lang.domain {
            if d.contains('/') || d.contains(':') || !d.contains('.') {
                return Err(format!("“{d}”不是域名：只写主机名，比如 example.jp。"));
            }
        }
        if ["admin", "api", "uploads", "account", "mcp"].contains(&lang.code.as_str()) {
            return Err(format!("语言代码“{}”和系统的网址冲突，请换一个。", lang.code));
        }
    }
    for lang in &l.list {
        if let Some(f) = lang.fallback.iter().find(|f| !seen.contains(*f)) {
            return Err(format!("语言“{}”的回退语言“{f}”不在语言列表里。", lang.code));
        }
    }
    Ok(())
}

/// The site's languages as `draft` sees them; None for a single-language site that never set any.
pub fn load(conn: &rusqlite::Connection, draft: bool) -> Option<Languages> {
    let v = crate::changes::setting(conn, "languages", draft);
    serde_json::from_value::<Languages>(v).ok().filter(|l| !l.list.is_empty())
}

/// Whether a field is translated: text and rich text are, unless they say `translate: false`.
pub fn translated(f: &FieldDef) -> bool {
    f.translate.unwrap_or(matches!(f.kind, FieldType::Text | FieldType::Richtext)) && matches!(f.kind, FieldType::Text | FieldType::Richtext)
}

/// A short fingerprint of a source value.
pub fn fingerprint(v: &Value) -> String {
    hex::encode(&Sha256::digest(v.to_string().as_bytes())[..6])
}

/// The record as `chain[0]` reads it: each translated field from the first language in `chain` that has it
/// (the default language's text is the document itself). Undeclared keys (`_i18n`) are dropped.
pub fn localize(def: &CollectionDef, doc: &Value, chain: &[String], default: &str) -> Value {
    let mut out = crate::schema::project(def, doc);
    let Some(map) = out.as_object_mut() else { return out };
    for f in def.fields.iter().filter(|f| translated(f)) {
        for code in chain {
            if code == default {
                break;
            }
            if let Some(v) = doc[KEY][code].get(&f.name).filter(|v| !is_blank(v)) {
                map.insert(f.name.clone(), v.clone());
                break;
            }
        }
    }
    out
}

fn is_blank(v: &Value) -> bool {
    v.is_null() || v.as_str().is_some_and(|s| s.trim().is_empty())
}

/// Whether a record shows in `code`: the default always; another language when it has a translation, or when
/// its collection falls back to the default language; never when it is hidden there.
pub fn visible(def: &CollectionDef, doc: &Value, code: &str, default: &str) -> bool {
    if code == default {
        return true;
    }
    let t = &doc[KEY][code];
    if t["_hide"].as_bool() == Some(true) {
        return false;
    }
    def.untranslated != Some(crate::schema::Untranslated::Hide) || t.as_object().is_some_and(|o| o.keys().any(|k| !k.starts_with('_')))
}

/// Per language, the state of each translated field that has a source value: `done`, `stale` (the source changed
/// since it was translated) or `missing`.
pub fn status(def: &CollectionDef, doc: &Value, code: &str) -> Map<String, Value> {
    let mut out = Map::new();
    for f in def.fields.iter().filter(|f| translated(f)) {
        let Some(src) = doc.get(&f.name).filter(|v| !is_blank(v)) else { continue };
        let t = &doc[KEY][code];
        let state = match t.get(&f.name).filter(|v| !is_blank(v)) {
            None => "missing",
            Some(_) if t["_src"][&f.name].as_str() != Some(&fingerprint(src)) => "stale",
            Some(_) => "done",
        };
        out.insert(f.name.clone(), json!(state));
    }
    out
}

/// The worst state of a record in a language: `missing` over `stale` over `done`; None with nothing to translate.
pub fn summary(def: &CollectionDef, doc: &Value, code: &str) -> Option<&'static str> {
    let s = status(def, doc, code);
    if s.is_empty() {
        return None;
    }
    let has = |x: &str| s.values().any(|v| v == x);
    Some(if has("missing") { "missing" } else if has("stale") { "stale" } else { "done" })
}

/// Text direction of a language.
pub fn dir(code: &str) -> &'static str {
    match code.split('-').next().unwrap_or_default() {
        "ar" | "he" | "fa" | "ur" | "yi" | "ps" | "sd" | "ug" | "dv" => "rtl",
        _ => "ltr",
    }
}

// ---- formats ----

fn base(code: &str) -> &str {
    code.split('-').next().unwrap_or(code)
}

const MONTHS_EN: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_FR: [&str; 12] = ["janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.", "déc."];
const MONTHS_ES: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic"];

/// A `YYYY-MM-DD…` date the way `code` writes it; anything else comes back as it is.
pub fn format_date(s: &str, code: &str) -> String {
    let (Some(y), Some(m), Some(d)) = (s.get(0..4).and_then(|v| v.parse::<u32>().ok()), s.get(5..7).and_then(|v| v.parse::<usize>().ok()), s.get(8..10).and_then(|v| v.parse::<u32>().ok())) else {
        return s.to_string();
    };
    if !(1..=12).contains(&m) {
        return s.to_string();
    }
    match base(code) {
        "zh" | "ja" => format!("{y}年{m}月{d}日"),
        "ko" => format!("{y}년 {m}월 {d}일"),
        "en" => format!("{} {d}, {y}", MONTHS_EN[m - 1]),
        "fr" => format!("{d} {} {y}", MONTHS_FR[m - 1]),
        "es" | "pt" => format!("{d} {} {y}", MONTHS_ES[m - 1]),
        "de" | "ru" | "pl" | "tr" => format!("{d:02}.{m:02}.{y}"),
        _ => format!("{y}-{m:02}-{d:02}"),
    }
}

/// A number with the thousands and decimal separators of `code`, `decimals` places.
pub fn format_number(n: f64, code: &str, decimals: usize) -> String {
    let (group, point) = match base(code) {
        "fr" | "ru" | "pl" | "cs" | "sv" | "nb" | "fi" => ('\u{202f}', ','),
        "de" | "es" | "it" | "pt" | "nl" | "tr" | "id" => ('.', ','),
        _ => (',', '.'),
    };
    let fixed = format!("{:.*}", decimals, n.abs());
    let (int, frac) = fixed.split_once('.').map_or((fixed.as_str(), ""), |(a, b)| (a, b));
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(group);
        }
        grouped.push(c);
    }
    let sign = if n < 0.0 { "-" } else { "" };
    if frac.is_empty() { format!("{sign}{grouped}") } else { format!("{sign}{grouped}{point}{frac}") }
}

/// An amount of `currency` (ISO code) the way `code` writes it.
pub fn format_money(n: f64, currency: &str, code: &str) -> String {
    let cur = currency.to_ascii_uppercase();
    let decimals = if ["JPY", "KRW", "VND", "IDR"].contains(&cur.as_str()) { 0 } else { 2 };
    let symbol = match cur.as_str() {
        "CNY" | "JPY" => "¥",
        "USD" => "$",
        "EUR" => "€",
        "GBP" => "£",
        "KRW" => "₩",
        "HKD" => "HK$",
        "TWD" => "NT$",
        _ => "",
    };
    let num = format_number(n, code, decimals);
    match (symbol, base(code)) {
        ("", _) => format!("{num} {cur}"),
        (s, "fr" | "de" | "es" | "it" | "pt" | "ru" | "pl" | "nl") => format!("{num} {s}"),
        (s, _) => format!("{s}{num}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_chains() {
        assert_eq!(format_date("2026-11-24", "zh"), "2026年11月24日");
        assert_eq!(format_date("2026-11-24T10:00:00Z", "en"), "Nov 24, 2026");
        assert_eq!(format_date("2026-11-24", "fr"), "24 nov. 2026");
        assert_eq!(format_date("2026-11-24", "de"), "24.11.2026");
        assert_eq!(format_date("soon", "en"), "soon");
        assert_eq!(format_number(1234567.5, "en", 2), "1,234,567.50");
        assert_eq!(format_number(1234567.5, "de", 1), "1.234.567,5");
        assert_eq!(format_money(1299.0, "CNY", "zh"), "¥1,299.00");
        assert_eq!(format_money(1299.0, "EUR", "fr"), "1\u{202f}299,00 €");
        assert_eq!(format_money(5000.0, "JPY", "ja"), "¥5,000");
        let l = Languages { list: vec![
            Language { code: "zh".into(), name: "中文".into(), domain: None, fallback: vec![] },
            Language { code: "en".into(), name: "English".into(), domain: None, fallback: vec![] },
            Language { code: "zh-tw".into(), name: "繁體中文".into(), domain: None, fallback: vec!["en".into()] },
        ], sync: false };
        assert_eq!(l.chain("zh-tw"), ["zh-tw", "en", "zh"]);
        assert_eq!(l.chain("zh"), ["zh"]);
        assert!(valid_code("pt-br") && !valid_code("EN") && !valid_code("e"));
        assert_eq!(dir("ar"), "rtl");
    }
}
