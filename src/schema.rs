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
    /// Text and rich text are translated into the site's other languages unless this is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translate: Option<bool>,
}

/// What another language shows of a record that has no translation there.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Untranslated {
    /// The default language's text (the default).
    Fallback,
    /// Nothing: the record is not in that language.
    Hide,
}

/// Who may read a collection's live records (content API and built-in site).
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Default, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Read {
    /// Anyone.
    Public,
    /// Logged-in members (and API keys).
    Members,
    /// Each member their own records (API keys see all).
    Owner,
    /// API keys and the admin only.
    #[default]
    Private,
}

/// Who may add records from the site: they go live at once, stamped with their owner.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Default, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Create {
    #[default]
    None,
    Members,
    Public,
}

#[derive(Serialize, JsonSchema, Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Access {
    #[serde(default)]
    pub read: Read,
    #[serde(default)]
    pub create: Create,
}

impl<'de> Deserialize<'de> for Access {
    /// Also reads the old `{ "public_read": true }` form as `read: public`.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            read: Option<Read>,
            #[serde(default)]
            create: Create,
            #[serde(default)]
            public_read: bool,
        }
        let r = Raw::deserialize(d)?;
        let read = r.read.unwrap_or(if r.public_read { Read::Public } else { Read::Private });
        Ok(Access { read, create: r.create })
    }
}

impl Access {
    pub fn is_public(&self) -> bool {
        self.read == Read::Public
    }
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
    /// Where each record's page lives, like `/news/{slug}`; `/{name}/{slug}` when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// In another language, records without a translation fall back to the default language, or are hidden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untranslated: Option<Untranslated>,
    /// The admin nav's group this collection sits in (the site's section, like 租赁业务); none: at the top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Its place in the admin nav, lowest first; a group sits where its first collection does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
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
    if def.group.as_deref().is_some_and(|g| g.trim().is_empty() || g.chars().count() > 40) {
        return Err("集合的 group（后台导航分组）要是 1 到 40 个字的名字，或者不设。".into());
    }
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
        if f.unique && f.default.is_some() {
            return Err(format!("字段“{n}”不能同时设置 unique 和默认值：每条记录都会拿到同一个默认值，必然重复。请去掉其中一个。"));
        }
        if f.index && (!matches!(f.kind, Text | Number | Boolean | Date | Select | Relation) || f.multiple) {
            return Err(format!("字段“{n}”不能设置 index：只有单值的 text、number、boolean、date、select、relation 字段可以建索引。"));
        }
        if let Some(d) = &f.default {
            check_default(f, d)?;
        }
    }
    if let Some(p) = &def.url {
        crate::routes::check_pattern(def, p)?;
    }
    Ok(())
}

fn check_default(f: &FieldDef, d: &Value) -> Result<(), String> {
    check_value(f, d).map(|_| ()).map_err(|e| format!("默认值不合法，{e}"))
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
    // ammonia cannot express "all on* attributes" as a rule, so the allow-list is its default plus a filter that drops them.
    ammonia::Builder::default()
        .url_schemes(HashSet::from(["http", "https", "mailto", "tel"]))
        .clean_content_tags(HashSet::from(["script", "style"]))
        .rm_generic_attributes(&["style"])
        .attribute_filter(|_, attr, v| {
            let a = attr.to_ascii_lowercase();
            (a != "style" && !a.starts_with("on")).then(|| v.into())
        })
        .clean(html)
        .to_string()
}

fn pos_int(v: &Value) -> bool {
    v.as_u64().is_some_and(|n| n > 0)
}

const MAX_ITEMS: usize = 1000;

/// The value for error messages: at most 50 characters.
fn show(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 50 { format!("{}…", s.chars().take(50).collect::<String>()) } else { s }
}

