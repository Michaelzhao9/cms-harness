//! 防护: the kernel guards everything the environment serves, whoever built it and however. A page the AI wrote
//! a minute ago is behind the same door as the rest, and nothing a site file, a tool call or a setting can open
//! it. It has no page in the admin and nothing to switch: the level is `Config::default()` below, and changing
//! it means changing this file.
//!
//! Four things, cheapest first, all decided in memory before a request reaches a handler:
//! - the block list, and clients shut out for a while (a scanner's path, or going over the limit again and again);
//! - a count of requests per client per minute;
//! - crawlers that say who they are: search engines pass, AI crawlers and scripting tools do not get pages;
//! - a proof-of-work challenge for a client that reads page after page (or for every visitor, in strict mode):
//!   a real browser pays a fraction of a second once, a crawler pays it on every address it comes from.
//!
//! What it cannot do: a flood from thousands of addresses has to be stopped upstream (the host, a CDN), and a
//! crawler driving real browsers through many addresses gets through, slower and at a cost.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS shield_events(id INTEGER PRIMARY KEY, at TEXT NOT NULL DEFAULT (datetime('now')), client TEXT NOT NULL,
  kind TEXT NOT NULL, path TEXT NOT NULL, ua TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS shield_events_at ON shield_events(at);
";

/// Leading zero bits a challenge's hash needs: about 65,000 hashes, a fraction of a second in a browser.
pub const BITS: u32 = 16;
/// How long a solved challenge is good for.
pub const PASS_SECONDS: u64 = 12 * 3600;
pub const COOKIE: &str = "hc_pass";

const WINDOW: Duration = Duration::from_secs(60);
/// Windows over the limit before a client is shut out, and for how long.
const STRIKES: u32 = 3;
const SHUT_OUT: Duration = Duration::from_secs(10 * 60);
const PROBE_SHUT_OUT: Duration = Duration::from_secs(60 * 60);
/// Events kept: a week, and no more than this many a minute however many clients misbehave.
const EVENTS_A_MINUTE: u32 = 60;
// ponytail: every client seen in the last ten minutes is one small entry; past this many the newest go untracked
// rather than the map growing without bound. A front proxy that limits connections is the upgrade path.
const MAX_CLIENTS: usize = 200_000;

#[derive(Serialize, Deserialize, Clone, Copy, Default, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    #[default]
    Standard,
    /// Every visitor solves the challenge before the first page.
    Strict,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(default)]
pub struct Config {
    pub mode: Mode,
    /// Requests one client may make in a minute.
    pub per_minute: u32,
    /// Pages one client may open in a minute before it is challenged.
    pub pages_per_minute: u32,
    /// Refuse pages to crawlers that collect text for AI models.
    pub block_ai: bool,
    /// Refuse pages to scripting tools and headless browsers.
    pub block_tools: bool,
    /// Client addresses that get nothing.
    pub blocked: Vec<String>,
}

