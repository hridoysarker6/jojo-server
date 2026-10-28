//! Extra tools: Node.js (LTS install), DbGate (database manager) and public share links.

use crate::manager::{self, catalog, download_file, e2s, extract_zip, hide, log, root, version_key, versions, Core, Running};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn tools_dir() -> PathBuf {
    root().join("bin").join("tools")
}

// ---------------------------------------------------------------- DbGate

fn dbgate_dir() -> PathBuf {
    tools_dir().join("dbgate")
}

/// DbGate.exe (found by looking at the top two folder levels, since the archive layout can vary).
pub fn dbgate_exe() -> Option<PathBuf> {
    let base = dbgate_dir();
    let direct = base.join("DbGate.exe");
    if direct.is_file() {
        return Some(direct);
    }
    for e in fs::read_dir(&base).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            let c = p.join("DbGate.exe");
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

pub fn launch_dbgate() -> Result<(), String> {
    let exe = dbgate_exe().ok_or("DbGate is not installed yet.")?;
    Command::new(&exe)
        .current_dir(exe.parent().unwrap_or(&dbgate_dir()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not open DbGate: {e}"))
}

// ---------------------------------------------------------------- public share (Cloudflare quick tunnel)

fn cloudflared_exe() -> PathBuf {
    tools_dir().join("cloudflared").join("cloudflared.exe")
}

/// First `https://something.trycloudflare.com` in a line of cloudflared output.
pub fn find_tunnel_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest.find(|c: char| c.is_whitespace() || c == '|' || c == '"').unwrap_or(rest.len());
    let url = &rest[..end];
    if url.ends_with(".trycloudflare.com") && url.len() > "https://.trycloudflare.com".len() {
        Some(url.to_string())
    } else {
        None
    }
}

impl Core {
    // ---- Node.js

    /// Install the newest LTS Node.js from the catalog (does nothing if some version is installed).
    pub fn install_node_lts(&self) -> Result<(), String> {
        if !versions("node").is_empty() {
            return Ok(());
        }
        // Make sure the list knows today's newest LTS (best effort: the built-in entry is the fallback).
        if manager::catalog_cache_stale() {
            let _ = manager::refresh_catalog(&self.emit);
        }
        let best = catalog()
            .into_iter()
            .filter(|c| c.service == "node" && c.note.starts_with("LTS"))
            .max_by(|a, b| version_key(&a.version).cmp(&version_key(&b.version)))
            .ok_or("No Node.js LTS version is in the download list.")?;
        self.install("node", &best.version, &best.url)
    }

    // ---- DbGate

    pub fn install_dbgate(&self) -> Result<(), String> {
        if dbgate_exe().is_some() {
            return Ok(());
        }
        log(&self.emit, "dbgate", "info", "Looking for the newest DbGate...");
        let (name, tag, url) = manager::github_latest("dbgate/dbgate", |n| {
            n.starts_with("dbgate-") && n.ends_with("-win_x64.zip") && !n.contains("premium")
        })?;
        let ver = tag.trim_start_matches('v').to_string();
        fs::create_dir_all(root().join("downloads")).map_err(e2s)?;
        let zip = root().join("downloads").join(&name);
        log(&self.emit, "dbgate", "info", format!("Downloading DbGate {ver} (about 180 MB)..."));
        let got = download_file(&self.emit, "dbgate", &ver, &url, &zip)?;
        manager::emit_progress(&self.emit, "dbgate", &ver, got, got, "extract");
        let tmp = tools_dir().join(".dbgate-tmp");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(e2s)?;
        if let Err(e) = extract_zip(&zip, &tmp) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(format!("Extraction failed: {e}"));
        }
        let _ = fs::remove_dir_all(dbgate_dir());
        fs::rename(&tmp, dbgate_dir()).map_err(e2s)?;
        let _ = fs::remove_file(&zip);
        if dbgate_exe().is_none() {
            return Err("DbGate was downloaded but DbGate.exe was not found inside it.".into());
        }
        manager::emit_progress(&self.emit, "dbgate", &ver, got, got, "done");
        log(&self.emit, "dbgate", "ok", format!("DbGate {ver} installed"));
        Ok(())
    }

    // ---- public share links

    /// Put a site on the internet through a free Cloudflare quick tunnel. Returns the public URL.
    pub fn share_start(&self, site: &str) -> Result<String, String> {
        if let Some(u) = self.shares.lock().unwrap().get(site) {
            return Ok(u.clone());
        }
        let s = manager::load_settings();
        let info = manager::sites().into_iter().find(|x| x.name == site).ok_or("Site not found.")?;
        if !self.web_running() {
            return Err("Start the web server first (Nginx or Apache, on the Dashboard), so there is a website to share.".into());
        }
        let exe = cloudflared_exe();
        if !exe.is_file() {
            log(&self.emit, "share", "info", "Downloading the Cloudflare tunnel program (once, about 60 MB)...");
            fs::create_dir_all(exe.parent().unwrap()).map_err(e2s)?;
            let part = exe.with_extension("part");
            download_file(
                &self.emit,
                "cloudflared",
                "latest",
                "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-amd64.exe",
                &part,
            )?;
            fs::rename(&part, &exe).map_err(e2s)?;
        }
        let port = manager::port_of(&s, "nginx");
        let mut c = Command::new(&exe);
        c.arg("tunnel")
            .arg("--no-autoupdate")
            .arg("--url")
            .arg(format!("http://127.0.0.1:{port}"))
            .arg("--http-host-header")
            .arg(&info.domain)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide(&mut c);
        let mut child = c.spawn().map_err(|e| format!("Could not start the tunnel: {e}"))?;
        let found: std::sync::Arc<std::sync::Mutex<Option<String>>> = Default::default();
        for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)]
            .into_iter()
            .flatten()
        {
            let f = found.clone();
            std::thread::spawn(move || {
                use std::io::{BufRead, BufReader};
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    if let Some(u) = find_tunnel_url(&line) {
                        *f.lock().unwrap() = Some(u);
                    }
                }
            });
        }
        let key = format!("share:{site}");
        let mut url = None;
        for _ in 0..80 {
            std::thread::sleep(Duration::from_millis(300));
            if let Some(u) = found.lock().unwrap().clone() {
                url = Some(u);
                break;
            }
            if matches!(child.try_wait(), Ok(Some(_))) {
                return Err("The tunnel program stopped right after starting. Check your internet connection.".into());
            }
        }
        let Some(url) = url else {
            crate::workers::kill_tree(child.id());
            let _ = child.kill();
            return Err("Timed out waiting for the public address. Check your internet connection and try again.".into());
        };
        self.running
            .lock()
            .unwrap()
            .insert(key, Running { child, started: Instant::now(), version: String::new() });
        self.shares.lock().unwrap().insert(site.to_string(), url.clone());
        log(&self.emit, "share", "ok", format!("{site} is public at {url}"));
        Ok(url)
    }

    pub fn share_stop(&self, site: &str) {
        let taken = self.running.lock().unwrap().remove(&format!("share:{site}"));
        if let Some(mut r) = taken {
            crate::workers::kill_tree(r.child.id());
            let _ = r.child.kill();
            let _ = r.child.wait();
        }
        if self.shares.lock().unwrap().remove(site).is_some() {
            log(&self.emit, "share", "warn", format!("{site} is no longer shared"));
        }
    }

    pub fn stop_shares(&self) {
        let sites: Vec<String> = self.shares.lock().unwrap().keys().cloned().collect();
        for s in sites {
            self.share_stop(&s);
        }
    }

    /// Forget shares whose tunnel process has died (so the Sites tab does not show a dead link).
    pub fn prune_shares(&self) {
        let sites: Vec<String> = self.shares.lock().unwrap().keys().cloned().collect();
        for s in sites {
            if !self.alive(&format!("share:{s}")) {
                self.shares.lock().unwrap().remove(&s);
            }
        }
    }
}
