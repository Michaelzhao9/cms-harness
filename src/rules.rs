//! Access rules: who may change or remove a record from the site. A rule is one of four words (`public`,
//! `members`, `owner`, `none`) or a small expression over the record, the member and what the request would write:
//!
//! ```text
//! record.owner == member.id and data.stars == record.stars
//! ```
//!
//! `record.<field>`, `record.id`, `record.owner`; `member` (logged in), `member.id`, `member.email`, `member.name`;
//! `data.<field>`; numbers, 'text', true, false, null; `== != < <= > >=`; `and or not` (also `&& || !`); brackets.
//!
//! A rule is compiled to one SQL expression and decided by the database, so the same text can later filter a
//! list. Everything from outside (text in the rule, the member, the data) reaches SQL as a bound value. Unknown
//! is never allowed: a visitor has no id, so `record.owner == member.id` is not true for a record nobody owns.

use crate::members::Member;
use crate::schema::CollectionDef;
use rusqlite::{params_from_iter, types::Value as Sql, Connection, OptionalExtension};
use serde_json::Value;

pub const MAX_LEN: usize = 500;

const PATHS: &str = "record.字段、record.id、record.owner、data.字段、member、member.id、member.email、member.name";

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Num(String),
    Text(String),
    /// An operator or bracket, as written.
    Sym(&'static str),
}

impl Tok {
    fn shown(&self) -> String {
        match self {
            Tok::Word(w) => w.clone(),
            Tok::Num(n) => n.clone(),
            Tok::Text(t) => format!("'{t}'"),
            Tok::Sym(s) => s.to_string(),
        }
    }
}

const SYMS: [&str; 13] = ["==", "!=", "<=", ">=", "&&", "||", "<", ">", "=", "!", "(", ")", "."];

fn tokens(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(Tok::Word(chars[start..i].iter().collect()));
        } else if c.is_ascii_digit() || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || (chars[i] == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit))) {
                i += 1;
            }
            out.push(Tok::Num(chars[start..i].iter().collect()));
        } else if c == '\'' || c == '"' {
            let mut text = String::new();
            i += 1;
            loop {
                match chars.get(i) {
                    None => return Err(format!("引号 {c} 没有闭合。")),
                    Some(&q) if q == c => break,
                    Some('\\') if chars.get(i + 1).is_some() => {
                        text.push(chars[i + 1]);
                        i += 2;
                    }
                    Some(&ch) => {
                        text.push(ch);
                        i += 1;
                    }
                }
            }
            i += 1;
            out.push(Tok::Text(text));
        } else {
            let rest: String = chars[i..chars.len().min(i + 2)].iter().collect();
            let Some(sym) = SYMS.iter().find(|s| rest.starts_with(**s)) else {
                return Err(format!("规则里有不认识的字符“{c}”。"));
            };
            out.push(Tok::Sym(sym));
            i += sym.len();
        }
    }
    Ok(out)
}

/// A compiled piece: SQL that is a truth value, SQL that is a value, or the word `null`.
enum Expr {
    Truth(String),
    Value(String),
    Null,
}

/// A rule as SQL over a row of `records`, with its bound values.
pub struct Compiled {
    pub sql: String,
    pub params: Vec<Sql>,
}

struct Compiler<'a> {
    def: &'a CollectionDef,
    toks: Vec<Tok>,
    at: usize,
    params: Vec<Sql>,
    member: Option<&'a Member>,
    /// What the request would write, as JSON text.
    data: Option<String>,
}