impl Default for Config {
    /// The level every deployment runs at. This is the one place to change it.
    fn default() -> Self {
        Config { mode: Mode::Standard, per_minute: 240, pages_per_minute: 40, block_ai: true, block_tools: true, blocked: vec![] }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    /// Over the limit: try again in a minute.
    Limited,
    /// Refused, and why (an event kind).
    Blocked(&'static str),
    /// Solve the proof of work first.
    Challenge,
}

/// Crawlers that collect pages for AI models, by the name they give. Also listed in `robots.txt`.
pub const AI_CRAWLERS: [&str; 16] = [
    "GPTBot", "OAI-SearchBot", "ClaudeBot", "Claude-Web", "anthropic-ai", "CCBot", "Bytespider", "PerplexityBot", "Amazonbot", "meta-externalagent",
    "Diffbot", "cohere-ai", "omgili", "ImagesiftBot", "YouBot", "Timpibot",
];
const SEARCH_ENGINES: [&str; 9] = ["Googlebot", "bingbot", "Baiduspider", "YandexBot", "DuckDuckBot", "Sogou", "360Spider", "Applebot", "Slurp"];
const TOOLS: [&str; 16] = [
    "python-requests", "python-urllib", "aiohttp", "httpx", "Scrapy", "curl/", "Wget", "Go-http-client", "libwww-perl", "Java/", "node-fetch", "axios/",
    "HeadlessChrome", "PhantomJS", "Puppeteer", "Playwright",
];
/// Paths only a vulnerability scanner asks this server for.
const PROBES: [&str; 14] = [
    ".php", "/.env", "/.git", "/.aws", "/.ssh", "/wp-admin", "/wp-content", "/wp-includes", "/phpmyadmin", "/cgi-bin/", "/actuator/", "/.DS_Store", "/xmlrpc",
    "/etc/passwd",
];

fn named(ua: &str, names: &[&str]) -> bool {
    let ua = ua.to_ascii_lowercase();
    names.iter().any(|n| ua.contains(&n.to_ascii_lowercase()))
}

#[derive(Clone, Copy)]
struct Client {
    since: Instant,
    seen: Instant,
    /// The last event written for this client: one of a kind a minute is enough.
    logged: Option<(&'static str, Instant)>,
    hits: u32,
    pages: u32,
    strikes: u32,
    out_until: Option<Instant>,
    /// The window was already counted as one over the limit.
    struck: bool,
}

struct State {
    config: Config,
    clients: HashMap<String, Client>,
    swept: Option<Instant>,
    /// Events written this minute.
    logged: (Option<Instant>, u32),
}

pub struct Shield {
    state: Mutex<State>,
    /// Signs challenges. New on every start: a restart asks browsers to solve it once more.
    secret: [u8; 32],
}

impl Default for Shield {
    fn default() -> Self {
        Shield { state: Mutex::new(State { config: Config::default(), clients: HashMap::new(), swept: None, logged: (None, 0) }), secret: rand::random() }
    }
}

/// How a request counts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    /// The admin (its own login limit and sessions guard it), preview frames (a token does) and static files.
    Free,
    /// The bearer-key API: programs, in bursts.
    Keyed,
    /// A page a visitor opens.
    Page,
    Other,
}

fn class(method: &str, path: &str) -> Class {
    let under = |p: &str| path == p || path.starts_with(&format!("{p}/"));
    if under("/admin") || under("/_hc") || under("/uploads") {
        Class::Free
    } else if under("/api/v1/tools") || under("/api/v1/site") || under("/api/v1/media") || path == "/mcp" {
        Class::Keyed
    } else if under("/api") || path.split('/').any(|seg| seg == crate::func::DIR) {
        Class::Other
    } else if method == "GET" && path.rsplit('/').next().is_none_or(|last| !last.contains('.') || last.ends_with(".html")) {
        Class::Page
    } else {
        Class::Other
    }
}

impl Shield {
    pub fn config(&self) -> Config {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).config.clone()
    }

    pub fn set_config(&self, config: Config) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).config = config;
    }

    /// What to do with one request. `passed`: it carries a solved challenge.
    pub fn check(&self, client: &str, method: &str, path: &str, ua: &str, passed: bool, now: Instant) -> Verdict {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let kind = class(method, path);
        if st.config.mode == Mode::Off || kind == Class::Free {
            return Verdict::Pass;
        }
        if st.config.blocked.iter().any(|b| b == client) {
            return Verdict::Blocked("listed");
        }
        if st.swept.is_none_or(|s| now.saturating_duration_since(s) > WINDOW) {
            st.swept = Some(now);
            st.clients.retain(|_, c| now.saturating_duration_since(c.seen) < SHUT_OUT.max(PROBE_SHUT_OUT));
        }
        if st.clients.len() >= MAX_CLIENTS && !st.clients.contains_key(client) {
            return Verdict::Pass;
        }
        let (per_minute, pages_per_minute, mode, block_ai, block_tools) = (st.config.per_minute, st.config.pages_per_minute, st.config.mode, st.config.block_ai, st.config.block_tools);
        let c = st.clients.entry(client.to_string()).or_insert(Client { since: now, seen: now, logged: None, hits: 0, pages: 0, strikes: 0, out_until: None, struck: false });
        c.seen = now;
        if c.out_until.is_some_and(|until| now < until) {
            return Verdict::Blocked("out");
        }
        c.out_until = None;
        let lower = path.to_ascii_lowercase();
        if PROBES.iter().any(|p| if *p == ".php" { lower.ends_with(p) } else { lower.contains(p) }) {
            c.out_until = Some(now + PROBE_SHUT_OUT);
            return Verdict::Blocked("probe");
        }
        if now.saturating_duration_since(c.since) >= WINDOW {
            (c.since, c.hits, c.pages, c.struck) = (now, 0, 0, false);
        }
        c.hits += 1;
        let limit = if kind == Class::Keyed { per_minute.saturating_mul(5) } else { per_minute };
        if c.hits > limit {
            if !c.struck {
                c.struck = true;
                c.strikes += 1;
                if c.strikes >= STRIKES {
                    (c.strikes, c.out_until) = (0, Some(now + SHUT_OUT));
                    return Verdict::Blocked("flood");
                }
            }
            return Verdict::Limited;
        }
        if kind != Class::Page {
            return Verdict::Pass;
        }
        if block_ai && named(ua, &AI_CRAWLERS) {
            return Verdict::Blocked("ai");
        }
        if block_tools && named(ua, &TOOLS) {
            return Verdict::Blocked("tool");
        }
        if passed || named(ua, &SEARCH_ENGINES) {
            return Verdict::Pass;
        }
        c.pages += 1;
        if mode == Mode::Strict || c.pages > pages_per_minute {
            return Verdict::Challenge;
        }
        Verdict::Pass
    }

    /// Whether to write down that `client` was stopped for `kind`: once a minute for each client and kind, and
    /// only so many a minute in all.
    pub fn may_log(&self, client: &str, kind: &'static str, now: Instant) -> bool {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = st.clients.get_mut(client) {
            if c.logged.is_some_and(|(k, at)| k == kind && now.saturating_duration_since(at) < WINDOW) {
                return false;
            }
            c.logged = Some((kind, now));
        }
        if st.logged.0.is_none_or(|s| now.saturating_duration_since(s) >= WINDOW) {
            st.logged = (Some(now), 0);
        }
        st.logged.1 += 1;
        st.logged.1 <= EVENTS_A_MINUTE
    }

    fn sign(&self, text: &str) -> String {
        // HMAC-SHA256.
        let (mut inner, mut outer) = ([0x36u8; 64], [0x5cu8; 64]);
        for (i, k) in self.secret.iter().enumerate() {
            inner[i] ^= k;
            outer[i] ^= k;
        }
        let first = Sha256::new().chain_update(inner).chain_update(text.as_bytes()).finalize();
        hex::encode(&Sha256::new().chain_update(outer).chain_update(first).finalize()[..16])
    }

    fn who(client: &str, ua: &str) -> String {
        hex::encode(&Sha256::digest(format!("{client}\n{ua}"))[..8])
    }

    /// A challenge for this client and browser: `<expires>.<who>.<signature>`. The browser finds a number that
    /// gives `sha256("<seed>.<number>")` its leading zero bits, and sends both back as the pass.
    pub fn issue(&self, client: &str, ua: &str, now_unix: u64) -> String {
        let body = format!("{}.{}", now_unix + PASS_SECONDS, Self::who(client, ua));
        format!("{body}.{}", self.sign(&body))
    }

    /// Whether `pass` (`<seed>.<number>`) is a challenge we issued to this client, solved and not expired.
    pub fn passes(&self, pass: &str, client: &str, ua: &str, now_unix: u64) -> bool {
        let parts: Vec<&str> = pass.split('.').collect();
        let [expires, who, signature, number] = parts.as_slice() else { return false };
        let body = format!("{expires}.{who}");
        expires.parse::<u64>().is_ok_and(|e| e > now_unix)
            && *who == Self::who(client, ua)
            && *signature == self.sign(&body)
            && number.len() <= 20
            && number.bytes().all(|b| b.is_ascii_digit())
            && leading_zero_bits(&Sha256::digest(format!("{body}.{signature}.{number}"))) >= BITS
    }
}

