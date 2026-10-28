//! Minimal Redis client (RESP over TCP) powering the built-in Redis browser.

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

pub enum Reply {
    Val(Value),
    Err(String),
}

struct Conn {
    r: BufReader<TcpStream>,
}

fn e2s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn val(r: Reply) -> Result<Value, String> {
    match r {
        Reply::Val(v) => Ok(v),
        Reply::Err(e) => Err(e),
    }
}

impl Conn {
    fn open(db: u8) -> Result<Self, String> {
        let port = crate::manager::service_port("redis");
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().map_err(e2s)?;
        let s = TcpStream::connect_timeout(&addr, Duration::from_millis(1500))
            .map_err(|_| "Redis is not running. Start it from the Dashboard first.".to_string())?;
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
        let _ = s.set_nodelay(true);
        let mut c = Conn { r: BufReader::new(s) };
        if db != 0 {
            val(c.cmd(&["SELECT", &db.to_string()])?)?;
        }
        Ok(c)
    }

    fn encode<S: AsRef<str>>(buf: &mut Vec<u8>, args: &[S]) {
        buf.extend(format!("*{}\r\n", args.len()).as_bytes());
        for a in args {
            let a = a.as_ref();
            buf.extend(format!("${}\r\n", a.len()).as_bytes());
            buf.extend(a.as_bytes());
            buf.extend(b"\r\n");
        }
    }

    fn cmd<S: AsRef<str>>(&mut self, args: &[S]) -> Result<Reply, String> {
        let mut buf = Vec::new();
        Self::encode(&mut buf, args);
        self.r.get_mut().write_all(&buf).map_err(e2s)?;
        read_reply(&mut self.r).map_err(e2s)
    }

    fn pipeline(&mut self, cmds: &[Vec<String>]) -> Result<Vec<Reply>, String> {
        let mut buf = Vec::new();
        for c in cmds {
            Self::encode(&mut buf, c);
        }
        self.r.get_mut().write_all(&buf).map_err(e2s)?;
        let mut out = Vec::with_capacity(cmds.len());
        for _ in cmds {
            out.push(read_reply(&mut self.r).map_err(e2s)?);
        }
        Ok(out)
    }
}

fn read_reply(r: &mut BufReader<TcpStream>) -> io::Result<Reply> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Connection closed by Redis"));
    }
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Empty reply"));
    }
    let (t, rest) = line.split_at(1);
    Ok(match t {
        "+" => Reply::Val(json!(rest)),
        "-" => Reply::Err(rest.to_string()),
        ":" => Reply::Val(json!(rest.parse::<i64>().unwrap_or(0))),
        "$" => {
            let n: i64 = rest.parse().unwrap_or(-1);
            if n < 0 {
                Reply::Val(Value::Null)
            } else {
                let mut buf = vec![0u8; n as usize + 2];
                r.read_exact(&mut buf)?;
                buf.truncate(n as usize);
                Reply::Val(json!(String::from_utf8_lossy(&buf).to_string()))
            }
        }
        "*" => {
            let n: i64 = rest.parse().unwrap_or(-1);
            if n < 0 {
                Reply::Val(Value::Null)
            } else {
                let mut items = Vec::new();
                for _ in 0..n {
                    match read_reply(r)? {
                        Reply::Val(v) => items.push(v),
                        Reply::Err(e) => items.push(json!({ "error": e })),
                    }
                }
                Reply::Val(Value::Array(items))
            }
        }
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "Unexpected reply from Redis")),
    })
}

// ---------------------------------------------------------------- public API

#[derive(Serialize)]
pub struct KeyItem {
    pub key: String,
    pub kind: String,
    pub ttl: i64,
}

#[derive(Serialize)]
pub struct KeyPage {
    pub keys: Vec<KeyItem>,
    pub cursor: String,
    pub dbsize: i64,
}

