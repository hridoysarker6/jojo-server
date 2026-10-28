//! Project workers: background commands that belong to a site (Laravel queue worker, scheduler,
//! Vite dev server...). They run with the site's PHP and Node.js, in the project folder.

use crate::manager::{self, e2s, hide, load_settings, log, pipe_logs, save_settings, Core, Running, WorkerCfg};
use serde::Serialize;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Serialize)]
pub struct WorkerInfo {
    pub site: String,
    pub name: String,
    pub command: String,
    pub autostart: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub uptime: u64,
}

fn key(site: &str, name: &str) -> String {
    format!("worker:{site}:{name}")
}

fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 30 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Stop a process and the programs it started.
///
/// This deliberately does not use `taskkill /T`: Windows reuses process ids, and `/T` trusts the
/// recorded parent id, so it can also kill unrelated programs whose (long dead) parent happened to
/// have the same id. Here a program only counts as a child if it was created after its parent.
pub(crate) fn kill_tree(pid: u32) {
    let script = format!(
        "function Stop-Tree($id) {{ \
           $p = Get-CimInstance Win32_Process -Filter \"ProcessId=$id\" -ErrorAction SilentlyContinue; \
           if (-not $p) {{ return }}; \
           Get-CimInstance Win32_Process -Filter \"ParentProcessId=$id\" -ErrorAction SilentlyContinue | \
             Where-Object {{ $_.CreationDate -ge $p.CreationDate }} | ForEach-Object {{ Stop-Tree $_.ProcessId }}; \
           Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }}; \
         Stop-Tree {pid}"
    );
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-Command"]).arg(script);
    manager::run_quiet(&mut c);
}

impl Core {
    pub fn workers_overview(&self) -> Vec<WorkerInfo> {
        let cfg = load_settings().workers;
        cfg.into_iter()
            .map(|w| {
                let k = key(&w.site, &w.name);
                let running = self.alive(&k);
                let (pid, uptime) = if running {
                    let g = self.running.lock().unwrap();
                    g.get(&k).map(|r| (Some(r.child.id()), r.started.elapsed().as_secs())).unwrap_or((None, 0))
                } else {
                    (None, 0)
                };
                WorkerInfo { site: w.site, name: w.name, command: w.command, autostart: w.autostart, running, pid, uptime }
            })
            .collect()
    }

    pub fn add_worker(&self, site: &str, name: &str, command: &str) -> Result<(), String> {
        if !manager::sites().iter().any(|s| s.name == site) {
            return Err("Site not found.".into());
        }
        if !valid_name(name) {
            return Err("Give the worker a short name (letters, numbers, - and _).".into());
        }
        let command = command.trim();
        if command.is_empty() || command.len() > 500 {
            return Err("Enter the command to run, for example: php artisan queue:work".into());
        }
        let mut s = load_settings();
        if s.workers.iter().any(|w| w.site == site && w.name == name) {
            return Err(format!("This site already has a worker called '{name}'."));
        }
        s.workers.push(WorkerCfg { site: site.into(), name: name.into(), command: command.into(), autostart: false });
        save_settings(&s)
    }

    pub fn remove_worker(&self, site: &str, name: &str) -> Result<(), String> {
        self.stop_worker(site, name)?;
        let mut s = load_settings();
        s.workers.retain(|w| !(w.site == site && w.name == name));
        save_settings(&s)
    }

    pub fn set_worker_autostart(&self, site: &str, name: &str, on: bool) -> Result<(), String> {
        let mut s = load_settings();
        for w in s.workers.iter_mut() {
            if w.site == site && w.name == name {
                w.autostart = on;
            }
        }
        save_settings(&s)
    }

    pub fn start_worker(&self, site: &str, name: &str) -> Result<(), String> {
        let k = key(site, name);
        if self.alive(&k) {
            return Ok(());
        }
        let cfg = load_settings()
            .workers
            .into_iter()
            .find(|w| w.site == site && w.name == name)
            .ok_or("Worker not found.")?;
        let (path, _title, cwd, phprc) = manager::terminal_env(Some(site))?;
        let label = format!("{site}/{name}");

        let mut c = Command::new("cmd");
        // `/S` removes the outer quotes, so the command can contain quotes of its own.
        c.raw_arg(format!("/D /S /C \"{}\"", cfg.command));
        c.current_dir(&cwd).env("PATH", path);
        if let Some(p) = phprc {
            c.env("PHPRC", p);
        }
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        hide(&mut c);
        let mut child = c.spawn().map_err(|e| format!("Could not start the worker: {e}"))?;
        if let Some(o) = child.stdout.take() {
            pipe_logs(self.emit.clone(), label.clone(), o);
        }
        if let Some(o) = child.stderr.take() {
            pipe_logs(self.emit.clone(), label.clone(), o);
        }
        std::thread::sleep(Duration::from_millis(700));
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "The worker '{name}' stopped right away ({status}). Open the Live log on the Dashboard to see why."
            ));
        }
        log(&self.emit, &label, "ok", format!("Worker started: {}", cfg.command));
        self.running
            .lock()
            .unwrap()
            .insert(k, Running { child, started: Instant::now(), version: String::new() });
        Ok(())
    }

    pub fn stop_worker(&self, site: &str, name: &str) -> Result<(), String> {
        let k = key(site, name);
        let taken = self.running.lock().unwrap().remove(&k);
        if let Some(mut r) = taken {
            kill_tree(r.child.id());
            let _ = r.child.kill();
            let _ = r.child.wait();
            log(&self.emit, &format!("{site}/{name}"), "warn", "Worker stopped");
        }
        Ok(())
    }

    pub fn stop_workers(&self) {
        let keys: Vec<String> = self.running.lock().unwrap().keys().filter(|k| k.starts_with("worker:")).cloned().collect();
        for k in keys {
            let mut parts = k.splitn(3, ':');
            let (_, site, name) = (parts.next(), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let _ = self.stop_worker(site, name);
        }
    }

    /// Start every worker marked "start automatically" (after the services are up).
    pub fn start_autostart_workers(&self) {
        for w in load_settings().workers.into_iter().filter(|w| w.autostart) {
            if let Err(e) = self.start_worker(&w.site, &w.name) {
                log(&self.emit, &format!("{}/{}", w.site, w.name), "err", e);
            }
        }
    }
}

#[allow(dead_code)]
fn _unused(e: std::io::Error) -> String {
    e2s(e)
}