pub fn leading_zero_bits(hash: &[u8]) -> u32 {
    let mut bits = 0;
    for b in hash {
        bits += b.leading_zeros();
        if *b != 0 {
            break;
        }
    }
    bits
}

/// Records one stopped request; a week of them is kept.
pub fn log(conn: &Connection, client: &str, kind: &str, path: &str, ua: &str) {
    let short = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    let _ = conn.execute("INSERT INTO shield_events(client, kind, path, ua) VALUES (?1, ?2, ?3, ?4)", params![client, kind, short(path, 200), short(ua, 200)]);
    let _ = conn.execute("DELETE FROM shield_events WHERE at < datetime('now', '-7 days')", []);
}

/// The newest stopped requests.
pub fn events(conn: &Connection, limit: i64) -> Result<Vec<Value>, String> {
    let fail = |e: rusqlite::Error| format!("读取防护记录失败：{e}");
    let mut st = conn.prepare("SELECT at, client, kind, path, ua FROM shield_events ORDER BY id DESC LIMIT ?1").map_err(fail)?;
    let rows = st
        .query_map([limit], |r| Ok(json!({ "at": r.get::<_, String>(0)?, "client": r.get::<_, String>(1)?, "kind": r.get::<_, String>(2)?, "path": r.get::<_, String>(3)?, "ua": r.get::<_, String>(4)? })))
        .map_err(fail)?;
    rows.collect::<Result<_, _>>().map_err(fail)
}

