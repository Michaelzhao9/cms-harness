//! Collection and field definitions: content structure as data.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum FieldType { Text, Richtext, Number, Boolean, Date, Select, Media, Relation }

#[derive(Serialize, Deserialize, JsonSchema, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct FieldDef {
    pub name: String,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: FieldType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub unique: bool,
    #[serde(default)]
    pub index: bool,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Default, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Access {
    #[serde(default)]
    pub public_read: bool,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct CollectionDef {
    pub name: String,
    pub title: String,
    #[serde(default)]
    pub single: bool,
    #[serde(default)]
    pub access: Access,
    pub fields: Vec<FieldDef>,
}

impl CollectionDef {
    pub fn field(&self, name: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.name == name)
    }
}

const RESERVED: [&str; 5] = ["id", "sort", "limit", "offset", "expand"];

/// `^[a-z][a-z0-9_]{0,39}$`, minus reserved words.
pub fn valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && s.len() <= 40
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !RESERVED.contains(&s)
}

pub fn check_def(def: &CollectionDef) -> Result<(), String> {
    use FieldType::*;
    if !valid_name(&def.name) {
        return Err(format!("集合名“{}”不合法：只能用小写字母开头，后接小写字母、数字或下划线，最长 40 个字符，且不能是保留字（id、sort、limit、offset、expand）。", def.name));
    }
    for (i, f) in def.fields.iter().enumerate() {
        let n = &f.name;
        if !valid_name(n) {
            return Err(format!("字段名“{n}”不合法：只能用小写字母开头，后接小写字母、数字或下划线，最长 40 个字符，且不能是保留字（id、sort、limit、offset、expand）。"));
        }
        if def.fields[..i].iter().any(|g| g.name == *n) {
            return Err(format!("字段名“{n}”重复：同一个集合里每个字段名只能出现一次，请改名或删除多余的字段。"));
        }
        if f.title.trim().is_empty() {
            return Err(format!("字段“{n}”的标题为空：请给它填一个人能看懂的标题。"));
        }
        if (f.kind == Select) != !f.options.is_empty() {
            return Err(format!("字段“{n}”的选项设置不对：只有 select 类型的字段能且必须填写 options（非空列表）。"));
        }
        if f.kind == Select {
            for (j, o) in f.options.iter().enumerate() {
                if f.options[..j].contains(o) {
                    return Err(format!("字段“{n}”的选项“{o}”重复：请删除重复的选项。"));
                }
            }
        }
        match (&f.kind, &f.target) {
            (Relation, Some(t)) if valid_name(t) => {}
            (Relation, Some(t)) => return Err(format!("字段“{n}”的 target“{t}”不是合法的集合名：请填写目标集合的名字。")),
            (Relation, None) => return Err(format!("关联字段“{n}”缺少 target：请填写要关联的集合名。")),
            (_, Some(_)) => return Err(format!("字段“{n}”不是 relation 类型，不能设置 target：请删除 target。")),
            (_, None) => {}
        }
        if f.multiple && !matches!(f.kind, Select | Media | Relation) {
            return Err(format!("字段“{n}”不能设置 multiple：只有 select、media、relation 类型支持多值。"));
        }
        if f.unique && (!matches!(f.kind, Text | Number | Date) || f.multiple) {
            return Err(format!("字段“{n}”不能设置 unique：只有单值的 text、number、date 字段可以要求唯一。"));
        }
        if f.index && (!matches!(f.kind, Text | Number | Boolean | Date | Select | Relation) || f.multiple) {
            return Err(format!("字段“{n}”不能设置 index：只有单值的 text、number、boolean、date、select、relation 字段可以建索引。"));
        }
        if let Some(d) = &f.default {
            check_default(f, d)?;
        }
    }
    Ok(())
}

fn check_default(f: &FieldDef, d: &Value) -> Result<(), String> {
    check_value(f, d)
        .map(|_| ())
        .map_err(|e| format!("字段“{}”的默认值不合法：{e}", f.name))
}

pub const MAX_TEXT: usize = 100_000;
pub const MAX_RICHTEXT: usize = 1_000_000;

fn digits(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) { return None; }
    s.parse().ok()
}

