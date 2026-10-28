//! Wildcard domains: a tiny DNS server that answers every name under the chosen suffix
//! (shop.test, api.shop.test, anything.test) with 127.0.0.1, so the hosts file never needs editing.
//!
//! Windows only asks this server about the suffix because of one "name resolution policy" rule
//! that is added once with administrator rights (see `enable_wildcard_dns`).

use crate::manager::{self, e2s, log, Core};
use std::net::UdpSocket;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct DnsHandle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DnsHandle {
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Build the answer to one DNS query, or None if it is not something we should answer.
pub fn answer(q: &[u8], tld: &str) -> Option<Vec<u8>> {
    if q.len() < 17 || q[2] & 0x80 != 0 {
        return None; // too short, or already a response
    }
    if u16::from_be_bytes([q[4], q[5]]) < 1 {
        return None;
    }
    // question name
    let mut pos = 12;
    let mut labels: Vec<String> = vec![];
    loop {
        let len = *q.get(pos)? as usize;
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xC0 != 0 {
            return None; // compression is not used in questions
        }
        let part = q.get(pos + 1..pos + 1 + len)?;
        labels.push(String::from_utf8_lossy(part).to_lowercase());
        pos += 1 + len;
    }
    let qtype = u16::from_be_bytes([*q.get(pos)?, *q.get(pos + 1)?]);
    let question_end = pos + 4;
    if q.len() < question_end {
        return None;
    }
    let name = labels.join(".");
    let tld = tld.trim_start_matches('.').to_lowercase();
    let ours = name == tld || name.ends_with(&format!(".{tld}"));

    let give_a = ours && (qtype == 1 || qtype == 255);
    let mut r = Vec::with_capacity(question_end + 16);
    r.extend_from_slice(&q[0..2]); // id
    // response, recursion desired/available; NXDOMAIN for names that are not ours
    r.extend_from_slice(&(if ours { 0x8180u16 } else { 0x8183u16 }).to_be_bytes());
    r.extend_from_slice(&1u16.to_be_bytes()); // questions
    r.extend_from_slice(&(give_a as u16).to_be_bytes()); // answers
    r.extend_from_slice(&0u16.to_be_bytes());
    r.extend_from_slice(&0u16.to_be_bytes());
    r.extend_from_slice(&q[12..question_end]);
    if give_a {
        r.extend_from_slice(&[0xC0, 0x0C]); // name: pointer to the question
        r.extend_from_slice(&1u16.to_be_bytes()); // type A
        r.extend_from_slice(&1u16.to_be_bytes()); // class IN
        r.extend_from_slice(&60u32.to_be_bytes()); // ttl
        r.extend_from_slice(&4u16.to_be_bytes());
        r.extend_from_slice(&[127, 0, 0, 1]);
    }
    Some(r)
}

/// Start answering on `bind` (normally 127.0.0.1:53).
pub fn start(bind: &str, tld: &str) -> Result<DnsHandle, String> {
    let sock = UdpSocket::bind(bind).map_err(|e| {
        format!("Could not open the DNS port ({bind}): {e}. Another program (often Docker, WSL or Internet Connection Sharing) may be using port 53.")
    })?;
    sock.set_read_timeout(Some(Duration::from_millis(300))).map_err(e2s)?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let tld = tld.to_string();
    let thread = std::thread::spawn(move || {
        let mut buf = [0u8; 512];
        while !flag.load(Ordering::SeqCst) {
            match sock.recv_from(&mut buf) {
                Ok((n, from)) => {
                    let debug = std::env::var("JOJO_DNS_DEBUG").is_ok();
                    if debug {
                        eprintln!("dns in  ({n}): {:02x?}", &buf[..n]);
                    }
                    if let Some(resp) = answer(&buf[..n], &tld) {
                        if debug {
                            eprintln!("dns out ({}): {:02x?}", resp.len(), resp);
                        }
                        let _ = sock.send_to(&resp, from);
                    }
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    });
    Ok(DnsHandle { stop, thread: Some(thread) })
}

/// Run PowerShell commands with administrator rights (one Windows prompt) and report success.
pub fn run_elevated(script: &str) -> Result<(), String> {
    let run = manager::root().join("run");
    std::fs::create_dir_all(&run).map_err(e2s)?;
    let ps1 = run.join("elevated.ps1");
    let result = run.join("elevated.result");
    let _ = std::fs::remove_file(&result);
    let body = format!(
        "$ErrorActionPreference = 'Stop'\r\ntry {{\r\n{script}\r\nSet-Content -LiteralPath '{res}' -Value 'ok'\r\n}} catch {{\r\nSet-Content -LiteralPath '{res}' -Value ('error: ' + $_.Exception.Message)\r\n}}\r\n",
        res = result.display()
    );
    std::fs::write(&ps1, body).map_err(e2s)?;
    let launch = format!(
        "Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','\"{}\"'",
        ps1.display()
    );
    let mut c = Command::new("powershell");
    c.arg("-NoProfile").arg("-Command").arg(&launch);
    manager::hide(&mut c);
    let st = c.status().map_err(e2s)?;
    let res = std::fs::read_to_string(&result).unwrap_or_default();
    let res = res.trim();
    if res == "ok" {
        Ok(())
    } else if res.starts_with("error:") {
        Err(res.trim_start_matches("error:").trim().to_string())
    } else if !st.success() {
        Err("Administrator permission was not granted. Click 'Yes' on the Windows prompt.".into())
    } else {
        Err("The administrator step did not finish.".into())
    }
}

/// Does Windows itself resolve a made-up name under the suffix to this PC?
pub fn resolves_locally(tld: &str) -> bool {
    let script = format!(
        "try {{ ([System.Net.Dns]::GetHostAddresses('jojo-check.{}') | Where-Object {{ $_.ToString() -eq '127.0.0.1' }}).Count }} catch {{ 0 }}",
        tld.trim_start_matches('.')
    );
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-Command"]).arg(script);
    manager::hide(&mut c);
    c.output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().ok())
        .map(|n| n > 0)
        .unwrap_or(false)
}

impl Core {
    pub fn dns_running(&self) -> bool {
        self.dns.lock().unwrap().is_some()
    }

    /// Start the DNS server if wildcard domains are switched on (at app start, and after a suffix change).
    pub fn dns_start_if_enabled(&self) -> Result<(), String> {
        let s = manager::load_settings();
        if !s.dns.enabled {
            return Ok(());
        }
        self.dns_restart(&manager::tld_of(&s), "127.0.0.1:53")
    }

    pub fn dns_restart(&self, tld: &str, bind: &str) -> Result<(), String> {
        self.dns_stop();
        let h = start(bind, tld)?;
        *self.dns.lock().unwrap() = Some(h);
        Ok(())
    }

    pub fn dns_stop(&self) {
        if let Some(h) = self.dns.lock().unwrap().take() {
            h.stop();
        }
    }

    /// Turn wildcard domains on: start the server and (one administrator prompt) tell Windows to ask it
    /// about the suffix.
    pub fn enable_wildcard_dns(&self) -> Result<String, String> {
        let s = manager::load_settings();
        let tld = manager::tld_of(&s);
        self.dns_restart(&tld, "127.0.0.1:53")?;
        let script = format!(
            "Get-DnsClientNrptRule | Where-Object {{ $_.Comment -eq 'JOJO Server' }} | Remove-DnsClientNrptRule -Force\r\n\
             Add-DnsClientNrptRule -Namespace '.{tld}' -NameServers '127.0.0.1' -Comment 'JOJO Server'\r\n\
             Clear-DnsClientCache"
        );
        if let Err(e) = run_elevated(&script) {
            self.dns_stop();
            return Err(e);
        }
        let mut s = manager::load_settings();
        s.dns.enabled = true;
        s.dns.applied_tld = Some(tld.clone());
        manager::save_settings(&s)?;
        // Give the DNS client a moment, then confirm that Windows really asks our server.
        let mut ok = false;
        for _ in 0..5 {
            if resolves_locally(&tld) {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(600));
        }
        log(&self.emit, "dns", if ok { "ok" } else { "warn" }, if ok {
            format!("Wildcard domains are on: anything.{tld} now points to this PC.")
        } else {
            format!("Wildcard domains were set up, but Windows did not answer a test name under .{tld} yet.")
        });
        Ok(if ok {
            format!("Wildcard domains are on. Any name ending in .{tld} now opens your local sites.")
        } else {
            "The setting was applied, but a test lookup did not work yet. Wait a few seconds and try the site, or restart your browser.".into()
        })
    }

    pub fn disable_wildcard_dns(&self) -> Result<(), String> {
        self.dns_stop();
        let script = "Get-DnsClientNrptRule | Where-Object { $_.Comment -eq 'JOJO Server' } | Remove-DnsClientNrptRule -Force\r\nClear-DnsClientCache";
        let res = run_elevated(script);
        let mut s = manager::load_settings();
        if res.is_ok() {
            s.dns.enabled = false;
            s.dns.applied_tld = None;
            manager::save_settings(&s)?;
        }
        res
    }
}