impl Compiler<'_> {
    fn bind(&mut self, v: Sql) -> String {
        self.params.push(v);
        format!("?{}", self.params.len())
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn eat_sym(&mut self, syms: &[&str]) -> Option<&'static str> {
        match self.peek() {
            Some(Tok::Sym(s)) if syms.contains(s) => {
                let s = *s;
                self.at += 1;
                Some(s)
            }
            _ => None,
        }
    }

    fn eat_word(&mut self, word: &str) -> bool {
        let hit = matches!(self.peek(), Some(Tok::Word(w)) if w == word);
        self.at += hit as usize;
        hit
    }

    fn truth(e: Expr) -> String {
        match e {
            Expr::Truth(s) => s,
            Expr::Value(s) => format!("({s} IS NOT NULL AND {s} IS NOT 0 AND {s} IS NOT '')"),
            Expr::Null => "NULL".into(),
        }
    }

    fn or(&mut self) -> Result<Expr, String> {
        let mut left = self.and()?;
        while self.eat_word("or") || self.eat_sym(&["||"]).is_some() {
            let right = self.and()?;
            left = Expr::Truth(format!("({} OR {})", Self::truth(left), Self::truth(right)));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut left = self.not()?;
        while self.eat_word("and") || self.eat_sym(&["&&"]).is_some() {
            let right = self.not()?;
            left = Expr::Truth(format!("({} AND {})", Self::truth(left), Self::truth(right)));
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<Expr, String> {
        if self.eat_word("not") || self.eat_sym(&["!"]).is_some() {
            let inner = self.not()?;
            return Ok(Expr::Truth(format!("(NOT {})", Self::truth(inner))));
        }
        self.compare()
    }

    fn compare(&mut self) -> Result<Expr, String> {
        let left = self.operand(None)?;
        let Some(op) = self.eat_sym(&["==", "!=", "<=", ">=", "<", ">", "="]) else { return Ok(left) };
        let right = self.operand(Some(op))?;
        let sql = match (left, right, op) {
            (Expr::Null, Expr::Null, "==" | "=") => "1".to_string(),
            (Expr::Null, Expr::Null, "!=") => "0".to_string(),
            (Expr::Null, other, "==" | "=") | (other, Expr::Null, "==" | "=") => format!("({} IS NULL)", Self::value(other)),
            (Expr::Null, other, "!=") | (other, Expr::Null, "!=") => format!("({} IS NOT NULL)", Self::value(other)),
            (Expr::Null, _, _) | (_, Expr::Null, _) => return Err(format!("null 不能用“{op}”比大小：只能用 == 或 !=。")),
            (l, r, _) => {
                let op = match op {
                    "==" => "=",
                    "!=" => "<>",
                    o => o,
                };
                format!("({} {op} {})", Self::value(l), Self::value(r))
            }
        };
        Ok(Expr::Truth(sql))
    }

    fn value(e: Expr) -> String {
        match e {
            Expr::Truth(s) | Expr::Value(s) => s,
            Expr::Null => "NULL".into(),
        }
    }

    /// One value. `after` is the operator it follows, for the message when it is missing.
    fn operand(&mut self, after: Option<&str>) -> Result<Expr, String> {
        let Some(tok) = self.peek().cloned() else {
            return Err(match after {
                Some(op) => format!("“{op}”后面缺少要比较的值。"),
                None => "规则没有写完：最后缺少一个条件。".into(),
            });
        };
        self.at += 1;
        match tok {
            Tok::Sym("(") => {
                let inner = self.or()?;
                if self.eat_sym(&[")"]).is_none() {
                    return Err("括号没有闭合：缺少“)”。".into());
                }
                Ok(inner)
            }
            Tok::Num(n) => {
                let v = n.parse::<i64>().map(Sql::Integer).or_else(|_| n.parse::<f64>().map(Sql::Real)).map_err(|_| format!("“{n}”不是一个数字。"))?;
                Ok(Expr::Value(self.bind(v)))
            }
            Tok::Text(t) => Ok(Expr::Value(self.bind(Sql::Text(t)))),
            Tok::Word(w) => self.word(&w),
            Tok::Sym(s) => Err(format!("“{s}”出现在不该出现的地方：这里要的是一个值，比如 record.字段、member.id 或一个数字。")),
        }
    }

    fn word(&mut self, w: &str) -> Result<Expr, String> {
        let field = if self.eat_sym(&["."]).is_some() {
            match self.peek().cloned() {
                Some(Tok::Word(f)) => {
                    self.at += 1;
                    Some(f)
                }
                _ => return Err(format!("“{w}.”后面缺少名字。")),
            }
        } else {
            None
        };
        let declared = |def: &CollectionDef, f: &str| def.field(f).is_some() && crate::schema::valid_name(f);
        match (w, field.as_deref()) {
            ("true", None) => Ok(Expr::Value("1".into())),
            ("false", None) => Ok(Expr::Value("0".into())),
            ("null", None) => Ok(Expr::Null),
            ("member", None | Some("id")) => {
                let id = self.member.map_or(Sql::Null, |m| Sql::Integer(m.id));
                Ok(Expr::Value(self.bind(id)))
            }
            ("member", Some(f @ ("email" | "name"))) => {
                let v = self.member.map_or(Sql::Null, |m| Sql::Text(if f == "email" { m.email.clone() } else { m.name.clone() }));
                Ok(Expr::Value(self.bind(v)))
            }
            ("member", Some(f)) => Err(format!("member 没有“{f}”：能用的是 member.id、member.email、member.name。")),
            ("record", Some("id")) => Ok(Expr::Value("id".into())),
            ("record", Some("owner")) if self.def.field("owner").is_none() => Ok(Expr::Value("owner".into())),
            ("record", Some("owner")) => Err(format!(
                "集合“{}”有一个叫 owner 的字段，而规则里的 record.owner 指的是提交这条记录的会员，两者分不清：请给字段换个名字。",
                self.def.name
            )),
            ("record", Some(f)) if declared(self.def, f) => Ok(Expr::Value(format!("json_extract(published,'$.{f}')"))),
            ("data", Some(f)) if declared(self.def, f) => {
                let data = self.data.clone().map_or(Sql::Null, Sql::Text);
                let p = self.bind(data);
                Ok(Expr::Value(format!("json_extract({p},'$.{f}')")))
            }
            ("record" | "data", Some(f)) => Err(format!("规则里的 {w}.{f}：集合“{}”没有字段“{f}”。", self.def.name)),
            ("record" | "data", None) => Err(format!("“{w}”后面要跟字段名，比如 {w}.{}。", self.def.fields.first().map_or("title", |f| f.name.as_str()))),
            _ => Err(format!("规则里不认识“{}”：能用的是 {PATHS}。", field.map_or(w.to_string(), |f| format!("{w}.{f}")))),
        }
    }
}

/// `rule` as SQL for `member` and the document the request would write.
pub fn compile(def: &CollectionDef, rule: &str, member: Option<&Member>, data: Option<&Value>) -> Result<Compiled, String> {
    let rule = rule.trim();
    if rule.is_empty() {
        return Err("规则是空的：写 public、members、owner、none，或一个条件，比如 record.owner == member.id。".into());
    }
    if rule.chars().count() > MAX_LEN {
        return Err(format!("规则太长了：最多 {MAX_LEN} 个字符。"));
    }
    let id = member.map_or(Sql::Null, |m| Sql::Integer(m.id));
    let word = |sql: &str, params: Vec<Sql>| Ok(Compiled { sql: sql.into(), params });
    match rule {
        "public" => return word("1", vec![]),
        "none" => return word("0", vec![]),
        "members" => return word("(?1 IS NOT NULL)", vec![id]),
        // The column, not `record.owner`: a collection may have a field of that name.
        "owner" => return word("(owner = ?1)", vec![id]),
        _ => {}
    }
    let mut c = Compiler { def, toks: tokens(rule)?, at: 0, params: vec![], member, data: data.map(Value::to_string) };
    let sql = Compiler::truth(c.or()?);
    if let Some(extra) = c.peek() {
        return Err(match extra {
            Tok::Sym(")") => "多了一个“)”。".into(),
            t => format!("“{}”前面缺少 and 或 or。", t.shown()),
        });
    }
    Ok(Compiled { sql, params: c.params })
}

/// Whether `rule` can be kept on `def`: it parses and names only what exists.
pub fn check(def: &CollectionDef, rule: &str) -> Result<(), String> {
    compile(def, rule, None, None).map(|_| ())
}

/// Whether `rule` lets `member` (none: a visitor) act on live record `id` of `def`, when the request would
/// leave the record as `data`. A record that is not there or not live allows nothing.
pub fn allows(conn: &Connection, def: &CollectionDef, rule: &str, member: Option<&Member>, id: i64, data: Option<&Value>) -> Result<bool, String> {
    let Compiled { sql, mut params } = compile(def, rule, member, data)?;
    params.push(Sql::Integer(id));
    let at = params.len();
    params.push(Sql::Text(def.name.clone()));
    let sql = format!("SELECT ({sql}) IS TRUE FROM records WHERE id = ?{at} AND collection = ?{} AND published IS NOT NULL", at + 1);
    let hit: Option<bool> = conn.query_row(&sql, params_from_iter(params), |r| r.get(0)).optional().map_err(|e| format!("检查规则失败：{e}"))?;
    Ok(hit.unwrap_or(false))
}