fn valid_date(s: &str) -> bool {
    if !s.is_ascii() || !(s.len() == 10 || s.len() == 20) { return false; }
    let b = s.as_bytes();
    if b[4] != b'-' || b[7] != b'-' { return false; }
    let (Some(y), Some(m), Some(d)) = (digits(&s[..4]), digits(&s[5..7]), digits(&s[8..10])) else { return false };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = match m { 1 | 3 | 5 | 7 | 8 | 10 | 12 => 31, 4 | 6 | 9 | 11 => 30, 2 => if leap { 29 } else { 28 }, _ => return false };
    if d < 1 || d > days { return false; }
    if s.len() == 10 { return true; }
    b[10] == b'T' && b[13] == b':' && b[16] == b':' && b[19] == b'Z'
        && matches!(digits(&s[11..13]), Some(h) if h < 24)
        && matches!(digits(&s[14..16]), Some(n) if n < 60)
        && matches!(digits(&s[17..19]), Some(n) if n < 60)
}

fn sanitise(html: &str) -> String {
    use std::collections::HashSet;
    // style and on* attributes are not in ammonia's allow-list; script content is dropped explicitly.
    ammonia::Builder::default()
        .url_schemes(HashSet::from(["http", "https", "mailto", "tel"]))
        .clean_content_tags(HashSet::from(["script", "style"]))
        .clean(html)
        .to_string()
}

fn pos_int(v: &Value) -> bool {
    v.as_u64().is_some_and(|n| n > 0)
}

/// Checks one value against one field and returns the cleaned value. `null` is always accepted.
fn check_value(f: &FieldDef, value: &Value) -> Result<Value, String> {
    use FieldType::*;
    let n = &f.name;
    if value.is_null() { return Ok(Value::Null); }
    let bad = |why: &str| format!("字段“{n}”的值 {value} 不合法：{why}");
    let single = |v: &Value| -> Result<Value, String> {
        let bad = |why: &str| format!("字段“{n}”的值 {v} 不合法：{why}");
        let s = v.as_str();
        match f.kind {
            Text => match s {
                Some(s) if s.chars().count() <= MAX_TEXT => Ok(v.clone()),
                Some(_) => Err(bad(&format!("文本最长 {MAX_TEXT} 个字符，请缩短。"))),
                None => Err(bad("必须是字符串。")),
            },
            Richtext => match s {
                Some(s) if s.chars().count() <= MAX_RICHTEXT => Ok(Value::String(sanitise(s))),
                Some(_) => Err(bad(&format!("富文本最长 {MAX_RICHTEXT} 个字符，请缩短。"))),
                None => Err(bad("必须是字符串。")),
            },
            Number if v.as_f64().is_some_and(f64::is_finite) => Ok(v.clone()),
            Number => Err(bad("必须是有限的数字。")),
            Boolean if v.is_boolean() => Ok(v.clone()),
            Boolean => Err(bad("必须是 true 或 false。")),
            Date if s.is_some_and(valid_date) => Ok(v.clone()),
            Date => Err(bad("必须是真实存在的 YYYY-MM-DD 或 YYYY-MM-DDTHH:MM:SSZ 日期。")),
            Select if s.is_some_and(|s| f.options.iter().any(|o| o == s)) => Ok(v.clone()),
            Select => Err(bad(&format!("必须是这些选项之一：{}。", f.options.join("、")))),
            Media if s.is_some_and(|s| (s.starts_with("/uploads/") && !s.contains("..")) || s.starts_with("http://") || s.starts_with("https://")) => Ok(v.clone()),
            Media => Err(bad("必须是 /uploads/ 开头的上传路径（不含 ..）或 http(s) 地址。")),
            Relation if pos_int(v) => Ok(v.clone()),
            Relation => Err(bad("必须是正整数 id。")),
        }
    };
    if !f.multiple { return single(value); }
    let Some(items) = value.as_array() else { return Err(bad("多值字段必须是数组。")) };
    let mut out: Vec<Value> = Vec::with_capacity(items.len());
    for it in items {
        let c = single(it)?;
        if f.kind != FieldType::Media && out.contains(&c) {
            return Err(bad(&format!("数组里有重复的元素 {it}。")));
        }
        out.push(c);
    }
    Ok(Value::Array(out))
}

fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// Merges `changes` over `base`, validates and sanitises every declared field, checks required ones.
/// Keys in `base` that the definition no longer declares are carried through untouched.
pub fn normalize(def: &CollectionDef, base: &Value, changes: &Value) -> Result<Value, String> {
    let empty = serde_json::Map::new();
    let changes = changes.as_object().ok_or("要写入的内容必须是 JSON 对象：请传入形如 {\"字段名\": 值} 的对象。")?;
    let base = base.as_object().unwrap_or(&empty);
    if let Some(k) = changes.keys().find(|k| def.field(k).is_none()) {
        return Err(format!("集合“{}”没有字段“{k}”：请检查字段名，或先修改集合定义。", def.name));
    }
    let mut out: serde_json::Map<String, Value> =
        base.iter().filter(|(k, _)| def.field(k).is_none()).map(|(k, v)| (k.clone(), v.clone())).collect();
    for f in &def.fields {
        if let Some(v) = changes.get(&f.name).or_else(|| base.get(&f.name)) {
            out.insert(f.name.clone(), check_value(f, v)?);
        }
        if f.required && f.default.is_none() && out.get(&f.name).is_none_or(is_empty) {
            return Err(format!("字段“{}”是必填的：请提供一个非空的值。", f.name));
        }
    }
    Ok(Value::Object(out))
}

/// Declared fields only; a missing or null value falls back to the field's default, else `null`.
pub fn project(def: &CollectionDef, doc: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for f in &def.fields {
        let v = doc.get(&f.name).filter(|v| !v.is_null()).or(f.default.as_ref());
        out.insert(f.name.clone(), v.cloned().unwrap_or(Value::Null));
    }
    Value::Object(out)
}

fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

fn number_text(n: &serde_json::Number) -> String {
    match n.as_f64() {
        Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
        _ => n.to_string(),
    }
}

/// Converts a stored value from one field definition to another; `None` when no rule applies.
pub fn convert(value: &Value, from: &FieldDef, to: &FieldDef) -> Option<Value> {
    use FieldType::*;
    if value.is_null() { return Some(Value::Null); }
    let converted = if from.kind == to.kind {
        match (from.multiple, to.multiple) {
            (false, true) => Value::Array(vec![value.clone()]),
            (true, false) => match value.as_array()?.as_slice() {
                [] => Value::Null,
                [one] => one.clone(),
                _ => return None,
            },
            _ => value.clone(),
        }
    } else if from.multiple || to.multiple {
        return None;
    } else {
        match (from.kind, to.kind, value) {
            (Text, Richtext, Value::String(s)) => Value::String(
                s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")),
            (Richtext, Text, Value::String(s)) => Value::String(strip_tags(s)),
            (Number, Text, Value::Number(n)) => Value::String(number_text(n)),
            (Boolean, Text, Value::Bool(b)) => Value::String(b.to_string()),
            (Date | Select, Text, Value::String(_)) => value.clone(),
            (Text, Number, Value::String(s)) => {
                let s = s.trim();
                match s.parse::<i64>() {
                    Ok(i) => Value::from(i),
                    Err(_) => Value::from(s.parse::<f64>().ok().filter(|f| f.is_finite())?),
                }
            }
            (Text, Date | Select, Value::String(_)) => value.clone(),
            _ => return None,
        }
    };
    check_value(to, &converted).ok()
}

/// Brings a stored document in line with a changed definition: keeps values that are still valid,
/// converts the rest, and fails naming the field that cannot be converted.
pub fn migrate(old: &CollectionDef, new: &CollectionDef, doc: &Value) -> Result<Value, String> {
    let mut doc = doc.clone();
    for f in &new.fields {
        let Some(o) = old.field(&f.name) else { continue };
        if (o.kind, o.multiple, &o.options) == (f.kind, f.multiple, &f.options) { continue; }
        let Some(v) = doc.get(&f.name).filter(|v| !v.is_null()).cloned() else { continue };
        let fixed = match check_value(f, &v) {
            Ok(c) => c,
            Err(_) => convert(&v, o, f).ok_or_else(|| {
                format!("字段“{}”的现有值 {v} 无法转换成新的类型或选项：请先修改或清空这个值，再改字段定义。", f.name)
            })?,
        };
        doc[&f.name] = fixed;
    }
    normalize(new, &doc, &serde_json::json!({}))
}
