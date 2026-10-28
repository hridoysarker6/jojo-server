//! Service manager: versions, config generation, process control, downloads, sites, HTTPS, hosts.

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const HOSTS_PATH: &str = r"C:\Windows\System32\drivers\etc\hosts";
const DEFAULT_TLD: &str = "test";
/// Real, commonly used suffixes that must not be hijacked by local sites.
const BLOCKED_TLDS: [&str; 14] = [
    "com", "net", "org", "dev", "app", "io", "co", "local", "edu", "gov", "info", "me", "ai", "bd",
];
const MKCERT_URL: &str =
    "https://github.com/FiloSottile/mkcert/releases/download/v1.4.4/mkcert-v1.4.4-windows-amd64.exe";
const ADMINER_URL: &str = "https://github.com/vrana/adminer/releases/download/v4.17.1/adminer-4.17.1.php";
// Node.js is added separately (always the newest LTS, see install_node_lts).
const RECOMMENDED: [(&str, &str); 4] = [("nginx", "1.26.2"), ("php", "8.3.12"), ("mysql", "8.4.2"), ("mailpit", "1.20.4")];

/// Sends an event (name, JSON payload) to whoever is listening (the UI, or a test).
pub type Emit = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

pub(crate) fn e2s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Run child processes without flashing a console window.
pub fn hide(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

pub(crate) fn run_quiet(cmd: &mut Command) {
    hide(cmd);
    let _ = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub(crate) fn fwd(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

// ---------------------------------------------------------------- paths

/// Read a value the installer stored under HKCU\Software\JOJO Server.
fn reg_value(name: &str) -> Option<String> {
    let mut c = Command::new("reg");
    c.args(["query", r"HKCU\Software\JOJO Server", "/v", name]);
    hide(&mut c);
    let out = c.output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_reg_value(&String::from_utf8_lossy(&out.stdout), name)
}

/// Pull the text value out of `reg query` output.
pub fn parse_reg_value(text: &str, name: &str) -> Option<String> {
    let line = text.lines().find(|l| l.trim_start().starts_with(name))?;
    let (_, value) = line.split_once("REG_SZ")?;
    let v = value.trim();
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

/// JOJO Server's data folder (programs, databases, projects). Order: JOJO_ROOT, the folder
/// chosen in the installer, then %USERPROFILE%\JojoServer.
pub fn root() -> PathBuf {
    if let Ok(r) = std::env::var("JOJO_ROOT") {
        return PathBuf::from(r);
    }
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        if let Some(p) = reg_value("RootDir") {
            let t = p.trim_end_matches(['\\', '/']);
            // "D:\" on its own would put everything in the drive root: use a sub-folder.
            let p = if t.len() == 2 && t.ends_with(':') { PathBuf::from(format!("{t}\\JojoServer")) } else { PathBuf::from(t) };
            if p.is_absolute() && p.parent().is_some() {
                return p;
            }
        }
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join("JojoServer")
    })
    .clone()
}

fn norm(p: &Path) -> String {
    fwd(p).trim_end_matches('/').to_lowercase()
}

fn is_inside(child: &Path, parent: &Path) -> bool {
    let (c, p) = (norm(child), norm(parent));
    c == p || c.starts_with(&format!("{p}/"))
}

/// Why this folder cannot be the projects folder, if it cannot.
pub fn www_dir_problem(p: &Path) -> Option<String> {
    if !p.is_absolute() || p.parent().is_none() {
        return Some("Enter a full folder path, for example D:\\websites".into());
    }
    let r = root();
    if norm(p) == norm(&r) {
        return Some("The projects folder cannot be JOJO Server's own data folder. Use a sub-folder such as 'www'.".into());
    }
    if is_inside(&r, p) {
        return Some("The projects folder cannot be a folder that contains JOJO Server's data folder.".into());
    }
    for inner in ["bin", "data", "etc", "run", "logs", "downloads"] {
        if is_inside(p, &r.join(inner)) {
            return Some(format!("The projects folder cannot be inside JOJO Server's own '{inner}' folder."));
        }
    }
    None
}

/// Folder that holds the user's websites (one sub-folder per site). A saved folder that is
/// not usable (for example the data folder itself) is ignored in favour of <data folder>\www.
pub fn www_dir() -> PathBuf {
    let default = root().join("www");
    match load_settings().www_dir.filter(|p| !p.trim().is_empty()) {
        Some(p) => {
            let p = PathBuf::from(p);
            if www_dir_problem(&p).is_none() {
                p
            } else {
                default
            }
        }
        None => default,
    }
}

pub fn ensure_dirs() {
    for d in ["bin", "data", "etc", "logs", "run", "downloads"] {
        let _ = fs::create_dir_all(root().join(d));
    }
    let _ = fs::create_dir_all(www_dir());
}

fn same_path(a: &Path, b: &Path) -> bool {
    norm(a) == norm(b)
}

/// Point JOJO Server at a different projects folder, optionally moving the existing projects.
pub fn set_www_dir(core: &Core, new_dir: &str, move_existing: bool) -> Result<(), String> {
    let new = PathBuf::from(new_dir.trim().trim_end_matches(['\\', '/']));
    if !new.is_absolute() || new.parent().is_none() {
        return Err("Enter a full folder path, for example D:\\websites".into());
    }
    let old = www_dir();
    if same_path(&new, &old) {
        return Ok(());
    }
    if let Some(why) = www_dir_problem(&new) {
        return Err(why);
    }
    if is_inside(&new, &old) || is_inside(&old, &new) {
        return Err("The new folder cannot be inside the old one, or the other way round.".into());
    }
    let moving = move_existing && old.is_dir();
    if moving {
        let non_empty = fs::read_dir(&new).map(|mut d| d.next().is_some()).unwrap_or(false);
        if non_empty {
            return Err("The new folder already has files in it. Pick an empty folder, or untick 'Move my existing projects'.".into());
        }
    }
    fs::create_dir_all(&new).map_err(|e| format!("Could not create the folder: {e}"))?;

    // Web server and PHP must not hold files open while they are moved.
    let web = core.running_web();
    let php = core.is_running("php");
    if let Some(w) = web {
        core.stop(w)?;
    }
    if php {
        core.stop("php")?;
    }
    let mut result: Result<(), String> = Ok(());
    if moving {
        let mut c = Command::new("robocopy");
        c.arg(&old).arg(&new).args(["/E", "/MOVE", "/NFL", "/NDL", "/NJH", "/NJS", "/NP", "/R:1", "/W:1"]);
        hide(&mut c);
        match c.output() {
            // robocopy reports success with exit codes below 8.
            Ok(o) if o.status.code().map(|c| c < 8).unwrap_or(false) => {
                let _ = fs::remove_dir(&old); // only works when it is empty now
            }
            Ok(o) => {
                result = Err(format!(
                    "Moving the projects failed (code {}). Close programs that use the old folder and try again.",
                    o.status.code().unwrap_or(-1)
                ))
            }
            Err(e) => result = Err(e2s(e)),
        }
    }
    if result.is_ok() {
        let mut s = load_settings();
        s.www_dir = Some(new.to_string_lossy().to_string());
        result = save_settings(&s);
    }
    if php {
        let _ = core.start("php");
    }
    if let Some(w) = web {
        let _ = core.start(w);
    }
    result
}

/// Native "choose a folder" dialog. Returns None when cancelled.
pub fn pick_folder(initial: &str) -> Result<Option<String>, String> {
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $f = New-Object System.Windows.Forms.Form; $f.TopMost = $true; \
         $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
         $d.Description = 'Choose the folder for your projects'; $d.ShowNewFolderButton = $true; \
         $d.SelectedPath = '{}'; \
         if ($d.ShowDialog($f) -eq 'OK') {{ Write-Output $d.SelectedPath }}",
        initial.replace('\'', "''")
    );
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-STA", "-Command"]).arg(script);
    hide(&mut c);
    let out = c.output().map_err(e2s)?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(if p.is_empty() { None } else { Some(p) })
}

// ---------------------------------------------------------------- definitions

pub struct Def {
    pub id: &'static str,
    pub name: &'static str,
    pub port: u16,
    pub exes: &'static [&'static str],
}

pub static DEFS: [Def; 7] = [
    Def { id: "postgres", name: "PostgreSQL", port: 5432, exes: &["bin/postgres.exe"] },
    Def { id: "mysql", name: "MySQL", port: 3306, exes: &["bin/mysqld.exe"] },
    Def { id: "redis", name: "Redis", port: 6379, exes: &["redis-server.exe", "valkey-server.exe"] },
    Def { id: "mailpit", name: "Mailpit", port: 8025, exes: &["mailpit.exe"] },
    Def { id: "php", name: "PHP", port: 9000, exes: &["php-cgi.exe"] },
    Def { id: "apache", name: "Apache", port: 80, exes: &["bin/httpd.exe"] },
    Def { id: "nginx", name: "Nginx", port: 80, exes: &["nginx.exe"] },
];

/// Node.js is a tool, not a service: it has versions (bin/node/<version>) but never runs on its own.
pub static NODE_DEF: Def = Def { id: "node", name: "Node.js", port: 0, exes: &["node.exe"] };

pub fn def(id: &str) -> Option<&'static Def> {
    if id == "node" {
        return Some(&NODE_DEF);
    }
    DEFS.iter().find(|d| d.id == id)
}

pub(crate) fn base_dir(ver_dir: &Path, d: &Def) -> Option<PathBuf> {
    let has = |p: &Path| d.exes.iter().any(|e| p.join(e).is_file());
    if has(ver_dir) {
        return Some(ver_dir.to_path_buf());
    }
    // Tolerate one nested folder (manually extracted archives).
    if let Ok(rd) = fs::read_dir(ver_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && has(&p) {
                return Some(p);
            }
        }
    }
    None
}

pub(crate) fn exe_in(base: &Path, d: &Def) -> Option<PathBuf> {
    d.exes.iter().map(|e| base.join(e)).find(|p| p.is_file())
}

pub(crate) fn version_key(s: &str) -> Vec<u32> {
    s.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

pub fn versions(id: &str) -> Vec<String> {
    let Some(d) = def(id) else { return vec![] };
    let dir = root().join("bin").join(id);
    let mut v = vec![];
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if e.path().is_dir() && !name.starts_with('.') && base_dir(&e.path(), d).is_some() {
                v.push(name);
            }
        }
    }
    v.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
    v
}

// ---------------------------------------------------------------- settings

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SiteCfg {
    #[serde(default)]
    pub php: Option<String>,
    #[serde(default)]
    pub ssl: bool,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Settings {
    #[serde(default)]
    pub active: HashMap<String, String>,
    #[serde(default)]
    pub ports: HashMap<String, u16>,
    #[serde(default)]
    pub php_ports: HashMap<String, u16>,
    #[serde(default)]
    pub sites: HashMap<String, SiteCfg>,
    #[serde(default)]
    pub ssl_ready: bool,
    #[serde(default)]
    pub tld: Option<String>,
    #[serde(default)]
    pub www_dir: Option<String>,
    #[serde(default)]
    pub php_ext: HashMap<String, HashMap<String, bool>>,
    #[serde(default)]
    pub php_conf: PhpConf,
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub dns: DnsCfg,
    #[serde(default)]
    pub workers: Vec<WorkerCfg>,
    /// "nginx" (default) or "apache": the web server started by Start All
    #[serde(default)]
    pub web_server: Option<String>,
}

/// Wildcard domains: a small built-in DNS server answers every name under the domain suffix.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct DnsCfg {
    #[serde(default)]
    pub enabled: bool,
    /// The suffix Windows was told to ask us about (needs a one-time administrator step).
    #[serde(default)]
    pub applied_tld: Option<String>,
}

