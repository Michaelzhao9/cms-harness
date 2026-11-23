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

// Task 3 replaces this with full single-value validation; for now only the JSON type is checked.
fn check_default(f: &FieldDef, d: &Value) -> Result<(), String> {
    let ok = |v: &Value| match f.kind {
        FieldType::Number => v.is_number(),
        FieldType::Boolean => v.is_boolean(),
        _ => v.is_string(),
    };
    let fits = match d {
        Value::Array(items) if f.multiple => items.iter().all(ok),
        _ => !f.multiple && ok(d),
    };
    if fits {
        Ok(())
    } else {
        Err(format!("字段“{}”的默认值类型不对：默认值必须和字段类型一致（多值字段用数组）。", f.name))
    }
}