/// List keys with SCAN (never blocks the server). `limit` is a soft target.
pub fn keys(db: u8, pattern: &str, cursor: &str, limit: usize) -> Result<KeyPage, String> {
    let mut c = Conn::open(db)?;
    let pattern = if pattern.trim().is_empty() { "*" } else { pattern };
    let mut cur = if cursor.is_empty() { "0".to_string() } else { cursor.to_string() };
    let mut found: Vec<String> = vec![];
    let mut seen = HashSet::new();
    for _ in 0..200 {
        let r = val(c.cmd(&["SCAN", &cur, "MATCH", pattern, "COUNT", "500"])?)?;
        let arr = r.as_array().ok_or("Unexpected SCAN reply")?;
        cur = arr.first().and_then(|v| v.as_str()).unwrap_or("0").to_string();
        if let Some(ks) = arr.get(1).and_then(|v| v.as_array()) {
            for k in ks.iter().filter_map(|k| k.as_str()) {
                if seen.insert(k.to_string()) {
                    found.push(k.to_string());
                }
            }
        }
        if cur == "0" || found.len() >= limit {
            break;
        }
    }
    let mut cmds: Vec<Vec<String>> = vec![];
    for k in &found {
        cmds.push(vec!["TYPE".into(), k.clone()]);
        cmds.push(vec!["TTL".into(), k.clone()]);
    }
    let replies = if cmds.is_empty() { vec![] } else { c.pipeline(&cmds)? };
    let mut keys = vec![];
    for (i, k) in found.into_iter().enumerate() {
        let kind = match replies.get(i * 2) {
            Some(Reply::Val(v)) => v.as_str().unwrap_or("?").to_string(),
            _ => "?".into(),
        };
        let ttl = match replies.get(i * 2 + 1) {
            Some(Reply::Val(v)) => v.as_i64().unwrap_or(-1),
            _ => -1,
        };
        keys.push(KeyItem { key: k, kind, ttl });
    }
    let dbsize = val(c.cmd(&["DBSIZE"])?)?.as_i64().unwrap_or(0);
    Ok(KeyPage { keys, cursor: cur, dbsize })
}

#[derive(Serialize)]
pub struct KeyDetail {
    pub kind: String,
    pub ttl: i64,
    pub size: i64,
    pub value: Value,
    pub truncated: bool,
}

fn scan_items(c: &mut Conn, verb: &str, key: &str, max: usize) -> Result<Vec<Value>, String> {
    let mut cur = "0".to_string();
    let mut out = vec![];
    for _ in 0..200 {
        let r = val(c.cmd(&[verb, key, &cur, "COUNT", "200"])?)?;
        let a = r.as_array().ok_or("Unexpected scan reply")?;
        cur = a.first().and_then(|v| v.as_str()).unwrap_or("0").to_string();
        if let Some(items) = a.get(1).and_then(|v| v.as_array()) {
            out.extend(items.iter().cloned());
        }
        if cur == "0" || out.len() >= max {
            break;
        }
    }
    Ok(out)
}

fn pairs(flat: &[Value]) -> Vec<Value> {
    flat.chunks(2).map(|p| json!([p[0], p.get(1).cloned().unwrap_or(Value::Null)])).collect()
}

const PREVIEW: usize = 200;

pub fn get(db: u8, key: &str) -> Result<KeyDetail, String> {
    let mut c = Conn::open(db)?;
    let kind = val(c.cmd(&["TYPE", key])?)?.as_str().unwrap_or("none").to_string();
    if kind == "none" {
        return Err("This key no longer exists.".into());
    }
    let ttl = val(c.cmd(&["TTL", key])?)?.as_i64().unwrap_or(-1);
    let lim = PREVIEW.to_string();
    let last = (PREVIEW - 1).to_string();
    let (size, value, truncated) = match kind.as_str() {
        "string" => {
            let n = val(c.cmd(&["STRLEN", key])?)?.as_i64().unwrap_or(0);
            (n, val(c.cmd(&["GET", key])?)?, false)
        }
        "list" => {
            let n = val(c.cmd(&["LLEN", key])?)?.as_i64().unwrap_or(0);
            (n, val(c.cmd(&["LRANGE", key, "0", &last])?)?, n as usize > PREVIEW)
        }
        "hash" => {
            let n = val(c.cmd(&["HLEN", key])?)?.as_i64().unwrap_or(0);
            let flat = scan_items(&mut c, "HSCAN", key, PREVIEW * 2)?;
            (n, Value::Array(pairs(&flat)), n as usize > PREVIEW)
        }
        "set" => {
            let n = val(c.cmd(&["SCARD", key])?)?.as_i64().unwrap_or(0);
            (n, Value::Array(scan_items(&mut c, "SSCAN", key, PREVIEW)?), n as usize > PREVIEW)
        }
        "zset" => {
            let n = val(c.cmd(&["ZCARD", key])?)?.as_i64().unwrap_or(0);
            let flat = val(c.cmd(&["ZRANGE", key, "0", &last, "WITHSCORES"])?)?;
            (n, Value::Array(pairs(flat.as_array().map(|a| a.as_slice()).unwrap_or(&[]))), n as usize > PREVIEW)
        }
        "stream" => {
            let n = val(c.cmd(&["XLEN", key])?)?.as_i64().unwrap_or(0);
            (n, val(c.cmd(&["XRANGE", key, "-", "+", "COUNT", "50"])?)?, n > 50)
        }
        _ => (0, Value::Null, false),
    };
    let _ = lim;
    Ok(KeyDetail { kind, ttl, size, value, truncated })
}