/// A background command that belongs to a project (queue worker, scheduler, dev server...).
#[derive(Serialize, Deserialize, Clone)]
pub struct WorkerCfg {
    pub site: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub autostart: bool,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct General {
    pub launch_on_login: bool,
    pub autostart: Vec<String>,
    pub close_to_tray: bool,
    /// look for a new JOJO Server version a few seconds after start-up
    pub check_updates: bool,
}

impl Default for General {
    fn default() -> Self {
        General { launch_on_login: false, autostart: vec![], close_to_tray: true, check_updates: true }
    }
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

pub fn set_general(g: General) -> Result<(), String> {
    if g.autostart.iter().any(|id| def(id).is_none()) {
        return Err("Unknown service in the start-up list.".into());
    }
    let mut c = Command::new("reg");
    if g.launch_on_login {
        let exe = std::env::current_exe().map_err(e2s)?;
        c.arg("add")
            .arg(RUN_KEY)
            .arg("/v")
            .arg("JOJO Server")
            .arg("/t")
            .arg("REG_SZ")
            .arg("/d")
            .arg(format!("\"{}\" --minimized", exe.display()))
            .arg("/f");
    } else {
        c.arg("delete").arg(RUN_KEY).arg("/v").arg("JOJO Server").arg("/f");
    }
    hide(&mut c);
    let out = c.output().map_err(e2s)?;
    // Deleting a value that does not exist is fine.
    if g.launch_on_login && !out.status.success() {
        return Err("Windows did not accept the start-up entry.".into());
    }
    let mut s = load_settings();
    s.general = g;
    save_settings(&s)
}

pub fn tld_of(s: &Settings) -> String {
    s.tld.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| DEFAULT_TLD.to_string())
}

pub fn load_settings() -> Settings {
    fs::read_to_string(root().join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_settings(s: &Settings) -> Result<(), String> {
    let t = serde_json::to_string_pretty(s).map_err(e2s)?;
    fs::write(root().join("settings.json"), t).map_err(e2s)
}

/// The two web servers. Only one runs at a time and they share one HTTP port.
pub const WEB_SERVERS: [&str; 2] = ["nginx", "apache"];

/// The web server that "Start All" starts: the chosen one, or whichever is installed.
pub fn active_web(s: &Settings) -> String {
    let chosen = s.web_server.clone().filter(|w| WEB_SERVERS.contains(&w.as_str())).unwrap_or_else(|| "nginx".into());
    if versions(&chosen).is_empty() {
        if let Some(other) = WEB_SERVERS.iter().find(|w| **w != chosen && !versions(w).is_empty()) {
            return other.to_string();
        }
    }
    chosen
}

pub(crate) fn port_of(s: &Settings, id: &str) -> u16 {
    // Apache and Nginx use the same HTTP port setting, so switching between them never changes the URLs.
    let id = if id == "apache" { "nginx" } else { id };
    s.ports
        .get(id)
        .copied()
        .unwrap_or_else(|| def(id).map(|d| d.port).unwrap_or(0))
}

/// Port Mailpit listens on for outgoing mail (what PHP and Laravel send to).
pub fn smtp_port_of(s: &Settings) -> u16 {
    s.ports.get("smtp").copied().unwrap_or(1025)
}

/// Configured port of a service (used by the built-in Redis browser).
pub fn service_port(id: &str) -> u16 {
    port_of(&load_settings(), id)
}

pub(crate) fn https_port(s: &Settings) -> u16 {
    s.ports.get("https").copied().unwrap_or(443)
}

pub(crate) fn active_of(s: &Settings, id: &str) -> Option<String> {
    let vs = versions(id);
    match s.active.get(id) {
        Some(v) if vs.contains(v) => Some(v.clone()),
        _ => vs.first().cloned(),
    }
}

/// Each PHP version gets its own stable FastCGI port, starting at the configured PHP port.
pub(crate) fn php_port_for(ver: &str) -> u16 {
    let mut s = load_settings();
    if let Some(p) = s.php_ports.get(ver) {
        return *p;
    }
    let mut p = port_of(&s, "php");
    while s.php_ports.values().any(|x| *x == p) {
        p += 1;
    }
    s.php_ports.insert(ver.into(), p);
    let _ = save_settings(&s);
    p
}

pub fn set_active(id: &str, version: &str) -> Result<(), String> {
    if def(id).is_none() {
        return Err("Unknown service.".into());
    }
    let mut s = load_settings();
    s.active.insert(id.into(), version.into());
    save_settings(&s)
}

pub fn set_port(id: &str, port: u16) -> Result<(), String> {
    if (def(id).is_none() && id != "https" && id != "smtp") || port == 0 {
        return Err("Invalid port.".into());
    }
    let id = if id == "apache" { "nginx" } else { id };
    let mut s = load_settings();
    s.ports.insert(id.into(), port);
    if id == "php" {
        s.php_ports.clear(); // re-assigned from the new base port
    }
    save_settings(&s)
}

// ---------------------------------------------------------------- logging

pub fn log(emit: &Emit, svc: &str, level: &str, line: impl Into<String>) {
    emit("log", json!({ "svc": svc, "level": level, "line": line.into() }));
}

/// Stop programs left running from an earlier session (a crash, or a force-quit) that live inside
/// JOJO Server's own `bin` folder. They would keep old ports and old settings alive.
/// Only processes whose executable is inside <data folder>\bin are touched.
pub fn reap_orphans(emit: &Emit) -> usize {
    let bin = root().join("bin");
    let script = format!(
        "$bin = '{}'; $n = 0; \
         Get-Process -ErrorAction SilentlyContinue | Where-Object {{ $_.Path -and $_.Path.StartsWith($bin + '\\', [StringComparison]::OrdinalIgnoreCase) }} | \
         ForEach-Object {{ Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue; $n++ }}; Write-Output $n",
        bin.to_string_lossy().replace('\'', "''")
    );
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-Command"]).arg(script);
    hide(&mut c);
    let n: usize = c
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0);
    if n > 0 {
        log(emit, "app", "warn", format!("Stopped {n} leftover process(es) from an earlier session."));
    }
    n
}

fn port_in_use(port: u16) -> bool {
    let addr: SocketAddr = match format!("127.0.0.1:{port}").parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

pub(crate) fn pipe_logs<R: Read + Send + 'static>(emit: Emit, id: String, r: R) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut rd = BufReader::new(r);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match rd.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let s = String::from_utf8_lossy(&buf).trim_end().to_string();
                    if s.is_empty() {
                        continue;
                    }
                    let low = s.to_lowercase();
                    let lvl = if low.contains("error") || low.contains("fatal") || low.contains("emerg") {
                        "err"
                    } else if low.contains("warn") {
                        "warn"
                    } else {
                        "info"
                    };
                    log(&emit, &id, lvl, s);
                }
            }
        }
    })
}

// ---------------------------------------------------------------- sites

#[derive(Serialize, Clone)]
pub struct Site {
    pub name: String,
    pub domain: String,
    pub path: String,
    pub doc_root: String,
    pub php: Option<String>,
    pub ssl: bool,
}

pub(crate) fn sanitize(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else if c == '_' || c == ' ' { '-' } else { '\0' })
        .filter(|c| *c != '\0')
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