/// Checks one value against one field and returns the cleaned value. `null` is always accepted.
pub fn check_value(f: &FieldDef, value: &Value) -> Result<Value, String> {
    use FieldType::*;
    let n = &f.name;
    if value.is_null() { return Ok(Value::Null); }
    let bad = |v: &Value, why: &str| format!("字段“{n}”的值 {} 不合法：{why}", show(v));
    let single = |v: &Value| -> Result<Value, String> {
        let bad = |why: &str| bad(v, why);
        let s = v.as_str();
        match f.kind {
            Text => match s {
                Some(s) if s.chars().count() <= MAX_TEXT => Ok(v.clone()),
                Some(s) => Err(bad(&format!("文本有 {} 个字符，超过上限 {MAX_TEXT}，请缩短。", s.chars().count()))),
                None => Err(bad("必须是字符串。")),
            },
            Richtext => match s {
                Some(s) if s.chars().count() <= MAX_RICHTEXT => Ok(Value::String(sanitise(s))),
                Some(s) => Err(bad(&format!("富文本有 {} 个字符，超过上限 {MAX_RICHTEXT}，请缩短。", s.chars().count()))),
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
    let Some(items) = value.as_array() else { return Err(bad(value, "多值字段必须是数组。")) };
    if items.len() > MAX_ITEMS {
        return Err(bad(value, &format!("数组有 {} 个元素，超过上限 {MAX_ITEMS}。", items.len())));
    }
    let mut out: Vec<Value> = Vec::with_capacity(items.len());
    for it in items {
        let c = single(it)?;
        if f.kind != FieldType::Media && out.contains(&c) {
            return Err(bad(value, &format!("数组里有重复的元素 {}。", show(it))));
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
        // explicit null/empty always fails a required field; an absent key passes only with a default
        if f.required && out.get(&f.name).map_or(f.default.is_none(), is_empty) {
            return Err(format!("字段“{}”是必填的：请提供一个非空的值。", f.name));
        }
    }
    Ok(Value::Object(out))
}

/// Declared fields only; an absent key takes the field's default (cleaned), else `null`. Explicit `null` stays `null`.
pub fn project(def: &CollectionDef, doc: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for f in &def.fields {
        let v = match (doc.get(&f.name), &f.default) {
            (Some(v), _) => v.clone(),
            (None, Some(d)) => check_value(f, d).unwrap_or(Value::Null),
            (None, None) => Value::Null,
        };
        out.insert(f.name.clone(), v);
    }
    Value::Object(out)
}

pub fn strip_tags(html: &str) -> String {
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

/// Brings a stored document in line with a changed definition: converts values whose field changed type,
/// keeps values that are still valid under a changed field of the same type, fills the default of a newly
/// added field, and fails naming the field that cannot follow.
pub fn migrate(old: &CollectionDef, new: &CollectionDef, doc: &Value) -> Result<Value, String> {
    let mut doc = doc.clone();
    for f in &new.fields {
        // An absent value takes the field's default (new field, or a default added later), so queries see it too.
        if let (Some(d), Some(map)) = (&f.default, doc.as_object_mut()) {
            if !map.contains_key(&f.name) {
                map.insert(f.name.clone(), check_value(f, d)?);
                continue;
            }
        }
        let Some(o) = old.field(&f.name) else { continue };
        if (o.kind, o.multiple, &o.options) == (f.kind, f.multiple, &f.options) { continue; }
        let Some(v) = doc.get(&f.name).filter(|v| !v.is_null()).cloned() else { continue };
        let cannot = || format!("字段“{}”的现有值 {} 无法转换成新的类型或选项：请先修改或清空这个值，再改字段定义。", f.name, show(&v));
        // A change of type always goes through the conversion rules: plain text must be escaped on its way to
        // rich text, never read as HTML.
        let fixed = if o.kind != f.kind {
            convert(&v, o, f).ok_or_else(cannot)?
        } else {
            match check_value(f, &v) {
                Ok(c) => c,
                Err(_) => convert(&v, o, f).ok_or_else(cannot)?,
            }
        };
        doc[&f.name] = fixed;
    }
    normalize(new, &doc, &serde_json::json!({}))
}

/// Fills every declared field that is absent from `doc` with its default (cleaned). Explicit `null` stays.
pub fn fill_defaults(def: &CollectionDef, doc: Value) -> Result<Value, String> {
    let Value::Object(mut map) = doc else { return Ok(doc) };
    for f in &def.fields {
        if let (Some(d), false) = (&f.default, map.contains_key(&f.name)) {
            map.insert(f.name.clone(), check_value(f, d)?);
        }
    }
    Ok(Value::Object(map))
}

/// Validates a whole stored document (for example an old version being restored) against `def`: every
/// declared field is checked and cleaned, undeclared keys are kept as they are.
pub fn fit(def: &CollectionDef, doc: &Value) -> Result<Value, String> {
    let map = doc.as_object().ok_or("要恢复的内容不是 JSON 对象，无法恢复。")?;
    let (declared, rest): (serde_json::Map<String, Value>, serde_json::Map<String, Value>) =
        map.iter().map(|(k, v)| (k.clone(), v.clone())).partition(|(k, _)| def.field(k).is_some());
    normalize(def, &Value::Object(rest), &Value::Object(declared))
}

pub const MAX_NOTES: usize = 20_000;

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Action { Create, Edit, Delete }

/// A declared admin screen over one collection.
#[derive(Serialize, Deserialize, JsonSchema, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct ViewDef {
    pub name: String,
    pub title: String,
    pub collection: String,
    pub columns: Vec<String>,
    #[serde(default)]
    pub filters: Vec<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub actions: Vec<Action>,
    /// How rows are drawn: a table (default), a grid of cards, or a plain list.
    #[serde(default, skip_serializing_if = "is_default")]
    pub layout: Layout,
    /// Cards and lists: the field shown as each item's title (default: the first column).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle_field: Option<String>,
    /// Cards: a media field shown as the picture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_field: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub density: Density,
    /// An indexed select field: one section per option, in the options' order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<String>,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Default, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    #[default]
    Table,
    Cards,
    List,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Default, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
}

pub fn check_view(view: &ViewDef, def: &CollectionDef) -> Result<(), String> {
    let name = &view.name;
    if !valid_name(name) {
        return Err(format!("界面名“{name}”不合法：只能用小写字母开头，后接小写字母、数字或下划线，最长 40 个字符，且不能是保留字（id、sort、limit、offset、expand）。"));
    }
    if view.title.trim().is_empty() {
        return Err(format!("界面“{name}”的 title 不能为空：请写一个给人看的标题。"));
    }
    if view.collection != def.name {
        return Err(format!("界面“{name}”的 collection 是“{}”，却在按集合“{}”检查：请检查集合名。", view.collection, def.name));
    }
    if view.columns.is_empty() {
        return Err(format!("界面“{name}”的 columns 不能为空：请至少列出一列（id 或集合的字段名）。"));
    }
    let known = |f: &str| f == "id" || def.field(f).is_some();
    let indexed = |f: &str| f == "id" || def.field(f).is_some_and(|f| f.index || f.unique);
    for (list, label) in [(&view.columns, "columns"), (&view.filters, "filters")] {
        for (i, f) in list.iter().enumerate() {
            if list[..i].contains(f) {
                return Err(format!("界面“{name}”的 {label} 里“{f}”重复了：请去掉重复的一项。"));
            }
        }
    }
    for f in &view.columns {
        if !known(f) {
            return Err(format!("界面“{name}”的 columns 里的“{f}”不是集合“{}”的字段：请用 id 或已声明的字段名。", def.name));
        }
    }
    let sort = view.sort.as_deref().map(|s| s.strip_prefix('-').unwrap_or(s));
    for (f, label) in view.filters.iter().map(|f| (f.as_str(), "filters")).chain(sort.map(|s| (s, "sort"))) {
        if !indexed(f) {
            return Err(format!("界面“{name}”的 {label} 里的“{f}”不能用：只有 id 和设置了 index 或 unique 的字段可以过滤、排序，请先给字段加上 index。"));
        }
    }
    for (f, label) in [(&view.title_field, "title_field"), (&view.subtitle_field, "subtitle_field")] {
        if let Some(f) = f.as_deref().filter(|f| !known(f)) {
            return Err(format!("界面“{name}”的 {label}“{f}”不是集合“{}”的字段。", def.name));
        }
    }
    if let Some(f) = view.image_field.as_deref() {
        if def.field(f).map(|d| d.kind) != Some(FieldType::Media) {
            return Err(format!("界面“{name}”的 image_field“{f}”要是集合“{}”里的 media（图片）字段。", def.name));
        }
    }
    if let Some(f) = view.group_by.as_deref() {
        let ok = def.field(f).is_some_and(|d| d.kind == FieldType::Select && (d.index || d.unique) && !d.multiple);
        if !ok {
            return Err(format!("界面“{name}”的 group_by“{f}”要是一个设置了 index 的单选（select）字段。"));
        }
    }
    Ok(())
}