pub fn set_string(db: u8, key: &str, value: &str, ttl: Option<u64>) -> Result<(), String> {
    if key.is_empty() {
        return Err("The key name cannot be empty.".into());
    }
    let mut c = Conn::open(db)?;
    match ttl {
        Some(t) if t > 0 => val(c.cmd(&["SET", key, value, "EX", &t.to_string()])?)?,
        _ => val(c.cmd(&["SET", key, value])?)?,
    };
    Ok(())
}

pub fn delete(db: u8, key: &str) -> Result<(), String> {
    let mut c = Conn::open(db)?;
    val(c.cmd(&["DEL", key])?)?;
    Ok(())
}

/// `Some(seconds)` sets an expiry, `None` removes it (key lives forever).
pub fn expire(db: u8, key: &str, seconds: Option<u64>) -> Result<(), String> {
    let mut c = Conn::open(db)?;
    match seconds {
        Some(s) if s > 0 => val(c.cmd(&["EXPIRE", key, &s.to_string()])?)?,
        _ => val(c.cmd(&["PERSIST", key])?)?,
    };
    Ok(())
}

pub fn flush_db(db: u8) -> Result<(), String> {
    let mut c = Conn::open(db)?;
    val(c.cmd(&["FLUSHDB"])?)?;
    Ok(())
}

#[derive(Serialize)]
pub struct ExecResult {
    pub ok: bool,
    pub reply: Value,
}

/// Split a command line into arguments, honouring "double" and 'single' quotes.
pub fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) => {
                if ch == '\\' && q == '"' {
                    match chars.next() {
                        Some('n') => cur.push('\n'),
                        Some('t') => cur.push('\t'),
                        Some(o) => cur.push(o),
                        None => return Err("Unterminated quote.".into()),
                    }
                } else if ch == q {
                    quote = None;
                } else {
                    cur.push(ch);
                }
            }
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                    has = true;
                } else if ch.is_whitespace() {
                    if has || !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                        has = false;
                    }
                } else {
                    cur.push(ch);
                }
            }
        }
    }
    if quote.is_some() {
        return Err("Unterminated quote.".into());
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

pub fn exec(db: u8, line: &str) -> Result<ExecResult, String> {
    let args = tokenize(line)?;
    let Some(first) = args.first() else {
        return Err("Type a Redis command, for example: GET mykey".into());
    };
    let verb = first.to_uppercase();
    if ["MONITOR", "SUBSCRIBE", "PSUBSCRIBE", "SSUBSCRIBE", "SYNC", "PSYNC", "SELECT", "WATCH", "MULTI", "EXEC"].contains(&verb.as_str()) {
        return Err(format!("{verb} is not supported in this console. Use the database picker for SELECT, or redis-cli for interactive commands."));
    }
    let mut c = Conn::open(db)?;
    Ok(match c.cmd(args.as_slice())? {
        Reply::Val(v) => ExecResult { ok: true, reply: v },
        Reply::Err(e) => ExecResult { ok: false, reply: json!(e) },
    })
}

#[derive(Serialize)]
pub struct DbCount {
    pub db: u8,
    pub keys: i64,
}

#[derive(Serialize)]
pub struct InfoOut {
    pub version: String,
    pub uptime: i64,
    pub clients: i64,
    pub memory: String,
    pub dbs: Vec<DbCount>,
}

pub fn info(db: u8) -> Result<InfoOut, String> {
    let mut c = Conn::open(db)?;
    let text = val(c.cmd(&["INFO"])?)?;
    let text = text.as_str().unwrap_or("");
    let get = |k: &str| -> String {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}:")))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let mut dbs = vec![];
    for l in text.lines() {
        if let Some(rest) = l.strip_prefix("db") {
            if let Some((n, stats)) = rest.split_once(':') {
                if let Ok(n) = n.parse::<u8>() {
                    let keys = stats
                        .split(',')
                        .find_map(|p| p.strip_prefix("keys="))
                        .and_then(|k| k.parse().ok())
                        .unwrap_or(0);
                    dbs.push(DbCount { db: n, keys });
                }
            }
        }
    }
    Ok(InfoOut {
        version: get("redis_version"),
        uptime: get("uptime_in_seconds").parse().unwrap_or(0),
        clients: get("connected_clients").parse().unwrap_or(0),
        memory: get("used_memory_human"),
        dbs,
    })
}