pub fn sites() -> Vec<Site> {
    let s = load_settings();
    let tld = tld_of(&s);
    let mut out = vec![];
    if let Ok(rd) = fs::read_dir(www_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            let raw = e.file_name().to_string_lossy().to_string();
            if !p.is_dir() || raw.starts_with('.') {
                continue;
            }
            let name = sanitize(&raw);
            if name.is_empty() {
                continue;
            }
            let public = p.join("public");
            let doc = if public.is_dir() { public } else { p.clone() };
            let cfg = s.sites.get(&name).cloned().unwrap_or_default();
            out.push(Site {
                domain: format!("{name}.{tld}"),
                name,
                path: p.to_string_lossy().to_string(),
                doc_root: doc.to_string_lossy().to_string(),
                php: cfg.php.filter(|v| !v.is_empty()),
                ssl: cfg.ssl,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn create_site(name: &str) -> Result<(), String> {
    let n = sanitize(name);
    if n.is_empty() {
        return Err("Invalid site name. Use letters, numbers and '-' only.".into());
    }
    let dir = www_dir().join(&n);
    if dir.exists() {
        return Err(format!("A folder named '{n}' already exists."));
    }
    fs::create_dir_all(&dir).map_err(e2s)?;
    let tld = tld_of(&load_settings());
    let html = format!(
        "<?php\n/* Created by JOJO Server */\n?>\n<!doctype html>\n<meta charset=\"utf-8\">\n<title>{n}.{tld}</title>\n<body style=\"font-family:system-ui;padding:3rem\">\n<h1>{n}.{tld} is live</h1>\n<p>PHP <?= PHP_VERSION ?> &middot; <?= date('c') ?></p>\n</body>\n"
    );
    fs::write(dir.join("index.php"), html).map_err(e2s)
}

/// Set the PHP version (empty string = use the default) and HTTPS flag for a site.
pub fn set_site_config(emit: &Emit, name: &str, php: &str, ssl: bool) -> Result<(), String> {
    if !sites().iter().any(|s| s.name == name) {
        return Err("Site not found.".into());
    }
    if !php.is_empty() && !versions("php").iter().any(|v| v == php) {
        return Err(format!("PHP {php} is not installed."));
    }
    let mut s = load_settings();
    if ssl {
        if !s.ssl_ready {
            return Err("Set up HTTPS first (Sites tab, 'Enable HTTPS').".into());
        }
        ensure_cert(emit, &format!("{name}.{}", tld_of(&s)))?;
    }
    let cfg = s.sites.entry(name.to_string()).or_default();
    cfg.php = if php.is_empty() { None } else { Some(php.to_string()) };
    cfg.ssl = ssl;
    save_settings(&s)
}

/// Change the domain suffix (for example "test" -> "localhost" or "loc").
pub fn set_tld(emit: &Emit, raw: &str) -> Result<(), String> {
    let t = raw.trim().trim_start_matches('.').to_lowercase();
    if t.len() < 2 || t.len() > 15 || !t.chars().all(|c| c.is_ascii_alphanumeric()) || t.starts_with(|c: char| c.is_ascii_digit()) {
        return Err("Use 2 to 15 letters or numbers, starting with a letter (for example: test, localhost, loc).".into());
    }
    if BLOCKED_TLDS.contains(&t.as_str()) {
        return Err(format!(
            ".{t} is a real internet suffix. Using it would make the real .{t} websites unreachable. Try .test, .localhost or .loc instead."
        ));
    }
    let mut s = load_settings();
    s.tld = Some(t.clone());
    save_settings(&s)?;
    // Sites that use HTTPS need a certificate for the new domain name.
    if s.ssl_ready {
        for site in sites() {
            if site.ssl {
                ensure_cert(emit, &site.domain)?;
            }
        }
    }
    log(emit, "sites", "info", format!("Domain suffix changed to .{t}. Click 'Sync hosts' to apply it."));
    Ok(())
}

fn hosts_block() -> String {
    let list = sites();
    if list.is_empty() {
        return String::new();
    }
    let mut b = String::from("# JOJO-BEGIN\r\n");
    for s in list {
        b.push_str(&format!("127.0.0.1 {}\r\n", s.domain));
    }
    b.push_str("# JOJO-END\r\n");
    b
}

fn merged_hosts() -> Result<String, String> {
    let cur = fs::read(HOSTS_PATH).map_err(e2s)?;
    let cur = String::from_utf8_lossy(&cur).to_string();
    let mut out = String::new();
    let mut skip = false;
    for line in cur.lines() {
        match line.trim() {
            "# JOJO-BEGIN" => skip = true,
            "# JOJO-END" => skip = false,
            _ if !skip => {
                out.push_str(line);
                out.push_str("\r\n");
            }
            _ => {}
        }
    }
    out.push_str(&hosts_block());
    Ok(out)
}

pub fn hosts_in_sync() -> bool {
    // With wildcard domains switched on (and applied for the current suffix) no hosts entries are needed.
    let s = load_settings();
    if s.dns.enabled && s.dns.applied_tld.as_deref() == Some(tld_of(&s).as_str()) {
        return true;
    }
    match (merged_hosts(), fs::read(HOSTS_PATH)) {
        (Ok(want), Ok(have)) => {
            let have = String::from_utf8_lossy(&have).replace("\r\n", "\n");
            want.replace("\r\n", "\n").trim_end() == have.trim_end()
        }
        _ => true,
    }
}

pub fn sync_hosts() -> Result<String, String> {
    let new_text = merged_hosts()?;
    match fs::write(HOSTS_PATH, &new_text) {
        Ok(_) => return Ok("Hosts file updated.".into()),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {}
        Err(e) => return Err(e2s(e)),
    }
    // Not running as administrator: ask Windows (one UAC prompt) to copy the prepared file.
    let run = root().join("run");
    let staged = run.join("hosts.new");
    let script = run.join("sync-hosts.ps1");
    fs::write(&staged, new_text).map_err(e2s)?;
    fs::write(
        &script,
        format!("Copy-Item -Force -LiteralPath '{}' -Destination '{}'\r\n", staged.display(), HOSTS_PATH),
    )
    .map_err(e2s)?;
    let cmdline = format!(
        "Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','\"{}\"'",
        script.display()
    );
    let mut c = Command::new("powershell");
    c.arg("-NoProfile").arg("-Command").arg(&cmdline);
    hide(&mut c);
    let st = c.status().map_err(e2s)?;
    if st.success() && hosts_in_sync() {
        Ok("Hosts file updated (with administrator permission).".into())
    } else {
        Err("Administrator permission was not granted. Click 'Yes' on the Windows prompt.".into())
    }
}

// ---------------------------------------------------------------- HTTPS (mkcert)

fn mkcert_exe() -> PathBuf {
    root().join("bin").join("mkcert").join("mkcert.exe")
}

pub(crate) fn ssl_dir() -> PathBuf {
    root().join("etc").join("ssl")
}

fn mkcert_cmd() -> Command {
    let mut c = Command::new(mkcert_exe());
    c.env("CAROOT", ssl_dir().join("caroot"));
    hide(&mut c);
    c
}

/// Download mkcert if it is not present yet.
pub fn fetch_mkcert(emit: &Emit) -> Result<(), String> {
    fs::create_dir_all(ssl_dir().join("caroot")).map_err(e2s)?;
    if !mkcert_exe().is_file() {
        log(emit, "https", "info", "Downloading mkcert...");
        fs::create_dir_all(mkcert_exe().parent().unwrap()).map_err(e2s)?;
        download_file(emit, "mkcert", "1.4.4", MKCERT_URL, &mkcert_exe())?;
    }
    Ok(())
}

/// Download mkcert and trust its local certificate authority (Windows shows one confirmation dialog).
pub fn setup_ssl(emit: &Emit) -> Result<String, String> {
    fetch_mkcert(emit)?;
    log(emit, "https", "info", "Installing the local certificate authority. Confirm the Windows security dialog if it appears.");
    let out = mkcert_cmd().arg("-install").output().map_err(e2s)?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).to_string();
        return Err(format!("mkcert -install failed: {}", msg.lines().last().unwrap_or("unknown error")));
    }
    let mut s = load_settings();
    s.ssl_ready = true;
    save_settings(&s)?;
    log(emit, "https", "ok", "HTTPS is ready. Turn it on per site.");
    Ok("HTTPS is ready.".into())
}

fn cert_paths(domain: &str) -> (PathBuf, PathBuf) {
    (ssl_dir().join(format!("{domain}.pem")), ssl_dir().join(format!("{domain}-key.pem")))
}

fn ensure_cert(emit: &Emit, domain: &str) -> Result<(), String> {
    let (crt, key) = cert_paths(domain);
    if crt.is_file() && key.is_file() {
        return Ok(());
    }
    if !mkcert_exe().is_file() {
        return Err("mkcert is not installed. Click 'Enable HTTPS' first.".into());
    }
    let out = mkcert_cmd()
        .arg("-cert-file")
        .arg(&crt)
        .arg("-key-file")
        .arg(&key)
        .arg(domain)
        .output()
        .map_err(e2s)?;
    if !out.status.success() {
        return Err(format!(
            "Could not create a certificate for {domain}: {}",
            String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("")
        ));
    }
    log(emit, "https", "ok", format!("Certificate created for {domain}"));
    Ok(())
}

// ---------------------------------------------------------------- config writers

fn write_php_ini(base: &Path, ver: &str) -> Result<PathBuf, String> {
    let dir = root().join("etc").join("php").join(ver);
    fs::create_dir_all(&dir).map_err(e2s)?;
    let src = ["php.ini-development", "php.ini-production"]
        .iter()
        .map(|f| base.join(f))
        .find(|p| p.is_file());
    let text = match src {
        Some(p) => fs::read_to_string(p).map_err(e2s)?,
        None => String::new(),
    };
    let s = load_settings();
    let conf = &s.php_conf;
    let mut out = text.replace("\r\n", "\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\n; --- JOJO Server (generated; change these in the Settings tab) ---\n");
    out.push_str(&format!("extension_dir = \"{}\"\n", fwd(&base.join("ext"))));
    // Without a CA bundle every HTTPS request from PHP fails with "cURL error 60".
    if let Some(ca) = ca_bundle() {
        out.push_str(&format!("curl.cainfo = \"{0}\"\nopenssl.cafile = \"{0}\"\n", fwd(&ca)));
    }
    for e in php_extension_list(base, ver, &s) {
        if e.enabled {
            out.push_str(&format!("{}={}\n", if e.zend { "zend_extension" } else { "extension" }, e.name));
        }
    }
    // PHP 8.5 has OPcache built in (and switched on), so it must be switched off explicitly:
    // with the default settings php-cgi 8.5 for Windows can stop at start-up with
    // "Opcode handlers are unusable due to ASLR". Older versions load it as an extension.
    let builtin_opcache = {
        let k = version_key(ver);
        k.first().copied().unwrap_or(0) > 8 || (k.first().copied() == Some(8) && k.get(1).copied().unwrap_or(0) >= 5)
    };
    if conf.opcache {
        if !builtin_opcache {
            out.push_str("zend_extension=opcache\n");
        }
        out.push_str("opcache.enable=1\nopcache.enable_cli=0\nopcache.validate_timestamps=1\nopcache.revalidate_freq=1\n");
    } else {
        out.push_str("opcache.enable=0\nopcache.enable_cli=0\n");
    }
    if php_extension_list(base, ver, &s).iter().any(|e| e.name == "xdebug" && e.enabled) {
        out.push_str("xdebug.mode=develop,debug\nxdebug.start_with_request=trigger\nxdebug.client_host=127.0.0.1\nxdebug.client_port=9003\n");
    }
    out.push_str(&format!(
        "SMTP = 127.0.0.1\nsmtp_port = {}\nsendmail_from = \"dev@jojo.test\"\nlog_errors = On\nerror_log = \"{}\"\ndate.timezone = {}\nmemory_limit = {}\nupload_max_filesize = {}\npost_max_size = {}\nmax_execution_time = {}\nmax_input_vars = {}\ndisplay_errors = {}\nerror_reporting = {}\n",
        smtp_port_of(&s),
        fwd(&php_error_log()),
        conf.timezone,
        conf.memory_limit,
        conf.upload_max_filesize,
        conf.post_max_size,
        conf.max_execution_time,
        conf.max_input_vars,
        if conf.display_errors { "On" } else { "Off" },
        conf.error_reporting,
    ));
    if !conf.extra.trim().is_empty() {
        out.push_str("\n; --- Custom lines ---\n");
        out.push_str(conf.extra.trim());
        out.push('\n');
    }
    let path = dir.join("php.ini");
    fs::write(&path, out).map_err(e2s)?;
    Ok(path)
}

fn composer_dir() -> PathBuf {
    root().join("bin").join("tools").join("composer")
}

pub fn composer_installed() -> bool {
    composer_dir().join("composer.phar").is_file() && composer_dir().join("composer.bat").is_file()
}

/// Certificate authorities that PHP trusts for HTTPS (the Mozilla list published by curl.se).
/// Downloaded once and refreshed every 60 days; if the download fails the old copy is kept.
pub fn ca_bundle() -> Option<PathBuf> {
    let p = ssl_dir().join("cacert.pem");
    let fresh = fs::metadata(&p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|age| age < Duration::from_secs(60 * 24 * 3600))
        .unwrap_or(false);
    if !fresh {
        let _ = fs::create_dir_all(ssl_dir());
        if let Ok(text) = http_text("https://curl.se/ca/cacert.pem") {
            if text.contains("BEGIN CERTIFICATE") && text.len() > 100_000 {
                let tmp = ssl_dir().join("cacert.pem.part");
                if fs::write(&tmp, text).is_ok() {
                    let _ = fs::rename(&tmp, &p);
                }
            }
        }
    }
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

// ---------------------------------------------------------------- mail (Mailpit) and log files

fn php_error_log() -> PathBuf {
    root().join("logs").join("php-error.log")
}

/// Send one test message to Mailpit's SMTP port, the same way PHP and Laravel do.
pub fn send_test_mail() -> Result<String, String> {
    let port = smtp_port_of(&load_settings());
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().map_err(e2s)?;
    // Mailpit needs a moment after starting before its mail port accepts connections.
    let mut stream = None;
    for _ in 0..15 {
        if let Ok(s) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
            stream = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let stream = stream.ok_or_else(|| "Mailpit is not running. Start it on the Dashboard first.".to_string())?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut w = stream.try_clone().map_err(e2s)?;
    let mut r = BufReader::new(stream);
    // Read one SMTP reply (it may span several "250-" lines) and check its status code.
    let expect = |r: &mut BufReader<std::net::TcpStream>, code: &str| -> Result<(), String> {
        loop {
            let mut line = String::new();
            if r.read_line(&mut line).map_err(e2s)? == 0 {
                return Err("Mailpit closed the connection.".into());
            }
            if !line.starts_with(code) {
                return Err(format!("Mailpit answered: {}", line.trim()));
            }
            if line.as_bytes().get(3) != Some(&b'-') {
                return Ok(());
            }
        }
    };
    let stamp = now_secs();
    expect(&mut r, "220")?;
    w.write_all(b"EHLO jojo.test\r\n").map_err(e2s)?;
    expect(&mut r, "250")?;
    w.write_all(b"MAIL FROM:<dev@jojo.test>\r\n").map_err(e2s)?;
    expect(&mut r, "250")?;
    w.write_all(b"RCPT TO:<you@example.test>\r\n").map_err(e2s)?;
    expect(&mut r, "250")?;
    w.write_all(b"DATA\r\n").map_err(e2s)?;
    expect(&mut r, "354")?;
    let body = format!(
        "From: JOJO Server <dev@jojo.test>\r\nTo: You <you@example.test>\r\nSubject: JOJO Server test email ({stamp})\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nIt works! Your app can send mail to Mailpit on 127.0.0.1:{port}.\r\n.\r\n"
    );
    w.write_all(body.as_bytes()).map_err(e2s)?;
    expect(&mut r, "250")?;
    let _ = w.write_all(b"QUIT\r\n");
    Ok(format!("Test email sent. Open the Mailpit inbox to see it (port {port})."))
}

#[derive(Serialize)]
pub struct LogSource {
    pub id: String,
    pub label: String,
}

/// Log files the Logs tab can show.
pub fn log_sources() -> Vec<LogSource> {
    let mut v = vec![
        LogSource { id: "php".into(), label: "PHP errors".into() },
        LogSource { id: "nginx-error".into(), label: "Nginx errors".into() },
        LogSource { id: "nginx-access".into(), label: "Nginx requests".into() },
        LogSource { id: "apache-error".into(), label: "Apache errors".into() },
        LogSource { id: "apache-access".into(), label: "Apache requests".into() },
    ];
    for s in sites() {
        if laravel_log_path(Path::new(&s.path)).is_some() {
            v.push(LogSource { id: format!("laravel:{}", s.name), label: format!("Laravel Â· {}", s.domain) });
        }
    }
    v
}

/// The newest storage/logs/laravel*.log of a project (single or daily logs).
fn laravel_log_path(site_dir: &Path) -> Option<PathBuf> {
    let dir = site_dir.join("storage").join("logs");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with("laravel") && name.ends_with(".log") {
            let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().map(|(bt, _)| t > *bt).unwrap_or(true) {
                best = Some((t, e.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Map a log source id to a file. Only files JOJO Server knows about can be read.
fn log_path(source: &str) -> Result<PathBuf, String> {
    match source {
        "php" => Ok(php_error_log()),
        "nginx-error" => Ok(root().join("run").join("nginx").join("logs").join("error.log")),
        "nginx-access" => Ok(root().join("run").join("nginx").join("logs").join("access.log")),
        "apache-error" => Ok(root().join("run").join("apache").join("logs").join("error.log")),
        "apache-access" => Ok(root().join("run").join("apache").join("logs").join("access.log")),
        s if s.starts_with("laravel:") => {
            let name = &s["laravel:".len()..];
            let site = sites().into_iter().find(|x| x.name == name).ok_or("Site not found.")?;
            laravel_log_path(Path::new(&site.path)).ok_or_else(|| "This project has no Laravel log yet.".to_string())
        }
        _ => Err("Unknown log.".into()),
    }
}

#[derive(Serialize)]
pub struct LogText {
    pub path: String,
    pub exists: bool,
    pub size: u64,
    pub lines: Vec<String>,
}

/// Last `max_lines` lines of a log (reads only the end of the file, so huge logs are fine).
pub fn read_log(source: &str, max_lines: usize) -> Result<LogText, String> {
    use std::io::{Seek, SeekFrom};
    let path = log_path(source)?;
    let Ok(mut f) = fs::File::open(&path) else {
        return Ok(LogText { path: path.to_string_lossy().to_string(), exists: false, size: 0, lines: vec![] });
    };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let tail: u64 = 512 * 1024;
    let start = size.saturating_sub(tail);
    f.seek(SeekFrom::Start(start)).map_err(e2s)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(e2s)?;
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // the first line is probably cut in half
    }
    let skip = lines.len().saturating_sub(max_lines.clamp(50, 5000));
    Ok(LogText { path: path.to_string_lossy().to_string(), exists: true, size, lines: lines.split_off(skip) })
}

pub fn clear_log(source: &str) -> Result<(), String> {
    let path = log_path(source)?;
    if path.is_file() {
        fs::write(&path, b"").map_err(|e| format!("Could not clear the log (is it in use?): {e}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- PHP settings and extensions

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct PhpConf {
    pub memory_limit: String,
    pub upload_max_filesize: String,
    pub post_max_size: String,
    pub max_execution_time: u32,
    pub max_input_vars: u32,
    pub display_errors: bool,
    pub error_reporting: String,
    pub timezone: String,
    pub opcache: bool,
    pub extra: String,
}

impl Default for PhpConf {
    fn default() -> Self {
        PhpConf {
            memory_limit: "512M".into(),
            upload_max_filesize: "128M".into(),
            post_max_size: "128M".into(),
            max_execution_time: 120,
            max_input_vars: 3000,
            display_errors: true,
            error_reporting: "E_ALL".into(),
            timezone: "UTC".into(),
            opcache: false,
            extra: String::new(),
        }
    }
}

#[derive(Serialize, Clone)]
pub struct ExtInfo {
    pub name: String,
    pub enabled: bool,
    pub zend: bool,
    pub external: bool,
}

/// Extensions that are switched on by default for a fresh PHP install.
const DEFAULT_ON: [&str; 15] = [
    "curl", "fileinfo", "gd", "intl", "mbstring", "exif", "mysqli", "openssl", "pdo_mysql", "pdo_pgsql",
    "pdo_sqlite", "pgsql", "sqlite3", "zip", "sodium",
];

pub struct ExtDef {
    pub name: &'static str,
    pub description: &'static str,
    pub zend: bool,
}

/// Extensions that can be downloaded from PECL (the official PHP extension site).
pub static EXT_CATALOG: [ExtDef; 6] = [
    ExtDef { name: "redis", description: "Redis client (phpredis)", zend: false },
    ExtDef { name: "xdebug", description: "Step debugger and profiler", zend: true },
    ExtDef { name: "apcu", description: "Fast in-memory user cache", zend: false },
    ExtDef { name: "igbinary", description: "Compact binary serializer", zend: false },
    ExtDef { name: "imagick", description: "ImageMagick image processing", zend: false },
    ExtDef { name: "mongodb", description: "MongoDB driver", zend: false },
];

fn ext_def(name: &str) -> Option<&'static ExtDef> {
    EXT_CATALOG.iter().find(|e| e.name == name)
}

/// Every php_*.dll in the version's ext folder, with its on/off state.
fn php_extension_list(base: &Path, ver: &str, s: &Settings) -> Vec<ExtInfo> {
    let mut out = vec![];
    if let Ok(rd) = fs::read_dir(base.join("ext")) {
        for e in rd.flatten() {
            let file = e.file_name().to_string_lossy().to_string();
            let Some(name) = file.strip_prefix("php_").and_then(|n| n.strip_suffix(".dll")) else { continue };
            if name == "opcache" {
                continue; // controlled by the OPcache switch in PHP settings
            }
            let over = s.php_ext.get(ver).and_then(|m| m.get(name)).copied();
            let def = ext_def(name);
            out.push(ExtInfo {
                name: name.to_string(),
                enabled: over.unwrap_or_else(|| DEFAULT_ON.contains(&name)),
                zend: def.map(|d| d.zend).unwrap_or(false),
                external: def.is_some(),
            });
        }
    }
    // igbinary must load before the extensions that use it.
    out.sort_by(|a, b| (a.name != "igbinary").cmp(&(b.name != "igbinary")).then_with(|| a.name.cmp(&b.name)));
    out
}

fn php_base(ver: &str) -> Result<PathBuf, String> {
    let d = def("php").unwrap();
    base_dir(&root().join("bin").join("php").join(ver), d).ok_or_else(|| format!("PHP {ver} is not installed."))
}

pub fn php_extensions(ver: &str) -> Result<Vec<ExtInfo>, String> {
    Ok(php_extension_list(&php_base(ver)?, ver, &load_settings()))
}

pub fn set_php_extension(ver: &str, name: &str, enabled: bool) -> Result<(), String> {
    let list = php_extensions(ver)?;
    if !list.iter().any(|e| e.name == name) {
        return Err(format!("Extension '{name}' was not found for PHP {ver}."));
    }
    let mut s = load_settings();
    s.php_ext.entry(ver.to_string()).or_default().insert(name.to_string(), enabled);
    save_settings(&s)
}

pub fn remove_php_extension(ver: &str, name: &str) -> Result<(), String> {
    if ext_def(name).is_none() {
        return Err("Only downloaded extensions can be removed. Bundled ones can be switched off instead.".into());
    }
    let dll = php_base(ver)?.join("ext").join(format!("php_{name}.dll"));
    if dll.is_file() {
        fs::remove_file(&dll).map_err(e2s)?;
    }
    let mut s = load_settings();
    if let Some(m) = s.php_ext.get_mut(ver) {
        m.remove(name);
    }
    save_settings(&s)
}

fn valid_size(v: &str) -> bool {
    let v = v.trim();
    v == "-1" || (!v.is_empty() && v.len() < 12 && v.trim_end_matches(['K', 'M', 'G', 'k', 'm', 'g']).chars().all(|c| c.is_ascii_digit()) && v.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false))
}

pub fn set_php_conf(conf: PhpConf) -> Result<(), String> {
    for (label, v) in [
        ("Memory limit", &conf.memory_limit),
        ("Upload size", &conf.upload_max_filesize),
        ("Post size", &conf.post_max_size),
    ] {
        if !valid_size(v) {
            return Err(format!("{label} must be a number with an optional K, M or G (for example 256M)."));
        }
    }
    if conf.max_execution_time > 86400 || conf.max_input_vars == 0 || conf.max_input_vars > 1_000_000 {
        return Err("Execution time or input variables are out of range.".into());
    }
    if conf.timezone.is_empty() || !conf.timezone.chars().all(|c| c.is_ascii_alphanumeric() || "/_+-".contains(c)) {
        return Err("Timezone must look like UTC or Asia/Dhaka.".into());
    }
    if conf.error_reporting.is_empty() || !conf.error_reporting.chars().all(|c| c.is_ascii_alphanumeric() || " _|&~^()!".contains(c)) {
        return Err("Error reporting must look like E_ALL or E_ALL & ~E_DEPRECATED.".into());
    }
    let mut s = load_settings();
    s.php_conf = conf;
    save_settings(&s)
}

/// Download a PECL extension that matches this PHP version, put its DLL in place and switch it on.
pub fn install_php_extension(emit: &Emit, ver: &str, name: &str) -> Result<(), String> {
    let def_ = ext_def(name).ok_or("This extension is not in the download list.")?;
    let base = php_base(ver)?;
    let branch: String = ver.split('.').take(2).collect::<Vec<_>>().join(".");
    log(emit, "php", "info", format!("Looking for {name} for PHP {branch}..."));

    let listing = http_text(&format!("https://downloads.php.net/~windows/pecl/releases/{name}/"))?;
    let mut versions: Vec<String> = vec![];
    for (i, _) in listing.match_indices("href=\"") {
        let rest = &listing[i + 6..];
        let end = rest.find('"').unwrap_or(0);
        let v = rest[..end].trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
        let stable = v.split('.').count() == 3 && v.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
        if stable && !versions.contains(&v) {
            versions.push(v);
        }
    }
    versions.sort_by(|a, b| version_key(b).cmp(&version_key(a)));

    let mut found: Option<(String, String)> = None;
    'outer: for v in versions.iter().take(8) {
        for vc in ["vs17", "vs16", "vc15"] {
            let url = format!("https://downloads.php.net/~windows/pecl/releases/{name}/{v}/php_{name}-{v}-{branch}-nts-{vc}-x64.zip");
            if head_ok(&url) {
                found = Some((v.clone(), url));
                break 'outer;
            }
        }
    }
    let (v, url) = found.ok_or_else(|| format!("No Windows build of {name} was found for PHP {branch}."))?;

    fs::create_dir_all(root().join("downloads")).map_err(e2s)?;
    let zip_path = root().join("downloads").join(format!("php_{name}-{v}.zip"));
    log(emit, "php", "info", format!("Downloading {name} {v}..."));
    download_file(emit, "php-ext", name, &url, &zip_path)?;

    let f = fs::File::open(&zip_path).map_err(e2s)?;
    let mut ar = zip::ZipArchive::new(f).map_err(e2s)?;
    let mut placed = false;
    for i in 0..ar.len() {
        let mut file = ar.by_index(i).map_err(e2s)?;
        let Some(rel) = file.enclosed_name() else { continue };
        if file.is_dir() || rel.components().count() != 1 {
            continue;
        }
        let fname = rel.file_name().unwrap().to_string_lossy().to_string();
        if !fname.to_lowercase().ends_with(".dll") {
            continue;
        }
        // The extension itself goes to ext/, helper libraries next to php-cgi.exe.
        let target = if fname.starts_with("php_") { base.join("ext").join(&fname) } else { base.join(&fname) };
        let mut o = fs::File::create(&target).map_err(e2s)?;
        io::copy(&mut file, &mut o).map_err(e2s)?;
        if fname == format!("php_{name}.dll") {
            placed = true;
        }
    }
    let _ = fs::remove_file(&zip_path);
    if !placed {
        return Err(format!("The download did not contain php_{name}.dll."));
    }
    let mut s = load_settings();
    s.php_ext.entry(ver.to_string()).or_default().insert(name.to_string(), true);
    save_settings(&s)?;
    let _ = def_;
    log(emit, "php", "ok", format!("{name} {v} installed for PHP {ver} and switched on"));
    Ok(())
}

const NGINX_TPL: &str = r#"worker_processes 1;
error_log "@RUN@/logs/error.log";
pid "@RUN@/nginx.pid";
events { worker_connections 1024; }
http {
    include "@CONF@/mime.types";
    default_type application/octet-stream;
    access_log "@RUN@/logs/access.log";
    sendfile off;
    client_max_body_size 128m;
    server_names_hash_bucket_size 128;
    client_body_temp_path "@RUN@/temp/client_body";
    proxy_temp_path "@RUN@/temp/proxy";
    fastcgi_temp_path "@RUN@/temp/fastcgi";

    server {
        listen @PORT@ default_server;
        server_name localhost;
        root "@WWW@";
        autoindex on;
        index index.php index.html;
        location / { try_files $uri $uri/ =404; }
        location ~ \.php$ {
            fastcgi_pass 127.0.0.1:@PHP@;
            include "@CONF@/fastcgi_params";
            fastcgi_param SCRIPT_FILENAME $document_root$fastcgi_script_name;
        }
    }
@VHOSTS@
}
"#;

const VHOST_TPL: &str = r#"
    server {
        listen @PORT@;
@SSL@        server_name @DOMAIN@ *.@DOMAIN@;
        root "@ROOT@";
        index index.php index.html;
        location / { try_files $uri $uri/ /index.php?$query_string; }
        location ~ \.php$ {
            fastcgi_pass 127.0.0.1:@PHP@;
            include "@CONF@/fastcgi_params";
            fastcgi_param SCRIPT_FILENAME $document_root$fastcgi_script_name;
            fastcgi_param HTTPS @HTTPSFLAG@;
        }
    }
"#;

fn write_nginx_conf(base: &Path, run: &Path, s: &Settings) -> Result<PathBuf, String> {
    for d in ["logs", "temp/client_body", "temp/proxy", "temp/fastcgi"] {
        fs::create_dir_all(run.join(d)).map_err(e2s)?;
    }
    let conf_dir = fwd(&base.join("conf"));
    let port = port_of(s, "nginx").to_string();
    let default_php = active_of(s, "php");
    let installed = versions("php");
    let mut vhosts = String::new();
    for site in sites() {
        let ver = site
            .php
            .clone()
            .filter(|v| installed.contains(v))
            .or_else(|| default_php.clone());
        let php_port = ver.map(|v| php_port_for(&v)).unwrap_or_else(|| port_of(s, "php"));
        let (crt, key) = cert_paths(&site.domain);
        let use_ssl = site.ssl && crt.is_file() && key.is_file();
        let ssl_lines = if use_ssl {
            format!(
                "        listen {} ssl;\n        ssl_certificate \"{}\";\n        ssl_certificate_key \"{}\";\n",
                https_port(s),
                fwd(&crt),
                fwd(&key)
            )
        } else {
            String::new()
        };
        vhosts.push_str(
            &VHOST_TPL
                .replace("@PORT@", &port)
                .replace("@SSL@", &ssl_lines)
                .replace("@HTTPSFLAG@", if use_ssl { "$https" } else { "off" })
                .replace("@DOMAIN@", &site.domain)
                .replace("@ROOT@", &fwd(Path::new(&site.doc_root)))
                .replace("@PHP@", &php_port.to_string())
                .replace("@CONF@", &conf_dir),
        );
    }
    let default_port = default_php
        .map(|v| php_port_for(&v))
        .unwrap_or_else(|| port_of(s, "php"));
    let text = NGINX_TPL
        .replace("@VHOSTS@", &vhosts)
        .replace("@RUN@", &fwd(run))
        .replace("@CONF@", &conf_dir)
        .replace("@PORT@", &port)
        .replace("@PHP@", &default_port.to_string())
        .replace("@WWW@", &fwd(&www_dir()));
    let path = run.join("nginx.conf");
    fs::write(&path, text).map_err(e2s)?;
    Ok(path)
}

/// Apache's configuration: one virtual host per site (with .htaccess support), PHP through FastCGI.
fn write_apache_conf(base: &Path, run: &Path, s: &Settings) -> Result<PathBuf, String> {
    fs::create_dir_all(run.join("logs")).map_err(e2s)?;
    let port = port_of(s, "apache");
    let https = https_port(s);
    let default_php = active_of(s, "php");
    let installed = versions("php");
    let php_port_for_site = |ver: Option<String>| -> u16 {
        ver.filter(|v| installed.contains(v))
            .or_else(|| default_php.clone())
            .map(|v| php_port_for(&v))
            .unwrap_or_else(|| port_of(s, "php"))
    };
    let www = fwd(&www_dir());
    let all = sites();
    let ssl_sites: Vec<&Site> = all
        .iter()
        .filter(|x| {
            let (crt, key) = cert_paths(&x.domain);
            x.ssl && crt.is_file() && key.is_file()
        })
        .collect();

    // Load only the modules this Apache build really contains.
    let mut modules = vec![
        ("authn_core_module", "mod_authn_core.so"),
        ("authz_core_module", "mod_authz_core.so"),
        ("authz_host_module", "mod_authz_host.so"),
        ("access_compat_module", "mod_access_compat.so"),
        ("alias_module", "mod_alias.so"),
        ("autoindex_module", "mod_autoindex.so"),
        ("deflate_module", "mod_deflate.so"),
        ("dir_module", "mod_dir.so"),
        ("env_module", "mod_env.so"),
        ("expires_module", "mod_expires.so"),
        ("filter_module", "mod_filter.so"),
        ("headers_module", "mod_headers.so"),
        ("log_config_module", "mod_log_config.so"),
        ("mime_module", "mod_mime.so"),
        ("negotiation_module", "mod_negotiation.so"),
        ("proxy_module", "mod_proxy.so"),
        ("proxy_fcgi_module", "mod_proxy_fcgi.so"),
        ("rewrite_module", "mod_rewrite.so"),
        ("setenvif_module", "mod_setenvif.so"),
    ];
    if !ssl_sites.is_empty() {
        modules.push(("socache_shmcb_module", "mod_socache_shmcb.so"));
        modules.push(("ssl_module", "mod_ssl.so"));
    }

    let mut c = String::new();
    c.push_str(&format!("ServerRoot \"{}\"\n", fwd(base)));
    c.push_str(&format!("PidFile \"{}/httpd.pid\"\n", fwd(run)));
    c.push_str(&format!("Listen {port}\n"));
    if !ssl_sites.is_empty() {
        c.push_str(&format!("Listen {https}\n"));
    }
    c.push_str(&format!("ServerName localhost:{port}\nServerAdmin dev@jojo.test\nServerTokens Prod\nServerSignature Off\n"));
    c.push_str("Timeout 300\nKeepAlive On\nEnableSendfile Off\nEnableMMAP Off\n\n");
    for (name, file) in &modules {
        if base.join("modules").join(file).is_file() {
            c.push_str(&format!("LoadModule {name} modules/{file}\n"));
        }
    }
    c.push_str(&format!(
        "\nErrorLog \"{run}/logs/error.log\"\nLogLevel warn\n<IfModule log_config_module>\n    LogFormat \"%h %l %u %t \\\"%r\\\" %>s %b\" common\n    CustomLog \"{run}/logs/access.log\" common\n</IfModule>\n<IfModule mime_module>\n    TypesConfig \"{base}/conf/mime.types\"\n</IfModule>\n",
        run = fwd(run),
        base = fwd(base)
    ));
    c.push_str("\nDirectoryIndex index.php index.html\nProxyFCGIBackendType GENERIC\n");
    // On Windows the script path reaches PHP as "/D:/..." (leading slash) and PHP answers "No input file
    // specified". Build the real path from the site's document root instead.
    c.push_str("ProxyFCGISetEnvIf \"true\" SCRIPT_FILENAME \"%{reqenv:DOCUMENT_ROOT}%{reqenv:SCRIPT_NAME}\"\n");
    c.push_str("\n<Directory />\n    AllowOverride None\n    Require all denied\n</Directory>\n");
    // The whole projects folder: .htaccess files work (Laravel, WordPress and others rely on them).
    c.push_str(&format!(
        "\n<Directory \"{www}\">\n    Options Indexes FollowSymLinks\n    AllowOverride All\n    Require all granted\n</Directory>\n"
    ));
    if !ssl_sites.is_empty() {
        c.push_str(&format!("\nSSLSessionCache \"shmcb:{}/logs/ssl_scache(512000)\"\nSSLSessionCacheTimeout 300\n", fwd(run)));
    }

    let php_handler = |p: u16| format!("    <FilesMatch \"\\.php$\">\n        SetHandler \"proxy:fcgi://127.0.0.1:{p}/\"\n    </FilesMatch>\n");
    // the default site (http://localhost)
    c.push_str(&format!(
        "\n<VirtualHost *:{port}>\n    ServerName localhost\n    DocumentRoot \"{www}\"\n{}</VirtualHost>\n",
        php_handler(php_port_for_site(None))
    ));
    for site in &all {
        let php = php_port_for_site(site.php.clone());
        let root = fwd(Path::new(&site.doc_root));
        c.push_str(&format!(
            "\n<VirtualHost *:{port}>\n    ServerName {d}\n    ServerAlias *.{d}\n    DocumentRoot \"{root}\"\n{h}</VirtualHost>\n",
            d = site.domain,
            h = php_handler(php)
        ));
    }
    for site in ssl_sites {
        let (crt, key) = cert_paths(&site.domain);
        let php = php_port_for_site(site.php.clone());
        let root = fwd(Path::new(&site.doc_root));
        c.push_str(&format!(
            "\n<VirtualHost *:{https}>\n    ServerName {d}\n    ServerAlias *.{d}\n    DocumentRoot \"{root}\"\n    SSLEngine on\n    SSLCertificateFile \"{crt}\"\n    SSLCertificateKeyFile \"{key}\"\n    SetEnv HTTPS on\n{h}</VirtualHost>\n",
            d = site.domain,
            crt = fwd(&crt),
            key = fwd(&key),
            h = php_handler(php)
        ));
    }
    let path = run.join("httpd.conf");
    fs::write(&path, c).map_err(e2s)?;
    Ok(path)
}

// ---------------------------------------------------------------- running services

pub(crate) struct Running {
    pub(crate) child: Child,
    pub(crate) started: Instant,
    pub(crate) version: String,
}

#[derive(Clone)]
pub struct Core {
    pub(crate) running: Arc<Mutex<HashMap<String, Running>>>,
    pub(crate) emit: Emit,
    /// site name -> public URL, for sites that are shared on the internet right now
    pub(crate) shares: Arc<Mutex<HashMap<String, String>>>,
    pub(crate) dns: Arc<Mutex<Option<crate::dns::DnsHandle>>>,
}

#[derive(Serialize)]
pub struct SvcInfo {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub versions: Vec<String>,
    pub active: Option<String>,
    pub running: bool,
    pub uptime: u64,
    pub pid: Option<u32>,
}

#[derive(Serialize)]
pub struct Overview {
    pub root: String,
    pub services: Vec<SvcInfo>,
    pub sites: Vec<Site>,
    pub hosts_ok: bool,
    pub https_port: u16,
    pub ssl_ready: bool,
    pub adminer: bool,
    pub tld: String,
    pub smtp_port: u16,
    pub web_server: String,
    pub composer: bool,
    pub dbgate: bool,
    pub www_dir: String,
    pub general: General,
    pub php_conf: PhpConf,
    pub dns: DnsInfo,
    pub shares: HashMap<String, String>,
    pub workers: Vec<crate::workers::WorkerInfo>,
}

#[derive(Serialize)]
pub struct DnsInfo {
    pub enabled: bool,
    pub applied_tld: Option<String>,
    pub running: bool,
}

fn instance_key(id: &str, ver: &str) -> String {
    if id == "php" {
        format!("php@{ver}")
    } else {
        id.to_string()
    }
}

fn not_installed(name: &str) -> String {
    format!("{name} is not installed. Open the Versions tab to install it.")
}

impl Core {
    pub fn new(emit: Emit) -> Self {
        Core {
            running: Arc::new(Mutex::new(HashMap::new())),
            emit,
            shares: Arc::new(Mutex::new(HashMap::new())),
            dns: Arc::new(Mutex::new(None)),
        }
    }

    pub fn emit(&self) -> &Emit {
        &self.emit
    }

    pub(crate) fn keys(&self, id: &str) -> Vec<String> {
        let prefix = format!("{id}@");
        self.running
            .lock()
            .unwrap()
            .keys()
            .filter(|k| *k == id || k.starts_with(&prefix))
            .cloned()
            .collect()
    }

    pub(crate) fn alive(&self, key: &str) -> bool {
        let mut g = self.running.lock().unwrap();
        let alive = match g.get_mut(key) {
            Some(r) => matches!(r.child.try_wait(), Ok(None)),
            None => return false,
        };
        if !alive {
            g.remove(key);
        }
        alive
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.keys(id).iter().any(|k| self.alive(k))
    }

    pub fn overview(&self) -> Overview {
        self.prune_shares();
        let s = load_settings();
        let services = DEFS
            .iter()
            .chain(std::iter::once(&NODE_DEF))
            .map(|d| {
                let running = self.is_running(d.id);
                let (uptime, pid, live_ver) = {
                    let g = self.running.lock().unwrap();
                    let prefix = format!("{}@", d.id);
                    let mut up = 0;
                    let mut pid = None;
                    let mut ver = None;
                    for (k, r) in g.iter() {
                        if k == d.id || k.starts_with(&prefix) {
                            let e = r.started.elapsed().as_secs();
                            if pid.is_none() || e > up {
                                up = e;
                                pid = Some(r.child.id());
                                ver = Some(r.version.clone());
                            }
                        }
                    }
                    (up, pid, ver)
                };
                let active = if d.id == "php" { active_of(&s, d.id) } else { live_ver.or_else(|| active_of(&s, d.id)) };
                SvcInfo {
                    id: d.id.into(),
                    name: d.name.into(),
                    port: port_of(&s, d.id),
                    versions: versions(d.id),
                    active,
                    running,
                    uptime: if running { uptime } else { 0 },
                    pid: if running { pid } else { None },
                }
            })
            .collect();
        Overview {
            root: root().to_string_lossy().to_string(),
            services,
            sites: sites(),
            hosts_ok: hosts_in_sync(),
            https_port: https_port(&s),
            ssl_ready: s.ssl_ready,
            adminer: www_dir().join("adminer").join("adminer.php").is_file(),
            www_dir: www_dir().to_string_lossy().to_string(),
            composer: composer_installed(),
            dbgate: crate::tools::dbgate_exe().is_some(),
            smtp_port: smtp_port_of(&s),
            web_server: active_web(&s),
            tld: tld_of(&s),
            general: s.general.clone(),
            php_conf: s.php_conf.clone(),
            dns: DnsInfo { enabled: s.dns.enabled, applied_tld: s.dns.applied_tld.clone(), running: self.dns_running() },
            shares: self.shares.lock().unwrap().clone(),
            workers: self.workers_overview(),
        }
    }

    pub fn start(&self, id: &str) -> Result<(), String> {
        let d = def(id).ok_or("Unknown service.")?;
        if id == "node" {
            return Err("Node.js is a tool, not a service. Use it from a Terminal.".into());
        }
        if id == "php" {
            return self.start_php();
        }
        if self.is_running(id) {
            return Ok(());
        }
        let s = load_settings();
        let ver = active_of(&s, id).ok_or_else(|| not_installed(d.name))?;
        if WEB_SERVERS.contains(&id) {
            // Nginx and Apache share the web port: starting one replaces the other.
            let other = if id == "nginx" { "apache" } else { "nginx" };
            if self.is_running(other) {
                log(&self.emit, id, "info", format!("Switching web server: stopping {other} first."));
                self.stop(other)?;
            }
            if s.web_server.as_deref() != Some(id) {
                let mut s2 = load_settings();
                s2.web_server = Some(id.to_string());
                let _ = save_settings(&s2);
            }
        }
        self.launch(id, &ver)
    }

    /// The web server that is running now (Nginx or Apache), if any.
    pub fn running_web(&self) -> Option<&'static str> {
        WEB_SERVERS.iter().copied().find(|w| self.is_running(w))
    }

    pub fn web_running(&self) -> bool {
        self.running_web().is_some()
    }

    /// Restart whichever web server is running (so it reads new site or PHP settings).
    pub fn restart_web(&self) -> Result<(), String> {
        match self.running_web() {
            Some(w) => self.restart(w),
            None => Ok(()),
        }
    }

    /// Choose the web server for Start All. If the other one is running now, it is replaced.
    pub fn set_web_server(&self, id: &str) -> Result<(), String> {
        if !WEB_SERVERS.contains(&id) {
            return Err("Unknown web server.".into());
        }
        if versions(id).is_empty() {
            return Err(format!("{} is not installed. Install it from the Versions tab first.", def(id).map(|d| d.name).unwrap_or(id)));
        }
        let was_running = self.running_web();
        let mut s = load_settings();
        s.web_server = Some(id.to_string());
        save_settings(&s)?;
        if was_running.is_some() && was_running != Some(id) {
            self.start(id)?; // stops the other one first
        }
        Ok(())
    }

    /// Start the default PHP version plus every version that a site asks for.
    fn start_php(&self) -> Result<(), String> {
        let s = load_settings();
        let default = active_of(&s, "php").ok_or_else(|| not_installed("PHP"))?;
        let installed = versions("php");
        let mut want = vec![default];
        for site in sites() {
            if let Some(v) = site.php {
                if installed.contains(&v) && !want.contains(&v) {
                    want.push(v);
                }
            }
        }
        for (i, v) in want.iter().enumerate() {
            if self.alive(&instance_key("php", v)) {
                continue;
            }
            if let Err(e) = self.launch("php", v) {
                if i == 0 {
                    return Err(e);
                }
                log(&self.emit, "php", "err", e);
            }
        }
        Ok(())
    }

    fn launch(&self, id: &str, ver: &str) -> Result<(), String> {
        let emit = &self.emit;
        let d = def(id).ok_or("Unknown service.")?;
        let s = load_settings();
        let vdir = root().join("bin").join(id).join(ver);
        let base = base_dir(&vdir, d).ok_or("Could not find the program files for this version.")?;
        let exe = exe_in(&base, d).ok_or("Could not find the executable for this version.")?;
        let port = if id == "php" { php_port_for(ver) } else { port_of(&s, id) };
        let key = instance_key(id, ver);
        let data = |name: &str| root().join("data").join(name);

        // Something else already listens on this port: say so instead of starting a process that
        // would fail (or, worse, silently let the old program keep serving old settings).
        // (A program that was just stopped may need a moment to release its port.)
        let mut waited = 0;
        while port != 0 && port_in_use(port) && waited < 10 {
            std::thread::sleep(Duration::from_millis(200));
            waited += 1;
        }
        if port != 0 && port_in_use(port) {
            return Err(format!(
                "Port {port} is already in use by another program, so {} cannot start. Close that program, or change the port on the {} tile.",
                d.name, d.name
            ));
        }

        let mut cmd = Command::new(&exe);
        cmd.current_dir(&base);

        match id {
            "nginx" => {
                let run = root().join("run").join("nginx");
                let conf = write_nginx_conf(&base, &run, &s)?;
                if !hosts_in_sync() {
                    log(emit, id, "warn", "The hosts file is out of date. Open the Sites tab and click 'Sync hosts'.");
                }
                cmd.arg("-p").arg(format!("{}/", fwd(&run))).arg("-c").arg(fwd(&conf));
            }
            "apache" => {
                let run = root().join("run").join("apache");
                let conf = write_apache_conf(&base, &run, &s)?;
                if !hosts_in_sync() {
                    log(emit, id, "warn", "The hosts file is out of date. Open the Sites tab and click 'Sync hosts'.");
                }
                // Let Apache check its own configuration first, so a mistake is reported in plain words.
                let mut check = Command::new(&exe);
                check.current_dir(&base).arg("-t").arg("-f").arg(fwd(&conf));
                hide(&mut check);
                let out = check.output().map_err(e2s)?;
                if !out.status.success() {
                    let msg = String::from_utf8_lossy(&out.stderr).to_string();
                    let first: Vec<&str> = msg.lines().filter(|l| !l.trim().is_empty()).take(2).collect();
                    return Err(format!("Apache rejected its configuration: {}", if first.is_empty() { "unknown error".to_string() } else { first.join(" ") }));
                }
                cmd.arg("-f").arg(fwd(&conf));
            }
            "php" => {
                let ini = write_php_ini(&base, ver)?;
                cmd.arg("-b").arg(format!("127.0.0.1:{port}")).arg("-c").arg(ini);
                cmd.env("PHP_FCGI_MAX_REQUESTS", "0");
            }
            "mysql" => {
                let dd = data(&format!("mysql-{ver}"));
                if !dd.join("mysql").exists() {
                    log(emit, id, "info", "Initializing the MySQL data folder (this can take up to 30 seconds)...");
                    fs::create_dir_all(&dd).map_err(e2s)?;
                    let mut init = Command::new(&exe);
                    init.current_dir(&base)
                        .arg("--initialize-insecure")
                        .arg(format!("--basedir={}", fwd(&base)))
                        .arg(format!("--datadir={}", fwd(&dd)));
                    hide(&mut init);
                    let out = init.output().map_err(e2s)?;
                    if !out.status.success() {
                        let msg = String::from_utf8_lossy(&out.stderr).to_string();
                        return Err(format!("MySQL initialization failed: {}", msg.lines().last().unwrap_or("")));
                    }
                    log(emit, id, "info", "MySQL initialized (user 'root', empty password).");
                }
                cmd.arg(format!("--basedir={}", fwd(&base)))
                    .arg(format!("--datadir={}", fwd(&dd)))
                    .arg(format!("--port={port}"))
                    .arg("--bind-address=127.0.0.1")
                    .arg("--mysqlx=OFF")
                    .arg("--skip-log-bin")
                    .arg("--console");
            }
            "postgres" => {
                let dd = data(&format!("pgsql-{ver}"));
                if !dd.join("PG_VERSION").exists() {
                    log(emit, id, "info", "Initializing the PostgreSQL data folder...");
                    fs::create_dir_all(&dd).map_err(e2s)?;
                    let mut init = Command::new(base.join("bin").join("initdb.exe"));
                    init.current_dir(&base)
                        .args(["-U", "postgres", "-A", "trust", "-E", "UTF8", "-D"])
                        .arg(&dd);
                    hide(&mut init);
                    let out = init.output().map_err(e2s)?;
                    if !out.status.success() {
                        let msg = String::from_utf8_lossy(&out.stderr).to_string();
                        return Err(format!("initdb failed: {}", msg.lines().last().unwrap_or("")));
                    }
                    log(emit, id, "info", "PostgreSQL initialized (user 'postgres', no password).");
                }
                cmd.arg("-D")
                    .arg(&dd)
                    .arg("-p")
                    .arg(port.to_string())
                    .arg("-c")
                    .arg("listen_addresses=127.0.0.1");
            }
            "redis" => {
                let dd = data("redis");
                fs::create_dir_all(&dd).map_err(e2s)?;
                cmd.arg("--port")
                    .arg(port.to_string())
                    .arg("--bind")
                    .arg("127.0.0.1")
                    .arg("--dir")
                    .arg(&dd);
            }
            "mailpit" => {
                let dd = data("mailpit");
                fs::create_dir_all(&dd).map_err(e2s)?;
                cmd.arg("--listen")
                    .arg(format!("127.0.0.1:{port}"))
                    .arg("--smtp")
                    .arg(format!("127.0.0.1:{}", smtp_port_of(&s)))
                    .arg("--database")
                    .arg(dd.join("mailpit.db"));
            }
            _ => {}
        }

        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        hide(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("Could not start {}: {e}", d.name))?;
        if let Some(o) = child.stdout.take() {
            pipe_logs(emit.clone(), id.to_string(), o);
        }
        if let Some(o) = child.stderr.take() {
            pipe_logs(emit.clone(), id.to_string(), o);
        }

        std::thread::sleep(Duration::from_millis(900));
        if let Ok(Some(status)) = child.try_wait() {
            // 0xC0000135: a DLL the program needs is not installed.
            if status.code() == Some(-1073741515) {
                return Err(format!(
                    "{} cannot start because a Windows component is missing. Install the Microsoft Visual C++ 2015-2026 Redistributable (x64) from https://aka.ms/vs/17/release/vc_redist.x64.exe and try again.",
                    d.name
                ));
            }
            return Err(format!(
                "{} exited right after starting ({status}). Another program may be using port {port}. Check the log for details.",
                d.name
            ));
        }
        // "Started" should mean "ready": databases need a few seconds before they accept connections.
        if port != 0 {
            let mut waited = 0;
            while !port_in_use(port) && waited < 75 {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(format!("{} stopped while starting ({status}). Check the log for details.", d.name));
                }
                std::thread::sleep(Duration::from_millis(200));
                waited += 1;
            }
            if !port_in_use(port) {
                log(emit, id, "warn", format!("{} is running but is still not answering on port {port}.", d.name));
            }
        }
        log(emit, id, "ok", format!("{} {} started on port {}", d.name, ver, port));
        self.running
            .lock()
            .unwrap()
            .insert(key, Running { child, started: Instant::now(), version: ver.to_string() });
        Ok(())
    }

    pub fn stop(&self, id: &str) -> Result<(), String> {
        for key in self.keys(id) {
            self.stop_key(&key);
        }
        Ok(())
    }

    fn stop_key(&self, key: &str) {
        let taken = self.running.lock().unwrap().remove(key);
        let Some(mut r) = taken else { return };
        let id = key.split('@').next().unwrap_or(key);
        graceful(id, &r.version);
        for _ in 0..40 {
            if matches!(r.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if matches!(r.child.try_wait(), Ok(None)) {
            crate::workers::kill_tree(r.child.id());
            let _ = r.child.kill();
            let _ = r.child.wait();
        }
        let name = def(id).map(|d| d.name).unwrap_or(id);
        log(&self.emit, id, "warn", format!("{name} {} stopped", r.version));
    }

    pub fn restart(&self, id: &str) -> Result<(), String> {
        self.stop(id)?;
        self.start(id)
    }

    pub fn start_all(&self) {
        let web = active_web(&load_settings());
        for d in DEFS.iter() {
            if versions(d.id).is_empty() {
                continue;
            }
            if WEB_SERVERS.contains(&d.id) && d.id != web {
                continue; // only one web server runs
            }
            if let Err(e) = self.start(d.id) {
                log(&self.emit, d.id, "err", e);
            }
        }
    }

    /// Stop and start again every service that is running right now (in dependency order).
    pub fn restart_running(&self) {
        let running: Vec<&str> = DEFS.iter().map(|d| d.id).filter(|id| self.is_running(id)).collect();
        for id in running.iter().rev() {
            let _ = self.stop(id);
        }
        for id in running {
            if let Err(e) = self.start(id) {
                log(&self.emit, id, "err", e);
            }
        }
    }

    pub fn stop_all(&self) {
        self.stop_shares();
        self.stop_workers();
        for d in DEFS.iter().rev() {
            let _ = self.stop(d.id);
        }
    }

    /// Restart PHP (if it is running) so a changed php.ini takes effect.
    pub fn reload_php(&self) -> Result<(), String> {
        if self.is_running("php") {
            self.stop("php")?;
            self.start("php")?;
        }
        Ok(())
    }

    /// After site settings change: launch any missing PHP versions and reload Nginx.
    pub fn apply_site_changes(&self) -> Result<(), String> {
        if self.is_running("php") {
            self.start_php()?;
        }
        self.restart_web()?;
        Ok(())
    }

    pub fn remove_version(&self, id: &str, version: &str) -> Result<(), String> {
        if def(id).is_none() || !valid_version(version) {
            return Err("Invalid request.".into());
        }
        {
            let g = self.running.lock().unwrap();
            let prefix = format!("{id}@");
            if g.iter().any(|(k, r)| (k == id || k.starts_with(&prefix)) && r.version == version) {
                return Err("This version is running. Stop it first.".into());
            }
        }
        fs::remove_dir_all(root().join("bin").join(id).join(version)).map_err(e2s)
    }

    // ---- downloads

    pub fn install(&self, id: &str, version: &str, url: &str) -> Result<(), String> {
        let emit = &self.emit;
        let d = def(id).ok_or("Unknown service.")?;
        if !valid_version(version) {
            return Err("Invalid version. Use letters, numbers, '.', '-' and '_' only.".into());
        }
        if !url.starts_with("https://") && !url.starts_with("http://") {
            return Err("The URL must start with http:// or https://".into());
        }
        let final_dir = root().join("bin").join(id).join(version);
        if final_dir.exists() {
            return Err(format!("{} {} is already installed.", d.name, version));
        }
        fs::create_dir_all(root().join("downloads")).map_err(e2s)?;
        let zip_path = root().join("downloads").join(format!("{id}-{version}.zip"));

        log(emit, id, "info", format!("Downloading {} {}...", d.name, version));
        let received = download_file(emit, id, version, url, &zip_path)?;

        // Verify the checksum when the download site publishes one (PHP does).
        if let Some(want) = catalog().iter().find(|c| c.url == url).and_then(|c| c.sha256.clone()) {
            let got = sha256_file(&zip_path)?;
            if !got.eq_ignore_ascii_case(&want) {
                let _ = fs::remove_file(&zip_path);
                return Err("The downloaded file failed its checksum check, so it was discarded. Try again.".into());
            }
            log(emit, id, "ok", "Checksum verified");
        }

        emit_progress(emit, id, version, received, received, "extract");
        log(emit, id, "info", "Extracting...");
        let tmp = root().join("bin").join(id).join(format!(".tmp-{version}"));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(e2s)?;
        if let Err(e) = extract_zip(&zip_path, &tmp) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(format!("Extraction failed: {e}"));
        }
        if base_dir(&tmp, d).is_none() {
            let _ = fs::remove_dir_all(&tmp);
            return Err(format!(
                "The ZIP does not contain {}. Is this the Windows x64 ZIP for {}?",
                d.exes[0], d.name
            ));
        }
        fs::rename(&tmp, &final_dir).map_err(e2s)?;
        let _ = fs::remove_file(&zip_path);
        emit_progress(emit, id, version, received, received, "done");
        log(emit, id, "ok", format!("{} {} installed", d.name, version));
        Ok(())
    }

    /// Install Nginx, PHP and MySQL (if missing) and Adminer. Keeps going if one step fails.
    pub fn install_recommended(&self) -> Result<(), String> {
        let cat = catalog();
        let mut failures = vec![];
        for (svc, ver) in RECOMMENDED {
            if !versions(svc).is_empty() {
                continue;
            }
            let Some(entry) = cat.iter().find(|c| c.service == svc && c.version == ver) else {
                failures.push(format!("{svc} {ver} is missing from the catalog"));
                continue;
            };
            if let Err(e) = self.install(svc, ver, &entry.url) {
                log(&self.emit, svc, "err", e.clone());
                failures.push(e);
            }
        }
        if let Err(e) = self.install_node_lts() {
            log(&self.emit, "node", "err", e.clone());
            failures.push(e);
        }
        if !composer_installed() {
            if let Err(e) = self.install_composer() {
                log(&self.emit, "composer", "err", e.clone());
                failures.push(e);
            }
        }
        if let Err(e) = self.install_adminer() {
            log(&self.emit, "adminer", "err", e.clone());
            failures.push(e);
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!("Some steps failed: {}", failures.join(" | ")))
        }
    }

    /// Make sure `composer` exists whenever PHP does. Quiet when there is nothing to do or no internet.
    pub fn ensure_composer(&self) {
        if composer_installed() || versions("php").is_empty() {
            return;
        }
        if let Err(e) = self.install_composer() {
            log(&self.emit, "composer", "warn", format!("Could not install Composer automatically: {e}"));
        }
    }

    /// Composer (PHP package manager): downloads composer.phar, checks its SHA-256 and adds a
    /// `composer` command that runs on whichever PHP the terminal is using.
    pub fn install_composer(&self) -> Result<(), String> {
        let emit = &self.emit;
        let dir = composer_dir();
        fs::create_dir_all(&dir).map_err(e2s)?;
        log(emit, "composer", "info", "Downloading Composer...");
        let want = http_text("https://getcomposer.org/download/latest-stable/composer.phar.sha256sum")?
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_lowercase();
        let part = dir.join("composer.phar.part");
        download_file(emit, "composer", "latest", "https://getcomposer.org/download/latest-stable/composer.phar", &part)?;
        let got = sha256_file(&part)?;
        if want.len() != 64 || got != want {
            let _ = fs::remove_file(&part);
            return Err("The downloaded Composer file failed its checksum check, so it was discarded. Try again.".into());
        }
        let phar = dir.join("composer.phar");
        let _ = fs::remove_file(&phar);
        fs::rename(&part, &phar).map_err(e2s)?;
        fs::write(dir.join("composer.bat"), "@echo off\r\nphp \"%~dp0composer.phar\" %*\r\n").map_err(e2s)?;
        log(emit, "composer", "ok", "Composer installed. Open a Terminal and type: composer --version");
        Ok(())
    }

    /// Adminer: one-file database manager (MySQL, PostgreSQL, SQLite), served at adminer.test.
    pub fn install_adminer(&self) -> Result<(), String> {
        let dir = www_dir().join("adminer");
        if dir.join("adminer.php").is_file() {
            return Ok(());
        }
        fs::create_dir_all(&dir).map_err(e2s)?;
        log(&self.emit, "adminer", "info", "Downloading Adminer...");
        download_file(&self.emit, "adminer", "4.17.1", ADMINER_URL, &dir.join("adminer.php"))?;
        let wrapper = r#"<?php
// JOJO Server: allow passwordless logins to the local dev databases (127.0.0.1 only).
function adminer_object() {
    class JojoAdminer extends Adminer {
        function login($login, $password) { return true; }
    }
    return new JojoAdminer();
}
include __DIR__ . '/adminer.php';
"#;
        fs::write(dir.join("index.php"), wrapper).map_err(e2s)?;
        log(&self.emit, "adminer", "ok", "Adminer installed. Open the 'adminer' site (use server 127.0.0.1:<port>).");
        Ok(())
    }
}

fn graceful(id: &str, ver: &str) {
    let s = load_settings();
    let Some(d) = def(id) else { return };
    let Some(base) = base_dir(&root().join("bin").join(id).join(ver), d) else { return };
    match id {
        "nginx" => {
            let run = root().join("run").join("nginx");
            if let Some(exe) = exe_in(&base, d) {
                let mut c = Command::new(exe);
                c.current_dir(&base)
                    .arg("-p")
                    .arg(format!("{}/", fwd(&run)))
                    .arg("-c")
                    .arg(fwd(&run.join("nginx.conf")))
                    .arg("-s")
                    .arg("stop");
                run_quiet(&mut c);
            }
        }
        "apache" => {
            let conf = root().join("run").join("apache").join("httpd.conf");
            if let Some(exe) = exe_in(&base, d) {
                let mut c = Command::new(exe);
                c.current_dir(&base).arg("-k").arg("shutdown").arg("-f").arg(fwd(&conf));
                run_quiet(&mut c);
            }
        }
        "postgres" => {
            let dd = root().join("data").join(format!("pgsql-{ver}"));
            let mut c = Command::new(base.join("bin").join("pg_ctl.exe"));
            c.arg("stop").arg("-D").arg(dd).args(["-m", "fast", "-w", "-t", "8"]);
            run_quiet(&mut c);
        }
        "mysql" => {
            let mut c = Command::new(base.join("bin").join("mysqladmin.exe"));
            c.args(["-u", "root", "-h", "127.0.0.1"])
                .arg(format!("-P{}", port_of(&s, "mysql")))
                .arg("shutdown");
            run_quiet(&mut c);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- catalog

#[derive(Serialize, Deserialize, Clone)]
pub struct CatalogEntry {
    pub service: String,
    pub version: String,
    pub url: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

fn builtin_catalog() -> Vec<CatalogEntry> {
    let e = |s: &str, v: &str, u: &str, n: &str| CatalogEntry {
        service: s.into(),
        version: v.into(),
        url: u.into(),
        note: n.into(),
        sha256: None,
    };
    vec![
        e("nginx", "1.26.2", "https://nginx.org/download/nginx-1.26.2.zip", "Official build"),
        entry(
            "apache",
            "2.4.69",
            "https://www.apachelounge.com/download/VS18/binaries/httpd-2.4.69-261002-Win64-VS18.zip",
            "Apache Lounge build (needs the Visual C++ 2015-2026 runtime)",
            Some("77d715d0d9cee731e2251e7ddc2090a7a8e6d41fedef2e9827cd918593c5a652".to_string()),
        ),
        e("php", "8.4.1", "https://windows.php.net/downloads/releases/archives/php-8.4.1-nts-Win32-vs17-x64.zip", "Non-thread-safe build (for FastCGI)"),
        e("php", "8.3.12", "https://windows.php.net/downloads/releases/archives/php-8.3.12-nts-Win32-vs16-x64.zip", "Non-thread-safe build (for FastCGI)"),
        e("php", "8.2.24", "https://windows.php.net/downloads/releases/archives/php-8.2.24-nts-Win32-vs16-x64.zip", "Non-thread-safe build"),
        e("mysql", "8.4.2", "https://cdn.mysql.com/archives/mysql-8.4/mysql-8.4.2-winx64.zip", "Official ZIP (large, about 250 MB)"),
        e("postgres", "16.4", "https://get.enterprisedb.com/postgresql/postgresql-16.4-1-windows-x64-binaries.zip", "EDB binaries ZIP"),
        e("redis", "5.0.14.1", "https://github.com/tporadowski/redis/releases/download/v5.0.14.1/Redis-x64-5.0.14.1.zip", "Windows port by tporadowski"),
        e("mailpit", "1.20.4", "https://github.com/axllent/mailpit/releases/download/v1.20.4/mailpit-windows-amd64.zip", "Local mail catcher"),
        // Fallback until the live list from nodejs.org has been fetched. npm 11 (Node 24) is needed for
        // current Laravel front ends: the npm that comes with Node 22.11 fails on them.
        e("node", "24.22.1", "https://nodejs.org/dist/v24.22.1/node-v24.22.1-win-x64.zip", "LTS (long-term support)"),
    ]
}

fn catalog_path() -> PathBuf {
    root().join("catalog.json")
}

/// Discovered entries (from the last "check for new versions"), built-in entries and custom catalog.json entries.
pub fn catalog() -> Vec<CatalogEntry> {
    let mut all: Vec<CatalogEntry> = read_cache().map(|c| c.entries).unwrap_or_default();
    let custom: Vec<CatalogEntry> = fs::read_to_string(catalog_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    for c in builtin_catalog().into_iter().chain(custom) {
        if !all.iter().any(|b| b.service == c.service && b.version == c.version) {
            all.push(c);
        }
    }
    all.sort_by(|a, b| {
        let order = |s: &str| DEFS.iter().position(|d| d.id == s).unwrap_or(99);
        order(&a.service)
            .cmp(&order(&b.service))
            .then_with(|| version_key(&b.version).cmp(&version_key(&a.version)))
    });
    all
}

// ---------------------------------------------------------------- automatic version discovery

#[derive(Serialize, Deserialize, Default)]
struct CatalogCache {
    fetched_at: u64,
    entries: Vec<CatalogEntry>,
}

fn cache_path() -> PathBuf {
    root().join("catalog-cache.json")
}

fn read_cache() -> Option<CatalogCache> {
    serde_json::from_str(&fs::read_to_string(cache_path()).ok()?).ok()
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn catalog_last_checked() -> Option<u64> {
    read_cache().map(|c| c.fetched_at)
}

/// True when the version list was never fetched, or is older than 24 hours.
pub fn catalog_cache_stale() -> bool {
    match catalog_last_checked() {
        Some(t) => now_secs().saturating_sub(t) > 24 * 3600,
        None => true,
    }
}

pub(crate) fn http_text(url: &str) -> Result<String, String> {
    ureq::get(url)
        .set("User-Agent", "JOJO-Server/1.0")
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .into_string()
        .map_err(e2s)
}

pub(crate) fn head_ok(url: &str) -> bool {
    ureq::head(url)
        .set("User-Agent", "JOJO-Server/1.0")
        .timeout(Duration::from_secs(8))
        .call()
        .is_ok()
}

/// Check many URLs in parallel; returns the (key, url) pairs that exist.
fn probe_many(items: Vec<(String, String)>) -> Vec<(String, String)> {
    if items.is_empty() {
        return vec![];
    }
    let per = (items.len() + 15) / 16;
    std::thread::scope(|s| {
        let handles: Vec<_> = items
            .chunks(per)
            .map(|chunk| s.spawn(move || chunk.iter().filter(|(_, u)| head_ok(u)).cloned().collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

fn entry(service: &str, version: &str, url: &str, note: &str, sha256: Option<String>) -> CatalogEntry {
    CatalogEntry { service: service.into(), version: version.into(), url: url.into(), note: note.into(), sha256 }
}

/// Official PHP release list (windows.php.net), including SHA-256 checksums.
fn discover_php() -> Result<Vec<CatalogEntry>, String> {
    let text = http_text("https://windows.php.net/downloads/releases/releases.json")?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(e2s)?;
    let mut out = vec![];
    for (branch, info) in v.as_object().ok_or("Unexpected PHP release list.")? {
        if !(branch.starts_with("8.") || branch.starts_with("9.")) {
            continue;
        }
        let Some(ver) = info["version"].as_str() else { continue };
        let Some(key) = info
            .as_object()
            .and_then(|o| o.keys().find(|k| k.starts_with("nts-") && k.ends_with("-x64")))
        else {
            continue;
        };
        let zip = &info[key.as_str()]["zip"];
        let Some(path) = zip["path"].as_str() else { continue };
        out.push(entry(
            "php",
            ver,
            &format!("https://windows.php.net/downloads/releases/{path}"),
            "Latest in the PHP branch (non-thread-safe)",
            zip["sha256"].as_str().map(|s| s.to_string()),
        ));
    }
    Ok(out)
}

/// Apache Lounge, the usual source of Apache for Windows (the Apache project itself ships no Windows build).
fn discover_apache() -> Result<Vec<CatalogEntry>, String> {
    let page = http_text("https://www.apachelounge.com/download/")?;
    let mut best: Option<(Vec<u32>, String, String)> = None; // (version key, version, path)
    for (i, _) in page.match_indices("/download/VS") {
        let rest = &page[i..];
        let end = rest.find(|c: char| c == '"' || c == '\'' || c.is_whitespace() || c == '<').unwrap_or(rest.len());
        let path = &rest[..end];
        let file = path.rsplit('/').next().unwrap_or("");
        let lower = file.to_lowercase();
        if !(lower.starts_with("httpd-") && lower.contains("-win64-vs") && lower.ends_with(".zip")) {
            continue;
        }
        let ver = file.split('-').nth(1).unwrap_or("").to_string();
        if ver.is_empty() {
            continue;
        }
        let key = version_key(&ver);
        if best.as_ref().map(|b| key > b.0).unwrap_or(true) {
            best = Some((key, ver, path.to_string()));
        }
    }
    let Some((_, ver, path)) = best else { return Ok(vec![]) };
    let url = format!("https://www.apachelounge.com{path}");
    // The site publishes a checksum file next to every download.
    let sha = http_text(&format!("{url}.txt")).ok().and_then(|t| {
        let mut lines = t.lines();
        while let Some(l) = lines.next() {
            if l.starts_with("SHA256-Checksum for") {
                return lines.next().map(|h| h.trim().to_lowercase()).filter(|h| h.len() == 64);
            }
        }
        None
    });
    Ok(vec![entry("apache", &ver, &url, "Apache Lounge build (needs the Visual C++ 2015-2026 runtime)", sha)])
}

/// nginx.org download page.
fn discover_nginx() -> Result<Vec<CatalogEntry>, String> {
    let text = http_text("https://nginx.org/en/download.html")?;
    let mut vers: Vec<String> = vec![];
    for (i, _) in text.match_indices("/download/nginx-") {
        let rest = &text[i + "/download/nginx-".len()..];
        let raw: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let ver = raw.trim_end_matches('.').to_string();
        if !ver.is_empty() && rest[ver.len()..].starts_with(".zip") && !vers.contains(&ver) {
            vers.push(ver);
        }
    }
    vers.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
    Ok(vers
        .into_iter()
        .take(4)
        .map(|v| {
            let url = format!("https://nginx.org/download/nginx-{v}.zip");
            entry("nginx", &v, &url, "Official build", None)
        })
        .collect())
}

/// Latest GitHub release asset whose name satisfies `pick`; returns (name, tag, download URL).
pub(crate) fn github_latest(repo: &str, pick: impl Fn(&str) -> bool) -> Result<(String, String, String), String> {
    let text = http_text(&format!("https://api.github.com/repos/{repo}/releases/latest"))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(e2s)?;
    let tag = v["tag_name"].as_str().unwrap_or("").to_string();
    for a in v["assets"].as_array().ok_or("No release assets.")? {
        if let (Some(name), Some(url)) = (a["name"].as_str(), a["browser_download_url"].as_str()) {
            if pick(name) {
                return Ok((name.to_string(), tag, url.to_string()));
            }
        }
    }
    Err(format!("No Windows download found in the latest {repo} release."))
}

/// nodejs.org release list: the newest release of the three newest LTS lines, plus the newest "Current".
fn discover_node() -> Result<Vec<CatalogEntry>, String> {
    let text = http_text("https://nodejs.org/dist/index.json")?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(e2s)?;
    let list = v.as_array().ok_or("Unexpected Node.js release list.")?;
    let mut picked: Vec<(String, bool)> = vec![];
    let mut lts_majors: Vec<u32> = vec![];
    for r in list {
        let ver = r["version"].as_str().unwrap_or("").trim_start_matches('v').to_string();
        let has_win = r["files"].as_array().map(|f| f.iter().any(|x| x.as_str() == Some("win-x64-zip"))).unwrap_or(false);
        if ver.is_empty() || !has_win {
            continue;
        }
        let major: u32 = ver.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
        if r["lts"].is_string() {
            if !lts_majors.contains(&major) && lts_majors.len() < 3 {
                lts_majors.push(major);
                picked.push((ver, true));
            }
        } else if picked.is_empty() {
            picked.push((ver, false));
        }
    }
    let mut out = vec![];
    for (ver, lts) in picked {
        let sums = http_text(&format!("https://nodejs.org/dist/v{ver}/SHASUMS256.txt")).unwrap_or_default();
        let file = format!("node-v{ver}-win-x64.zip");
        let sha = sums
            .lines()
            .find(|l| l.trim_end().ends_with(&file))
            .and_then(|l| l.split_whitespace().next())
            .map(|s| s.to_string());
        out.push(entry(
            "node",
            &ver,
            &format!("https://nodejs.org/dist/v{ver}/{file}"),
            if lts { "LTS (long-term support)" } else { "Current (newest features, short support)" },
            sha,
        ));
    }
    Ok(out)
}

fn discover_mailpit() -> Result<Vec<CatalogEntry>, String> {
    let (_, tag, url) = github_latest("axllent/mailpit", |n| n == "mailpit-windows-amd64.zip")?;
    Ok(vec![entry("mailpit", tag.trim_start_matches('v'), &url, "Local mail catcher", None)])
}

fn discover_redis() -> Result<Vec<CatalogEntry>, String> {
    let (name, _, url) = github_latest("tporadowski/redis", |n| n.starts_with("Redis-x64-") && n.ends_with(".zip"))?;
    let ver = name.trim_start_matches("Redis-x64-").trim_end_matches(".zip").to_string();
    Ok(vec![entry("redis", &ver, &url, "Windows port by tporadowski", None)])
}

/// MySQL has no machine-readable list, so check which patch releases exist on the official CDN.
fn discover_mysql() -> Result<Vec<CatalogEntry>, String> {
    let mut items = vec![];
    for series in ["8.4"] {
        for n in 0..=30 {
            let v = format!("{series}.{n}");
            for base in [format!("https://cdn.mysql.com/Downloads/MySQL-{series}"), format!("https://cdn.mysql.com/archives/mysql-{series}")] {
                items.push((v.clone(), format!("{base}/mysql-{v}-winx64.zip")));
            }
        }
    }
    let hits = probe_many(items);
    let best = hits.into_iter().max_by(|a, b| version_key(&a.0).cmp(&version_key(&b.0)));
    Ok(best
        .map(|(v, url)| vec![entry("mysql", &v, &url, "Official ZIP (large, about 250 MB)", None)])
        .unwrap_or_default())
}

/// PostgreSQL binaries ZIPs from EDB follow a fixed naming pattern; find the newest minor of each major.
fn discover_postgres() -> Result<Vec<CatalogEntry>, String> {
    let mut items = vec![];
    for major in [18u32, 17, 16, 15, 14] {
        for minor in 0..=14u32 {
            for build in 1..=2u32 {
                items.push((
                    format!("{major}.{minor}"),
                    format!("https://get.enterprisedb.com/postgresql/postgresql-{major}.{minor}-{build}-windows-x64-binaries.zip"),
                ));
            }
        }
    }
    let hits = probe_many(items);
    let mut by_major: HashMap<String, (String, String)> = HashMap::new();
    for (ver, url) in hits {
        let major = ver.split('.').next().unwrap_or("").to_string();
        let better = match by_major.get(&major) {
            Some((v, u)) => (version_key(&ver), &url) > (version_key(v), u),
            None => true,
        };
        if better {
            by_major.insert(major, (ver, url));
        }
    }
    Ok(by_major
        .into_values()
        .map(|(v, url)| entry("postgres", &v, &url, "EDB binaries ZIP", None))
        .collect())
}

/// Look up the newest versions from the official download sites and remember them.
/// Returns how many versions were found. A source that fails is skipped.
pub fn refresh_catalog(emit: &Emit) -> Result<usize, String> {
    log(emit, "catalog", "info", "Checking for new versions...");
    let sources: Vec<(&str, fn() -> Result<Vec<CatalogEntry>, String>)> = vec![
        ("PHP", discover_php),
        ("Nginx", discover_nginx),
        ("Apache", discover_apache),
        ("Mailpit", discover_mailpit),
        ("Node.js", discover_node),
        ("Redis", discover_redis),
        ("MySQL", discover_mysql),
        ("PostgreSQL", discover_postgres),
    ];
    let before = catalog().len();
    let results: Vec<(&str, Result<Vec<CatalogEntry>, String>)> = std::thread::scope(|s| {
        let hs: Vec<_> = sources.iter().map(|(n, f)| (*n, s.spawn(move || f()))).collect();
        hs.into_iter()
            .map(|(n, h)| (n, h.join().unwrap_or_else(|_| Err("crashed".into()))))
            .collect()
    });
    let mut entries = vec![];
    let mut failed = 0;
    for (name, r) in results {
        match r {
            Ok(list) => entries.extend(list),
            Err(e) => {
                failed += 1;
                log(emit, "catalog", "warn", format!("{name}: could not check ({e})"));
            }
        }
    }
    if entries.is_empty() {
        return Err("Could not reach any download site. Check your internet connection.".into());
    }
    // Keep earlier discoveries too, so versions don't vanish when one source is temporarily down.
    if let Some(old) = read_cache() {
        for o in old.entries {
            if !entries.iter().any(|e| e.service == o.service && e.version == o.version) {
                entries.push(o);
            }
        }
    }
    let cache = CatalogCache { fetched_at: now_secs(), entries };
    fs::write(cache_path(), serde_json::to_string_pretty(&cache).map_err(e2s)?).map_err(e2s)?;
    let added = catalog().len().saturating_sub(before);
    let msg = if failed > 0 {
        format!("Version list updated ({added} new). {failed} source(s) could not be checked.")
    } else {
        format!("Version list updated ({added} new).")
    };
    log(emit, "catalog", "ok", msg);
    emit("catalog", json!({ "added": added }));
    Ok(added)
}

pub(crate) fn sha256_file(p: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut f = fs::File::open(p).map_err(e2s)?;
    let mut h = Sha256::new();
    io::copy(&mut f, &mut h).map_err(e2s)?;
    Ok(format!("{:x}", h.finalize()))
}

pub(crate) fn emit_progress(emit: &Emit, service: &str, version: &str, received: u64, total: u64, stage: &str) {
    emit(
        "progress",
        json!({ "service": service, "version": version, "received": received, "total": total, "stage": stage }),
    );
}

pub(crate) fn valid_version(v: &str) -> bool {
    !v.is_empty() && v.len() < 40 && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

pub(crate) fn download_file(emit: &Emit, svc: &str, ver: &str, url: &str, dest: &Path) -> Result<u64, String> {
    emit_progress(emit, svc, ver, 0, 0, "download");
    let resp = ureq::get(url)
        .set("User-Agent", "JOJO-Server/1.0")
        .call()
        .map_err(|e| format!("Download failed: {e}"))?;
    let total: u64 = resp.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut reader = resp.into_reader();
    let mut file = fs::File::create(dest).map_err(e2s)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut received: u64 = 0;
    let mut last = Instant::now();
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("Download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(e2s)?;
        received += n as u64;
        if last.elapsed() > Duration::from_millis(250) {
            emit_progress(emit, svc, ver, received, total, "download");
            last = Instant::now();
        }
    }
    emit_progress(emit, svc, ver, received, received.max(total), "download");
    Ok(received)
}

pub(crate) fn extract_zip(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let f = fs::File::open(zip_path).map_err(e2s)?;
    let mut ar = zip::ZipArchive::new(f).map_err(e2s)?;

    // If every entry lives under one top-level folder, strip it.
    let mut top: Option<String> = None;
    let mut strip = true;
    for i in 0..ar.len() {
        let name = ar.by_index(i).map_err(e2s)?.name().replace('\\', "/");
        let first = name.split('/').next().unwrap_or("").to_string();
        if !name.contains('/') {
            strip = false;
            break;
        }
        match &top {
            None => top = Some(first),
            Some(t) if *t == first => {}
            _ => {
                strip = false;
                break;
            }
        }
    }

    for i in 0..ar.len() {
        let mut file = ar.by_index(i).map_err(e2s)?;
        let Some(rel) = file.enclosed_name() else { continue };
        let rel: PathBuf = if strip { rel.components().skip(1).collect() } else { rel };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dest.join(&rel);
        if file.is_dir() {
            fs::create_dir_all(&out).map_err(e2s)?;
        } else {
            if let Some(p) = out.parent() {
                fs::create_dir_all(p).map_err(e2s)?;
            }
            let mut o = fs::File::create(&out).map_err(e2s)?;
            io::copy(&mut file, &mut o).map_err(e2s)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- shell helpers

pub fn open_path(kind: &str) -> Result<(), String> {
    let p = match kind {
        "www" => www_dir(),
        "bin" => root().join("bin"),
        "logs" => root().join("run").join("nginx").join("logs"),
        _ => root(),
    };
    let _ = fs::create_dir_all(&p);
    Command::new("explorer").arg(p).spawn().map(|_| ()).map_err(e2s)
}

pub fn open_url(url: &str) -> Result<(), String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("Invalid URL.".into());
    }
    let mut c = Command::new("cmd");
    c.args(["/c", "start", "", url]);
    hide(&mut c);
    c.spawn().map(|_| ()).map_err(e2s)
}

/// Opens a terminal with the active PHP / MySQL / PostgreSQL / Redis tools on PATH.
/// With a site name, the terminal opens in that project's folder and uses the site's PHP version.
pub fn open_terminal(site: Option<&str>) -> Result<(), String> {
    let (path, title, cwd, phprc) = terminal_env(site)?;
    let mut c = Command::new("cmd");
    c.arg("/c")
        .arg("start")
        .arg(&title)
        .arg("cmd")
        .arg("/k")
        .current_dir(cwd)
        .env("PATH", path);
    if let Some(p) = phprc {
        c.env("PHPRC", p); // makes the command-line `php` use the same php.ini (and extensions) as the web server
    }
    hide(&mut c);
    c.spawn().map(|_| ()).map_err(e2s)
}

/// Returns (PATH value, window title, working folder, folder holding the PHP configuration).
pub fn terminal_env(site: Option<&str>) -> Result<(String, String, PathBuf, Option<PathBuf>), String> {
    let s = load_settings();
    let site_info = match site {
        Some(n) => Some(sites().into_iter().find(|x| x.name == n).ok_or("Site not found.")?),
        None => None,
    };
    let installed_php = versions("php");
    let mut parts: Vec<String> = vec![];
    let mut phprc: Option<PathBuf> = None;
    for d in DEFS.iter() {
        let chosen = if d.id == "php" {
            site_info
                .as_ref()
                .and_then(|x| x.php.clone())
                .filter(|v| installed_php.contains(v))
                .or_else(|| active_of(&s, d.id))
        } else {
            active_of(&s, d.id)
        };
        if let Some(v) = chosen {
            if let Some(base) = base_dir(&root().join("bin").join(d.id).join(&v), d) {
                if d.id == "php" {
                    phprc = write_php_ini(&base, &v).ok().and_then(|p| p.parent().map(|x| x.to_path_buf()));
                }
                parts.push(base.to_string_lossy().to_string());
                let b = base.join("bin");
                if b.is_dir() {
                    parts.push(b.to_string_lossy().to_string());
                }
            }
        }
    }
    // Node.js (node, npm, npx) from the version chosen in Settings.
    if let Some(v) = active_of(&s, "node") {
        if let Some(base) = base_dir(&root().join("bin").join("node").join(&v), &NODE_DEF) {
            parts.push(base.to_string_lossy().to_string());
        }
    }
    if composer_installed() {
        parts.push(composer_dir().to_string_lossy().to_string());
    }
    let old = std::env::var("PATH").unwrap_or_default();
    parts.push(old);
    let (title, cwd) = match &site_info {
        Some(x) => (format!("JOJO - {}", x.domain), PathBuf::from(&x.path)),
        None => ("JOJO Terminal".to_string(), www_dir()),
    };
    Ok((parts.join(";"), title, cwd, phprc))
}