/// How many requests of each kind were stopped in the last day.
pub fn counts(conn: &Connection) -> Result<Value, String> {
    let fail = |e: rusqlite::Error| format!("读取防护记录失败：{e}");
    let mut st = conn.prepare("SELECT kind, count(*) FROM shield_events WHERE at >= datetime('now', '-1 day') GROUP BY kind").map_err(fail)?;
    let rows: Vec<(String, i64)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(fail)?.collect::<Result<_, _>>().map_err(fail)?;
    Ok(rows.into_iter().map(|(k, n)| (k, json!(n))).collect::<serde_json::Map<_, _>>().into())
}

/// `robots.txt` lines that turn AI crawlers away, for the ones that ask first.
pub fn robots(config: &Config) -> String {
    if config.mode == Mode::Off || !config.block_ai {
        return String::new();
    }
    AI_CRAWLERS.iter().map(|b| format!("User-agent: {b}\nDisallow: /\n\n")).collect()
}

/// The script of the challenge page: finds the number, keeps the pass in a cookie, asks for the page again.
/// SHA-256 is written out because `crypto.subtle` is missing on plain http.
pub const SCRIPT: &str = r#"(function () {
  var el = document.currentScript, seed = el.getAttribute('data-seed'), bits = +el.getAttribute('data-bits');
  var K = new Int32Array(64), H0 = new Int32Array(8), n = 2, i = 0, d, p;
  function frac(x) { return ((x - Math.floor(x)) * 4294967296) | 0; }
  for (; i < 64; n++) { p = true; for (d = 2; d * d <= n; d++) if (n % d === 0) { p = false; break; } if (p) { if (i < 8) H0[i] = frac(Math.pow(n, 1 / 2)); K[i++] = frac(Math.pow(n, 1 / 3)); } }
  // One buffer for every try: only the number at the end changes.
  var w = new Int32Array(64), h = new Int32Array(8), buf = new Uint8Array(256), prefix = seed + '.', pl = prefix.length;
  for (i = 0; i < pl; i++) buf[i] = prefix.charCodeAt(i) & 255;
  // The leading zero bits of sha256(prefix + k).
  function zeros(k) {
    var s = String(k), l = pl + s.length, total = ((l + 72) >> 6) << 6, j, o, x, y, t1, t2, a, b, c, dd, e, f, g, hh;
    for (j = 0; j < s.length; j++) buf[pl + j] = s.charCodeAt(j);
    buf[l] = 128;
    for (j = l + 1; j < total; j++) buf[j] = 0;
    buf[total - 4] = (l * 8) >>> 24; buf[total - 3] = (l * 8) >>> 16 & 255; buf[total - 2] = (l * 8) >>> 8 & 255; buf[total - 1] = (l * 8) & 255;
    for (j = 0; j < 8; j++) h[j] = H0[j];
    for (o = 0; o < total; o += 64) {
      for (j = 0; j < 16; j++) w[j] = buf[o + j * 4] << 24 | buf[o + j * 4 + 1] << 16 | buf[o + j * 4 + 2] << 8 | buf[o + j * 4 + 3];
      for (; j < 64; j++) { x = w[j - 15]; y = w[j - 2]; w[j] = (w[j - 16] + ((x >>> 7 | x << 25) ^ (x >>> 18 | x << 14) ^ x >>> 3) + w[j - 7] + ((y >>> 17 | y << 15) ^ (y >>> 19 | y << 13) ^ y >>> 10)) | 0; }
      a = h[0]; b = h[1]; c = h[2]; dd = h[3]; e = h[4]; f = h[5]; g = h[6]; hh = h[7];
      for (j = 0; j < 64; j++) {
        t1 = (hh + ((e >>> 6 | e << 26) ^ (e >>> 11 | e << 21) ^ (e >>> 25 | e << 7)) + (e & f ^ ~e & g) + K[j] + w[j]) | 0;
        t2 = (((a >>> 2 | a << 30) ^ (a >>> 13 | a << 19) ^ (a >>> 22 | a << 10)) + (a & b ^ a & c ^ b & c)) | 0;
        hh = g; g = f; f = e; e = (dd + t1) | 0; dd = c; c = b; b = a; a = (t1 + t2) | 0;
      }
      h[0] += a; h[1] += b; h[2] += c; h[3] += dd; h[4] += e; h[5] += f; h[6] += g; h[7] += hh;
    }
    for (x = 0, j = 0; j < 8; j++) { if (h[j] === 0) { x += 32; continue; } return x + Math.clz32(h[j]); }
    return x;
  }
  var k = 0;
  // In slices of a fifth of a second: a hidden tab runs timers once a second, so each slice has to count.
  function work() {
    var stop = Date.now() + 200, end;
    do {
      for (end = k + 2000; k < end; k++) if (zeros(k) >= bits) {
        document.cookie = 'hc_pass=' + seed + '.' + k + '; Path=/; Max-Age=43200; SameSite=Lax';
        location.reload();
        return;
      }
    } while (Date.now() < stop);
    setTimeout(work, 0);
  }
  setTimeout(work, 30);
})();
"#;

/// The page a challenged visitor gets. Plain text in two languages: it is shown before the site's own pages.
pub fn challenge_page(seed: &str) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"robots\" content=\"noindex\"><title>…</title>\
         <body style=\"margin:0;min-height:100vh;display:grid;place-items:center;font:16px/1.6 system-ui,-apple-system,'PingFang SC',sans-serif;color:#1d1d1f;background:#f5f5f7\">\
         <main style=\"max-width:22em;padding:24px;text-align:center\"><p>正在确认这是一次正常的访问，马上就好。</p><p style=\"color:#6e6e73;font-size:14px\">Checking that this is a normal visit. One moment.</p>\
         <noscript><p>这一步需要浏览器运行 JavaScript。<br>This step needs JavaScript.</p></noscript></main>\
         <script src=\"/_hc/shield.js\" data-seed=\"{seed}\" data-bits=\"{BITS}\"></script>"
    )
}
