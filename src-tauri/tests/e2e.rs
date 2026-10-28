//! End-to-end test against real binaries (uses the versions already installed under %USERPROFILE%\JojoServer\bin).
//! Run with: cargo test --test e2e -- --nocapture --test-threads=1

use jojo_server_lib::manager::{self as m, Core, Emit};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

struct Guard(Core);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.stop_all();
    }
}

fn setup() -> Guard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("e2e-root");
    let _ = std::fs::remove_dir_all(root.join("www"));
    let _ = std::fs::remove_dir_all(root.join("etc"));
    let _ = std::fs::remove_dir_all(root.join("run"));
    let _ = std::fs::remove_file(root.join("settings.json"));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("JOJO_ROOT", &root);

    // Reuse the binaries that are already installed (junction instead of copying hundreds of MB).
    let real_bin = PathBuf::from(std::env::var("USERPROFILE").unwrap()).join("JojoServer").join("bin");
    let link = root.join("bin");
    if !link.exists() {
        let st = Command::new("cmd")
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(&link)
            .arg(&real_bin)
            .status()
            .unwrap();
        assert!(st.success(), "could not create bin junction");
    }
    m::ensure_dirs();

    let emit: Emit = Arc::new(|name, payload| {
        if name == "log" {
            println!("[{}] {}", payload["svc"].as_str().unwrap_or(""), payload["line"].as_str().unwrap_or(""));
        }
    });
    Guard(Core::new(emit))
}

fn curl(args: &[&str]) -> String {
    // The very first request after a start can be slow on a busy machine: retry empty answers.
    for attempt in 0..3 {
        let out = Command::new("curl.exe").args(["-s", "-m", "10"]).args(args).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if !text.trim().is_empty() || attempt == 2 {
            return text;
        }
        std::thread::sleep(Duration::from_millis(1500));
    }
    String::new()
}

fn port_open(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(), Duration::from_millis(500)).is_ok()
}

fn php_cli(ver: &str, ini_dir: &std::path::Path, args: &[&str]) -> String {
    let exe = m::root().join("bin").join("php").join(ver).join("php.exe");
    let out = Command::new(exe).env("PHPRC", ini_dir).args(args).output().unwrap();
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn reg(args: &[&str]) -> (bool, String) {
    let out = Command::new("reg").args(args).output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string())
}

#[test]
fn projects_folder_can_be_moved_and_is_taken_from_the_installer() {
    let g = setup();
    let core = &g.0;
    m::set_port("nginx", 8081).unwrap();
    m::set_port("php", 9100).unwrap();
    m::create_site("e2e").unwrap();
    core.start("php").unwrap();
    core.start("nginx").unwrap();

    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let moved = base.join("e2e-www-moved");
    let busy = base.join("e2e-www-busy");
    let empty = base.join("e2e-www-empty");
    for d in [&moved, &busy, &empty] {
        let _ = std::fs::remove_dir_all(d);
    }
    let old = m::www_dir();

    // invalid choices are refused
    assert!(m::set_www_dir(core, "relative\\path", true).is_err(), "relative path");
    let inside = m::root().join("bin").join("x");
    assert!(m::set_www_dir(core, &inside.to_string_lossy(), false).is_err(), "inside bin");
    std::fs::create_dir_all(&busy).unwrap();
    std::fs::write(busy.join("keep.txt"), "x").unwrap();
    assert!(m::set_www_dir(core, &busy.to_string_lossy(), true).is_err(), "non-empty destination when moving");
    assert!(busy.join("keep.txt").is_file(), "refused move must not touch the folder");
    assert_eq!(m::www_dir(), old);

    // move the existing project; web server keeps working from the new place
    m::set_www_dir(core, &moved.to_string_lossy(), true).unwrap();
    assert_eq!(m::www_dir(), moved);
    assert!(moved.join("e2e").join("index.php").is_file(), "project moved");
    assert!(!old.join("e2e").exists(), "project left the old folder");
    assert!(core.is_running("nginx") && core.is_running("php"), "services restarted after the move");
    assert!(m::sites().iter().any(|s| s.name == "e2e"));
    let body = curl(&["-H", "Host: e2e.test", "http://127.0.0.1:8081/"]);
    assert!(body.contains("e2e.test is live"), "site must be served from the new folder: {body}");
    m::create_site("fresh").unwrap();
    assert!(moved.join("fresh").is_dir(), "new sites are created in the new folder");

    // switch without moving: the folder is just used as it is
    m::set_www_dir(core, &empty.to_string_lossy(), false).unwrap();
    assert!(m::sites().is_empty());
    assert!(moved.join("e2e").is_dir(), "files are left alone when 'move' is off");

    // A projects folder that is JOJO Server's own data folder (or contains it) is never used:
    // this is what happened when the installer dialog returned the parent folder.
    let root = m::root();
    let mut s = m::load_settings();
    s.www_dir = Some(root.to_string_lossy().to_string());
    m::save_settings(&s).unwrap();
    assert_eq!(m::www_dir(), root.join("www"), "saved folder equal to the data folder falls back to <data>\\www");
    assert!(m::sites().iter().all(|x| !["bin", "data", "etc", "run"].contains(&x.name.as_str())), "internal folders must never show up as sites");
    s.www_dir = root.parent().map(|p| p.to_string_lossy().to_string());
    m::save_settings(&s).unwrap();
    assert_eq!(m::www_dir(), root.join("www"), "a folder that contains the data folder is refused too");
    assert!(m::set_www_dir(core, &root.to_string_lossy(), false).is_err());
    assert!(m::set_www_dir(core, &root.parent().unwrap().to_string_lossy(), false).is_err());
    // never move the data folder's contents by accident
    s.www_dir = Some(root.to_string_lossy().to_string());
    m::save_settings(&s).unwrap();
    m::set_www_dir(core, &moved.to_string_lossy(), true).unwrap();
    assert!(root.join("bin").exists() && root.join("data").exists() && root.join("settings.json").exists(), "data folder untouched");

    // the installer stores the folder in the registry as REG_SZ
    let sample = "\r\nHKEY_CURRENT_USER\\Software\\JOJO Server\r\n    RootDir    REG_SZ    D:\\JojoServer\r\n\r\n";
    assert_eq!(m::parse_reg_value(sample, "RootDir").as_deref(), Some("D:\\JojoServer"));
    assert_eq!(m::parse_reg_value(sample, "WwwDir"), None);
    let _ = reg(&["query", r"HKCU\Software\JOJO Server"]); // the helper must not leave anything behind

    for d in [&moved, &busy, &empty] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn leftover_processes_are_detected_and_cleared() {
    let g = setup();
    let core = &g.0;
    m::set_port("php", 9100).unwrap();
    let ver = m::versions("php")[0].clone();
    let exe = m::root().join("bin").join("php").join(&ver).join("php-cgi.exe");

    // A stray PHP from an earlier session keeps port 9100 and the old settings alive.
    let (_, _, _, phprc) = m::terminal_env(None).unwrap(); // writes php.ini
    let ini = phprc.unwrap().join("php.ini");
    let mut cmd = Command::new(&exe);
    cmd.current_dir(exe.parent().unwrap())
        .arg("-b")
        .arg("127.0.0.1:9100")
        .arg("-c")
        .arg(&ini)
        .env("PHP_FCGI_MAX_REQUESTS", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    m::hide(&mut cmd);
    let mut stray = cmd.spawn().expect("start stray php-cgi");
    std::thread::sleep(Duration::from_millis(1000));
    assert!(port_open(9100), "stray php should be listening");

    let err = core.start("php").unwrap_err();
    println!("start with a busy port says: {err}");
    assert!(err.contains("already in use"), "must explain the port problem: {err}");

    let n = m::reap_orphans(core.emit());
    assert!(n >= 1, "leftover process should be found (found {n})");
    std::thread::sleep(Duration::from_millis(800));
    assert!(stray.try_wait().unwrap().is_some(), "stray php-cgi must be gone");
    core.start("php").expect("php starts once the leftover is gone");
    assert!(core.is_running("php"));
}

fn terminal_cmd(site: Option<&str>, line: &str) -> String {
    let (path, _t, cwd, phprc) = m::terminal_env(site).unwrap();
    let mut c = Command::new("cmd");
    c.args(["/c", line]).current_dir(&cwd).env("PATH", path);
    if let Some(p) = phprc {
        c.env("PHPRC", p);
    }
    let out = c.output().unwrap();
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

#[test]
fn node_is_installed_and_available_in_terminals() {
    let g = setup();
    g.0.install_node_lts().expect("install node lts");
    let vers = m::versions("node");
    println!("node versions: {vers:?}");
    assert!(!vers.is_empty());
    m::create_site("front").unwrap();
    let node = terminal_cmd(Some("front"), "node --version");
    let npm = terminal_cmd(Some("front"), "npm --version");
    println!("node {node} npm {npm}");
    assert!(node.trim().starts_with('v'), "node should run in a terminal: {node}");
    assert!(npm.trim().chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false), "npm should run: {npm}");
    assert!(g.0.start("node").is_err(), "node is a tool, it cannot be started like a service");
    // Node.js appears in the overview as a tool with versions, never as running
    let ov = g.0.overview();
    let n = ov.services.iter().find(|s| s.id == "node").expect("node in overview");
    assert!(!n.running && !n.versions.is_empty());
}

#[test]
fn wildcard_dns_answers_every_name_under_the_suffix() {
    use jojo_server_lib::dns;
    // the packet logic, with a hand-made query
    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for l in name.split('.') {
            q.push(l.len() as u8);
            q.extend_from_slice(l.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }
    let a = dns::answer(&query("shop.test", 1), "test").unwrap();
    assert_eq!(&a[0..2], &[0x12, 0x34], "same id");
    assert_eq!(u16::from_be_bytes([a[6], a[7]]), 1, "one answer");
    assert_eq!(&a[a.len() - 4..], &[127, 0, 0, 1]);
    let sub = dns::answer(&query("Tenant.Shop.TEST", 1), "test").unwrap();
    assert_eq!(&sub[sub.len() - 4..], &[127, 0, 0, 1], "sub-domains and upper case work");
    let aaaa = dns::answer(&query("shop.test", 28), "test").unwrap();
    assert_eq!(u16::from_be_bytes([aaaa[6], aaaa[7]]), 0, "no IPv6 answer, so clients fall back to IPv4 at once");
    assert_eq!(aaaa[3] & 0x0F, 0, "but it is not an error");
    let other = dns::answer(&query("example.com", 1), "test").unwrap();
    assert_eq!(other[3] & 0x0F, 3, "names outside the suffix are NXDOMAIN");
    assert!(dns::answer(&[1, 2, 3], "test").is_none(), "garbage is ignored");

    // a real DNS client (nslookup) talking to the real server over UDP
    let h = dns::start("127.0.0.1:15353", "test").expect("start dns on 15353");
    // the server itself, over a real UDP socket
    let ask = |name: &str, qtype: u16| -> Vec<u8> {
        let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        sock.send_to(&query(name, qtype), "127.0.0.1:15353").unwrap();
        let mut buf = [0u8; 512];
        let (n, _) = sock.recv_from(&mut buf).expect("the DNS server must answer");
        buf[..n].to_vec()
    };
    for name in ["shop.test", "a.b.c.shop.test", "anything-at-all.test"] {
        let r = ask(name, 1);
        assert_eq!(&r[r.len() - 4..], &[127, 0, 0, 1], "{name} should resolve to 127.0.0.1");
    }
    assert_eq!(ask("example.com", 1)[3] & 0x0F, 3, "outside names are refused over the wire too");
    h.stop();
    assert!(std::net::UdpSocket::bind("127.0.0.1:15353").is_ok(), "port is free again after stop");
}

#[test]
fn workers_run_in_the_project_with_its_php() {
    let g = setup();
    let core = &g.0;
    m::create_site("jobs").unwrap();
    let dir = m::www_dir().join("jobs");
    std::fs::write(dir.join("loop.php"), "<?php $i=0; while(true){ file_put_contents(__DIR__.'/tick.txt', PHP_VERSION.' '.(++$i).\"\\n\", FILE_APPEND); sleep(1);} ").unwrap();

    assert!(core.add_worker("nope", "x", "php loop.php").is_err(), "unknown site");
    assert!(core.add_worker("jobs", "bad name!", "php loop.php").is_err(), "bad name");
    assert!(core.add_worker("jobs", "q", "   ").is_err(), "empty command");
    core.add_worker("jobs", "ticker", "php loop.php").unwrap();
    assert!(core.add_worker("jobs", "ticker", "php loop.php").is_err(), "duplicate");
    core.set_worker_autostart("jobs", "ticker", true).unwrap();
    assert!(core.overview().workers.iter().any(|w| w.name == "ticker" && w.autostart && !w.running));

    core.start_worker("jobs", "ticker").expect("start worker");
    std::thread::sleep(Duration::from_millis(3500));
    let ov = core.overview();
    let w = ov.workers.iter().find(|w| w.name == "ticker").unwrap();
    assert!(w.running && w.pid.is_some());
    let ticks = std::fs::read_to_string(dir.join("tick.txt")).unwrap_or_default();
    println!("ticks:\n{ticks}");
    assert!(ticks.lines().count() >= 2, "worker should be running: {ticks}");
    let default_php = m::versions("php")[0].clone();
    assert!(ticks.starts_with(&default_php), "worker uses the project's PHP ({default_php}): {ticks}");

    core.stop_worker("jobs", "ticker").unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    let after = std::fs::read_to_string(dir.join("tick.txt")).unwrap().lines().count();
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(after, std::fs::read_to_string(dir.join("tick.txt")).unwrap().lines().count(), "worker must really be stopped");
    assert!(!core.overview().workers.iter().any(|w| w.running));

    // a command that fails at once is reported
    core.add_worker("jobs", "broken", "php does-not-exist.php").unwrap();
    assert!(core.start_worker("jobs", "broken").is_err());
    core.remove_worker("jobs", "broken").unwrap();
    core.remove_worker("jobs", "ticker").unwrap();
    assert!(core.overview().workers.is_empty());
}

fn np(name: &str, kind: &str, database: bool, npm: bool) -> jojo_server_lib::projects::NewProject {
    jojo_server_lib::projects::NewProject { name: name.into(), kind: kind.into(), database, npm }
}

#[test]
fn new_projects_wordpress_and_laravel_work_out_of_the_box() {
    let g = setup();
    let core = &g.0;
    m::set_port("nginx", 8081).unwrap();
    m::set_port("php", 9100).unwrap();
    m::set_port("mysql", 3307).unwrap();
    m::set_port("smtp", 1035).unwrap();

    // Use the newest Node LTS, like the app does on a fresh install (the old npm 10.9 of Node 22.11 cannot
    // install the current Laravel front end).
    let _ = m::refresh_catalog(core.emit());
    let key = |v: &str| v.split('.').map(|p| p.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    let best = m::catalog()
        .into_iter()
        .filter(|c| c.service == "node" && c.note.starts_with("LTS"))
        .max_by(|a, b| key(&a.version).cmp(&key(&b.version)))
        .expect("a Node LTS in the catalog");
    println!("using Node {}", best.version);
    if !m::versions("node").contains(&best.version) {
        core.install("node", &best.version, &best.url).expect("install node lts");
    }
    m::set_active("node", &best.version).unwrap();

    // blank + refusals
    assert_eq!(core.create_project(np("plain", "blank", false, false)).unwrap(), "http://plain.test");
    assert!(m::www_dir().join("plain").join("index.php").is_file());
    assert!(core.create_project(np("plain", "blank", false, false)).is_err(), "name already used");
    assert!(core.create_project(np("x", "rails", false, false)).is_err(), "unknown type");
    assert!(core.create_project(np("!!!", "blank", false, false)).is_err(), "invalid name");

    // WordPress with a database
    let url = core.create_project(np("blog", "wordpress", true, false)).expect("wordpress");
    assert_eq!(url, "http://blog.test");
    let cfg = std::fs::read_to_string(m::www_dir().join("blog").join("wp-config.php")).unwrap();
    assert!(cfg.contains("'blog'") && cfg.contains("127.0.0.1:3307"), "database details are written into wp-config.php");
    assert!(!cfg.contains("put your unique phrase here"), "fresh secret keys are used");
    core.start("php").unwrap();
    core.start("nginx").unwrap();
    let page = curl(&["-H", "Host: blog.test", "http://127.0.0.1:8081/wp-admin/install.php"]);
    println!("wordpress installer page starts: {}", &page[..page.len().min(160)].replace('\n', " "));
    assert!(page.contains("WordPress"), "WordPress install screen should load: {}", &page[..page.len().min(300)]);
    assert!(!page.contains("Error establishing a database connection"), "database must be reachable");

    // Laravel with MySQL and npm
    let url = core.create_project(np("shop", "laravel", true, true)).expect("laravel");
    assert_eq!(url, "http://shop.test");
    let dir = m::www_dir().join("shop");
    assert!(dir.join("artisan").is_file() && dir.join("vendor").is_dir(), "composer installed the framework");
    assert!(dir.join("node_modules").is_dir(), "npm install ran");
    let env = std::fs::read_to_string(dir.join(".env")).unwrap();
    println!("--- .env (selected)");
    for l in env.lines().filter(|l| l.starts_with("APP_URL") || l.starts_with("DB_") || l.starts_with("MAIL_")) {
        println!("{l}");
    }
    assert!(env.contains("APP_URL=http://shop.test"));
    assert!(env.contains("DB_CONNECTION=mysql") && env.contains("DB_DATABASE=shop") && env.contains("DB_PORT=3307"));
    assert!(env.contains("MAIL_MAILER=smtp") && env.contains("MAIL_PORT=1035"));
    core.apply_site_changes().unwrap(); // pick up the new site (what the app does after creating a project)
    let home = curl(&["-H", "Host: shop.test", "http://127.0.0.1:8081/"]);
    assert!(home.contains("Laravel"), "Laravel welcome page should load: {}", &home[..home.len().min(300)]);
    let tables = {
        let ver = m::versions("mysql")[0].clone();
        let mysql = m::root().join("bin").join("mysql").join(ver).join("bin").join("mysql.exe");
        let out = Command::new(mysql).args(["-u", "root", "-h", "127.0.0.1", "-P", "3307", "-N", "-e", "SHOW TABLES", "shop"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    println!("tables in shop: {}", tables.replace('\n', ", "));
    assert!(tables.contains("migrations") && tables.contains("users"), "migrations ran against MySQL");
}

#[test]
fn a_site_can_be_shared_on_the_internet_and_unshared() {
    let g = setup();
    let core = &g.0;
    m::set_port("nginx", 8081).unwrap();
    m::set_port("php", 9100).unwrap();
    m::create_site("pub").unwrap();
    assert!(core.share_start("pub").unwrap_err().contains("Nginx"), "needs a running web server");
    core.start("php").unwrap();
    core.start("nginx").unwrap();
    let url = core.share_start("pub").expect("share");
    println!("public url: {url}");
    assert!(url.starts_with("https://") && url.ends_with(".trycloudflare.com"));
    assert_eq!(core.share_start("pub").unwrap(), url, "sharing twice returns the same link");
    assert_eq!(core.overview().shares.get("pub"), Some(&url));

    let mut ok = false;
    for _ in 0..20 {
        let body = curl(&["-m", "15", &url]);
        if body.contains("pub.test is live") {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    assert!(ok, "the public link should show the site");
    core.share_stop("pub");
    assert!(core.overview().shares.is_empty());
    assert!(!core.is_running("share:pub"), "tunnel process is gone");
    assert!(jojo_server_lib::tools::find_tunnel_url("INF |  https://random-words-here.trycloudflare.com  |").is_some());
    assert!(jojo_server_lib::tools::find_tunnel_url("INF visit https://www.cloudflare.com/docs").is_none());
}

#[test]
fn apache_serves_sites_with_htaccess_https_and_switches_with_nginx() {
    let g = setup();
    let core = &g.0;
    if m::versions("apache").is_empty() {
        let e = m::catalog().into_iter().find(|c| c.service == "apache").expect("apache in catalog");
        core.install("apache", &e.version, &e.url).expect("install apache");
    }
    m::set_port("nginx", 8081).unwrap(); // the HTTP port is shared by both web servers
    assert_eq!(m::service_port("apache"), 8081, "Apache uses the same HTTP port setting");
    m::set_port("php", 9100).unwrap();
    m::set_port("https", 8443).unwrap();
    let vers = m::versions("php");
    assert!(vers.len() >= 2);

    m::create_site("legacy").unwrap();
    let dir = m::www_dir().join("legacy");
    std::fs::write(dir.join(".htaccess"), "RewriteEngine On\nRewriteRule ^pretty/(.*)$ x.php?x=$1 [L,QSA]\n").unwrap();
    std::fs::write(dir.join("x.php"), "<?php echo 'x=' . ($_GET['x'] ?? 'none');").unwrap();
    std::fs::write(dir.join("h.php"), "<?php echo isset($_SERVER['HTTPS']) ? 'https=' . $_SERVER['HTTPS'] : 'no-https';").unwrap();

    // start with Nginx, then switch to Apache: Nginx must stop by itself
    core.start("php").unwrap();
    core.start("nginx").unwrap();
    assert!(curl(&["-i", "-H", "Host: legacy.test", "http://127.0.0.1:8081/"]).contains("Server: nginx"));
    core.start("apache").expect("apache start");
    assert!(core.is_running("apache") && !core.is_running("nginx"), "starting Apache replaces Nginx");
    assert_eq!(core.overview().web_server, "apache", "the choice is remembered");

    let home = curl(&["-i", "-H", "Host: legacy.test", "http://127.0.0.1:8081/"]);
    println!("apache home: {}", home.replace('\r', "").replace('\n', " | ").chars().take(260).collect::<String>());
    assert!(home.contains("Server: Apache"), "answered by Apache: {home}");
    assert!(home.contains("legacy.test is live"), "PHP runs through FastCGI: {home}");
    assert!(home.contains(&format!("PHP {}", vers[0])), "default PHP version");

    // .htaccess works, and sub-domains of a site are served too
    assert_eq!(curl(&["-H", "Host: legacy.test", "http://127.0.0.1:8081/pretty/hello"]).trim(), "x=hello", ".htaccess rewrite");
    assert!(curl(&["-H", "Host: tenant.legacy.test", "http://127.0.0.1:8081/"]).contains("legacy.test is live"), "wildcard sub-domain");
    assert!(curl(&["-i", "http://127.0.0.1:8081/"]).starts_with("HTTP/1.1 200"), "the default site (localhost) answers");

    // per-site PHP version
    m::set_site_config(core.emit(), "legacy", &vers[1], false).unwrap();
    core.apply_site_changes().unwrap();
    assert!(core.is_running("apache"), "apply_site_changes restarts the running web server, whichever it is");
    assert!(curl(&["-H", "Host: legacy.test", "http://127.0.0.1:8081/"]).contains(&format!("PHP {}", vers[1])), "per-site PHP version");

    // HTTPS
    m::fetch_mkcert(core.emit()).unwrap();
    let mut s = m::load_settings();
    s.ssl_ready = true;
    m::save_settings(&s).unwrap();
    m::set_site_config(core.emit(), "legacy", &vers[1], true).unwrap();
    core.apply_site_changes().unwrap();
    let secure = curl(&["-k", "-H", "Host: legacy.test", "https://127.0.0.1:8443/h.php"]);
    assert_eq!(secure.trim(), "https=on", "HTTPS works and PHP knows about it: {secure}");

    // logs
    let ids: Vec<String> = m::log_sources().into_iter().map(|s| s.id).collect();
    assert!(ids.contains(&"apache-error".to_string()) && ids.contains(&"apache-access".to_string()));
    assert!(m::read_log("apache-access", 100).unwrap().lines.iter().any(|l| l.contains("/pretty/hello")), "requests are logged");

    // back to Nginx
    core.set_web_server("nginx").unwrap();
    assert!(core.is_running("nginx") && !core.is_running("apache"), "switching back stops Apache");
    assert!(curl(&["-i", "-H", "Host: legacy.test", "http://127.0.0.1:8081/"]).contains("Server: nginx"));

    // Start All starts only the chosen web server
    core.stop_all();
    std::thread::sleep(Duration::from_millis(800));
    assert!(!port_open(8081), "port is free after stop_all");
    core.set_web_server("apache").unwrap();
    core.start_all();
    assert!(core.is_running("apache") && !core.is_running("nginx"), "start_all starts the chosen web server only");

    // a clean stop frees the port (Apache's own shutdown command worked, or it was ended safely)
    core.stop_all();
    std::thread::sleep(Duration::from_millis(800));
    assert!(!port_open(8081) && !port_open(8443), "Apache stopped and released its ports");
}

#[test]
fn dbgate_installs_and_is_found() {
    let g = setup();
    let exe_before = jojo_server_lib::tools::dbgate_exe();
    g.0.install_dbgate().expect("install dbgate");
    let exe = jojo_server_lib::tools::dbgate_exe().expect("DbGate.exe found");
    println!("dbgate at {} (was installed before: {})", exe.display(), exe_before.is_some());
    assert!(exe.is_file());
    assert!(g.0.overview().dbgate);
}

#[test]
fn mail_reaches_mailpit_and_logs_can_be_read() {
    let g = setup();
    let core = &g.0;
    if m::versions("mailpit").is_empty() {
        let e = m::catalog().into_iter().find(|c| c.service == "mailpit").unwrap();
        core.install("mailpit", &e.version, &e.url).expect("install mailpit");
    }
    m::set_port("mailpit", 8035).unwrap();
    m::set_port("smtp", 1035).unwrap();
    m::set_port("nginx", 8081).unwrap();
    m::set_port("php", 9100).unwrap();

    assert!(m::send_test_mail().unwrap_err().contains("not running"), "clear message when Mailpit is off");
    core.start("mailpit").unwrap();
    m::send_test_mail().expect("test mail");

    // PHP's own mail() goes to Mailpit with no configuration by the user.
    m::create_site("mailer").unwrap();
    std::fs::write(
        m::www_dir().join("mailer").join("m.php"),
        "<?php var_dump(mail('dev@example.test', 'From PHP mail()', 'hello from php')); trigger_error('boom from test', E_USER_WARNING);",
    )
    .unwrap();
    core.start("php").unwrap();
    core.start("nginx").unwrap();
    let out = curl(&["-i", "-m", "25", "-w", "\nHTTP:%{http_code} time:%{time_total}", "-H", "Host: mailer.test", "http://127.0.0.1:8081/m.php"]);
    println!("m.php says: {}", out.replace('\n', " | "));
    assert!(out.contains("bool(true)"), "PHP mail() should succeed: {out}");

    let api = curl(&["http://127.0.0.1:8035/api/v1/messages"]);
    assert!(api.contains("JOJO Server test email"), "test mail should be in the inbox: {api}");
    assert!(api.contains("From PHP mail()"), "PHP mail() should be in the inbox: {api}");

    // logs: PHP errors, Nginx and a Laravel-style log
    let lara = m::www_dir().join("mailer").join("storage").join("logs");
    std::fs::create_dir_all(&lara).unwrap();
    let mut text = String::new();
    for i in 0..300 {
        text.push_str(&format!("[2026-10-25 10:00:{:02}] local.ERROR: line number {i}\n", i % 60));
    }
    std::fs::write(lara.join("laravel.log"), &text).unwrap();
    let ids: Vec<String> = m::log_sources().into_iter().map(|s| s.id).collect();
    println!("log sources: {ids:?}");
    assert!(ids.contains(&"laravel:mailer".to_string()) && ids.contains(&"php".to_string()));
    let l = m::read_log("laravel:mailer", 100).unwrap();
    assert_eq!(l.lines.len(), 100);
    assert!(l.lines.last().unwrap().contains("line number 299"), "tail should end with the newest line");
    let php = m::read_log("php", 100).unwrap();
    assert!(php.lines.iter().any(|x| x.contains("boom from test")), "PHP warning should be logged: {:?}", php.lines);
    assert!(m::read_log("nginx-access", 50).unwrap().lines.iter().any(|x| x.contains("m.php")));
    m::clear_log("laravel:mailer").unwrap();
    assert!(m::read_log("laravel:mailer", 100).unwrap().lines.is_empty());
    assert!(m::read_log("../../etc/passwd", 10).is_err(), "unknown sources are refused");
}

#[test]
fn composer_is_added_automatically_and_works_in_a_terminal() {
    let g = setup();
    let dir = m::root().join("bin").join("tools").join("composer");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!m::composer_installed());

    g.0.ensure_composer(); // what the Terminal button and app start-up do
    assert!(m::composer_installed(), "composer should be fetched automatically");

    // A terminal opened for a project can run `composer` straight away.
    m::create_site("proj").unwrap();
    let (path, _t, cwd, phprc) = m::terminal_env(Some("proj")).unwrap();
    let mut c = Command::new("cmd");
    c.args(["/c", "composer", "--version"]).current_dir(&cwd).env("PATH", path);
    if let Some(p) = phprc {
        c.env("PHPRC", p);
    }
    let out = c.output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    println!("{text}");
    assert!(text.contains("Composer version"), "composer must run in the terminal: {text} {}", String::from_utf8_lossy(&out.stderr));

    // A second call changes nothing (no re-download).
    let before = std::fs::metadata(dir.join("composer.phar")).unwrap().modified().unwrap();
    g.0.ensure_composer();
    assert_eq!(before, std::fs::metadata(dir.join("composer.phar")).unwrap().modified().unwrap());
}

#[test]
fn composer_runs_on_the_projects_php() {
    let g = setup();
    let vers = m::versions("php");
    assert!(vers.len() >= 2);
    g.0.install_composer().expect("install composer");
    assert!(m::composer_installed());
    m::create_site("proj-a").unwrap();
    m::create_site("proj-b").unwrap();
    m::set_site_config(g.0.emit(), "proj-a", &vers[0], false).unwrap();
    m::set_site_config(g.0.emit(), "proj-b", &vers[1], false).unwrap();

    for (site, want) in [("proj-a", &vers[0]), ("proj-b", &vers[1])] {
        let (path, _t, cwd, phprc) = m::terminal_env(Some(site)).unwrap();
        let mut c = Command::new("cmd");
        c.args(["/c", "composer", "diagnose", "--no-interaction"]).current_dir(&cwd).env("PATH", path);
        if let Some(p) = phprc {
            c.env("PHPRC", p);
        }
        let out = c.output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        println!("--- composer diagnose in {site}:\n{text}");
        assert!(text.contains("Composer version"), "composer should run: {text}");
        assert!(text.contains(&format!("PHP version: {want}")) || text.contains(want.as_str()), "{site} should use PHP {want}: {text}");
    }
}

#[test]
fn php_can_make_https_requests() {
    // Regression: "cURL error 60: SSL certificate problem: unable to get local issuer certificate".
    let _g = setup();
    let ver = m::versions("php")[0].clone();
    let (_, _, _, phprc) = m::terminal_env(None).unwrap(); // writes php.ini
    let ini_dir = phprc.unwrap();
    let ini = std::fs::read_to_string(ini_dir.join("php.ini")).unwrap();
    assert!(ini.contains("curl.cainfo"), "php.ini must point curl at a CA bundle");
    assert!(ini.contains("openssl.cafile"), "php.ini must point openssl at a CA bundle");
    let script = m::root().join("run").join("tls.php");
    std::fs::write(&script, r#"<?php
$c = curl_init('https://api.github.com/');
curl_setopt_array($c, [CURLOPT_RETURNTRANSFER => 1, CURLOPT_USERAGENT => 'jojo', CURLOPT_TIMEOUT => 20]);
curl_exec($c);
echo 'curl:', curl_getinfo($c, CURLINFO_HTTP_CODE), ':', curl_errno($c), ' ';
$ctx = stream_context_create(['http' => ['header' => "User-Agent: jojo\r\n", 'timeout' => 20, 'ignore_errors' => true]]);
$body = @file_get_contents('https://api.github.com/', false, $ctx);
echo 'stream:', $body === false ? 'FAILED' : 'ok';
"#).unwrap();
    let out = php_cli(&ver, &ini_dir, &[&script.to_string_lossy()]);
    println!("{out}");
    assert!(out.contains("curl:200:0"), "curl HTTPS must verify certificates: {out}");
    assert!(out.contains("stream:ok"), "PHP streams must verify certificates: {out}");
}

#[test]
fn restart_all_restarts_only_what_is_running() {
    let g = setup();
    let core = &g.0;
    m::set_port("nginx", 8081).unwrap();
    m::set_port("php", 9100).unwrap();
    m::set_port("mysql", 3307).unwrap();
    m::create_site("e2e").unwrap();
    core.start("php").unwrap();
    core.start("nginx").unwrap();
    let pid = |id: &str| core.overview().services.into_iter().find(|s| s.id == id).and_then(|s| s.pid);
    let (php0, nginx0) = (pid("php").unwrap(), pid("nginx").unwrap());

    core.restart_running();

    let (php1, nginx1) = (pid("php").expect("php running"), pid("nginx").expect("nginx running"));
    assert_ne!(php0, php1, "php should have been restarted");
    assert_ne!(nginx0, nginx1, "nginx should have been restarted");
    assert!(!core.is_running("mysql"), "a stopped service must stay stopped");
    let body = curl(&["-H", "Host: e2e.test", "http://127.0.0.1:8081/"]);
    assert!(body.contains("e2e.test is live"), "site must work after restart: {body}");
}

#[test]
fn project_terminals_each_see_their_own_php() {
    let g = setup();
    let vers = m::versions("php");
    assert!(vers.len() >= 2, "needs two PHP versions");
    let (a, b) = (vers[0].clone(), vers[1].clone());
    m::create_site("proj-a").unwrap();
    m::create_site("proj-b").unwrap();
    m::create_site("proj-default").unwrap();
    m::set_active("php", &a).unwrap();
    m::set_site_config(g.0.emit(), "proj-a", &a, false).unwrap();
    m::set_site_config(g.0.emit(), "proj-b", &b, false).unwrap();

    // Same environment the Terminal button gives a new command prompt, but run through `cmd /c php -v`.
    let version_in_terminal = |site: Option<&str>| -> (String, String) {
        let (path, _title, cwd, phprc) = m::terminal_env(site).unwrap();
        let mut c = Command::new("cmd");
        c.args(["/c", "php", "-r", "echo PHP_VERSION, '|', getcwd();"]).current_dir(&cwd).env("PATH", path);
        if let Some(p) = phprc {
            c.env("PHPRC", p);
        }
        let out = c.output().unwrap();
        let s = String::from_utf8_lossy(&out.stdout).to_string();
        let (v, d) = s.split_once('|').unwrap_or((&s, ""));
        (v.to_string(), d.to_string())
    };
    let (va, da) = version_in_terminal(Some("proj-a"));
    let (vb, db) = version_in_terminal(Some("proj-b"));
    let (vd, _) = version_in_terminal(Some("proj-default"));
    let (vn, _) = version_in_terminal(None);
    println!("proj-a: PHP {va} in {da}\nproj-b: PHP {vb} in {db}\nproj-default: PHP {vd}\nno project: PHP {vn}");
    assert_eq!(va, a);
    assert_eq!(vb, b);
    assert_eq!(vd, a, "site without its own version uses the default");
    assert_eq!(vn, a);
    assert!(da.ends_with("proj-a") && db.ends_with("proj-b"), "each terminal opens in its project folder");
}

#[test]
fn php_extensions_settings_and_general() {
    let g = setup();
    let core = &g.0;
    let ver = m::versions("php")[0].clone();
    for n in ["redis", "xdebug", "apcu"] {
        let _ = m::remove_php_extension(&ver, n); // leftovers from an earlier failed run
    }

    // bundled extensions are listed, downloadable ones are not there yet
    let list = m::php_extensions(&ver).unwrap();
    assert!(list.iter().any(|e| e.name == "mbstring" && e.enabled), "mbstring should be on by default");
    assert!(!list.iter().any(|e| e.name == "redis"));

    // install three extensions from PECL
    for n in ["redis", "xdebug", "apcu"] {
        m::install_php_extension(core.emit(), &ver, n).unwrap_or_else(|e| panic!("install {n}: {e}"));
    }
    let (_, _, _, phprc) = m::terminal_env(None).unwrap();
    let ini_dir = phprc.expect("php ini folder");
    let modules = php_cli(&ver, &ini_dir, &["-m"]).to_lowercase();
    println!("modules mention: redis={} apcu={} xdebug={}", modules.contains("redis"), modules.contains("apcu"), modules.contains("xdebug"));
    for n in ["redis", "apcu", "xdebug", "mbstring"] {
        assert!(modules.contains(n), "{n} should be loaded: {modules}");
    }

    // switch one off again
    m::set_php_extension(&ver, "redis", false).unwrap();
    let _ = m::terminal_env(None).unwrap(); // regenerates php.ini
    let modules = php_cli(&ver, &ini_dir, &["-m"]).to_lowercase();
    assert!(!modules.contains("redis"), "redis should be off: {modules}");
    assert!(modules.contains("apcu"));

    // runtime settings
    let mut conf = m::load_settings().php_conf;
    conf.memory_limit = "1G".into();
    conf.max_execution_time = 77;
    conf.timezone = "Asia/Dhaka".into();
    conf.display_errors = false;
    conf.extra = "precision = 10".into();
    m::set_php_conf(conf.clone()).unwrap();
    let _ = m::terminal_env(None).unwrap();
    let got = php_cli(&ver, &ini_dir, &["-r", "echo ini_get('memory_limit'),'|',date_default_timezone_get(),'|',ini_get('precision');"]);
    println!("ini says: {got}");
    assert_eq!(got.trim(), "1G|Asia/Dhaka|10"); // (the command line always ignores max_execution_time)
    conf.memory_limit = "lots".into();
    assert!(m::set_php_conf(conf.clone()).is_err(), "bad memory limit must be rejected");
    conf.memory_limit = "1G".into();
    conf.timezone = "Not A Zone!".into();
    assert!(m::set_php_conf(conf).is_err(), "bad timezone must be rejected");

    // settings also reach the web server
    m::set_port("nginx", 8081).unwrap();
    m::set_port("php", 9100).unwrap();
    m::create_site("e2e").unwrap();
    std::fs::write(m::root().join("www").join("e2e").join("ini.php"), "<?php echo ini_get('memory_limit'), '|', ini_get('max_execution_time'), '|', extension_loaded('apcu') ? 'apcu' : 'no-apcu';").unwrap();
    core.start("php").unwrap();
    core.start("nginx").unwrap();
    let body = curl(&["-H", "Host: e2e.test", "http://127.0.0.1:8081/ini.php"]);
    assert_eq!(body.trim(), "1G|77|apcu", "web PHP did not use the settings: {body}");

    // changing a setting restarts PHP and applies immediately
    let mut conf = m::load_settings().php_conf;
    conf.memory_limit = "256M".into();
    m::set_php_conf(conf).unwrap();
    core.reload_php().unwrap();
    let body = curl(&["-H", "Host: e2e.test", "http://127.0.0.1:8081/ini.php"]);
    assert_eq!(body.trim(), "256M|77|apcu");
    core.stop_all();

    // clean up what this test downloaded, and check removal works
    for n in ["redis", "xdebug", "apcu"] {
        m::remove_php_extension(&ver, n).unwrap();
    }
    assert!(!m::php_extensions(&ver).unwrap().iter().any(|e| e.name == "apcu"));
    assert!(m::remove_php_extension(&ver, "mbstring").is_err(), "bundled extensions cannot be removed");

    // general settings validation
    let mut gen = m::General::default();
    gen.autostart = vec!["not-a-service".into()];
    assert!(m::set_general(gen).is_err());
    let mut gen = m::General::default();
    gen.autostart = vec!["nginx".into(), "php".into()];
    m::set_general(gen).unwrap();
    assert_eq!(m::load_settings().general.autostart, vec!["nginx", "php"]);
}

#[test]
fn redis_browser_reads_and_writes_every_key_type() {
    use jojo_server_lib::redis as r;
    use serde_json::json;
    let g = setup();
    let core = &g.0;
    if m::versions("redis").is_empty() {
        let e = m::catalog().into_iter().find(|c| c.service == "redis").expect("redis in catalog");
        core.install("redis", &e.version, &e.url).expect("install redis");
    }
    m::set_port("redis", 6390).unwrap();
    core.start("redis").expect("redis start");

    r::set_string(0, "user:1", "hello", None).unwrap();
    r::set_string(0, "temp", "x", Some(100)).unwrap();
    for line in ["RPUSH jobs a b c", "HSET profile name \"Jojo Server\" age 3", "SADD tags php redis", "ZADD board 1 alice 2 bob"] {
        assert!(r::exec(0, line).unwrap().ok, "failed: {line}");
    }

    let page = r::keys(0, "*", "", 100).unwrap();
    println!("keys: {:?}", page.keys.iter().map(|k| format!("{}:{}", k.key, k.kind)).collect::<Vec<_>>());
    assert_eq!(page.dbsize, 6);
    let kind = |k: &str| page.keys.iter().find(|x| x.key == k).map(|x| x.kind.clone()).unwrap();
    assert_eq!(kind("user:1"), "string");
    assert_eq!(kind("jobs"), "list");
    assert_eq!(kind("profile"), "hash");
    assert_eq!(kind("tags"), "set");
    assert_eq!(kind("board"), "zset");
    assert_eq!(r::keys(0, "user:*", "", 100).unwrap().keys.len(), 1);

    assert_eq!(r::get(0, "user:1").unwrap().value, json!("hello"));
    assert_eq!(r::get(0, "jobs").unwrap().value, json!(["a", "b", "c"]));
    let h = r::get(0, "profile").unwrap();
    assert_eq!(h.size, 2);
    assert!(h.value.as_array().unwrap().contains(&json!(["name", "Jojo Server"])));
    assert_eq!(r::get(0, "board").unwrap().value, json!([["alice", "1"], ["bob", "2"]]));
    assert_eq!(r::get(0, "tags").unwrap().size, 2);
    let t = r::get(0, "temp").unwrap();
    assert!(t.ttl > 90 && t.ttl <= 100, "ttl was {}", t.ttl);

    r::expire(0, "temp", None).unwrap();
    assert_eq!(r::get(0, "temp").unwrap().ttl, -1);
    r::expire(0, "temp", Some(50)).unwrap();
    assert!(r::get(0, "temp").unwrap().ttl > 40);

    r::delete(0, "user:1").unwrap();
    assert!(r::get(0, "user:1").is_err());

    let ok = r::exec(0, "SET greeting \"hi there\"").unwrap();
    assert!(ok.ok);
    assert_eq!(r::exec(0, "GET greeting").unwrap().reply, json!("hi there"));
    let bad = r::exec(0, "NOTACOMMAND x").unwrap();
    assert!(!bad.ok, "unknown command should report an error");
    assert!(r::exec(0, "MONITOR").is_err());
    assert!(r::exec(0, "GET \"unterminated").is_err());

    // databases are separate
    r::set_string(1, "only-in-db1", "1", None).unwrap();
    assert_eq!(r::keys(1, "*", "", 100).unwrap().keys.len(), 1);
    let info = r::info(0).unwrap();
    println!("redis {} mem {} dbs {:?}", info.version, info.memory, info.dbs.iter().map(|d| (d.db, d.keys)).collect::<Vec<_>>());
    assert!(!info.version.is_empty());
    assert!(info.dbs.iter().any(|d| d.db == 1));

    r::flush_db(0).unwrap();
    assert_eq!(r::keys(0, "*", "", 100).unwrap().dbsize, 0);
    assert_eq!(r::keys(1, "*", "", 100).unwrap().dbsize, 1, "flush must only clear the selected database");
    r::flush_db(1).unwrap();

    core.stop("redis").unwrap();
    assert!(r::keys(0, "*", "", 10).is_err(), "should report that Redis is not running");
}

#[test]
fn discovers_new_versions_from_official_sites() {
    let g = setup();
    let _ = std::fs::remove_file(m::root().join("catalog-cache.json"));
    assert!(m::catalog_cache_stale());
    let added = m::refresh_catalog(g.0.emit()).expect("refresh");
    println!("added {added} entries");
    assert!(!m::catalog_cache_stale());
    let cat = m::catalog();
    for svc in ["php", "nginx", "apache", "mailpit", "redis", "mysql", "postgres", "node"] {
        let list: Vec<String> = cat.iter().filter(|c| c.service == svc).map(|c| c.version.clone()).collect();
        println!("{svc}: {}", list.join(", "));
        assert!(!list.is_empty(), "no versions found for {svc}");
    }
    // PHP entries come with a checksum and the newest branch is offered.
    let php8x: Vec<_> = cat.iter().filter(|c| c.service == "php" && c.version.starts_with("8.")).collect();
    assert!(php8x.iter().any(|c| c.sha256.is_some()), "php entries should carry sha256");
    let first = &cat.iter().find(|c| c.service == "php").unwrap().version;
    println!("newest php offered: {first}");
}

#[test]
fn adminer_logs_in_to_passwordless_mysql() {
    let g = setup();
    let core = &g.0;
    m::set_port("nginx", 8081).unwrap();
    m::set_port("php", 9100).unwrap();
    m::set_port("mysql", 3307).unwrap();

    core.install_adminer().expect("adminer install");
    core.start("php").unwrap();
    core.start("nginx").unwrap();
    core.start("mysql").unwrap();

    let page = curl(&["-H", "Host: adminer.test", "http://127.0.0.1:8081/"]);
    assert!(page.contains("Adminer"), "adminer page missing: {}", &page[..page.len().min(300)]);

    let jar = m::root().join("run").join("adminer.cookies");
    let jar = jar.to_string_lossy().to_string();
    let after = curl(&[
        "-L", "-c", &jar, "-b", &jar, "-H", "Host: adminer.test",
        "-d", "auth[driver]=server&auth[server]=127.0.0.1:3307&auth[username]=root&auth[password]=&auth[db]=",
        "http://127.0.0.1:8081/",
    ]);
    assert!(
        after.contains("information_schema") || after.contains("Select database") || after.contains("Logout"),
        "login failed: {}",
        &after[..after.len().min(600)]
    );
}

#[test]
fn nginx_php_mysql_https_and_per_site_php() {
    let g = setup();
    let core = &g.0;

    m::set_port("nginx", 8081).unwrap();
    m::set_port("php", 9100).unwrap();
    m::set_port("mysql", 3307).unwrap();
    m::set_port("https", 8443).unwrap();

    let php_versions = m::versions("php");
    println!("installed php: {:?}", php_versions);
    assert!(php_versions.len() >= 2, "need two PHP versions installed for this test");
    m::set_active("php", &php_versions[0]).unwrap();

    m::create_site("e2e").unwrap();

    // --- start everything
    core.start("php").expect("php start");
    core.start("nginx").expect("nginx start");
    core.start("mysql").expect("mysql start");
    assert!(core.is_running("php") && core.is_running("nginx") && core.is_running("mysql"));

    // --- site served through nginx + php
    let body = curl(&["-H", "Host: e2e.test", "http://127.0.0.1:8081/"]);
    println!("default php body: {body}");
    assert!(body.contains("e2e.test is live"), "site did not render: {body}");
    assert!(body.contains(&format!("PHP {}", php_versions[0])), "wrong PHP version: {body}");

    // --- MySQL answers queries
    let mysql = m::root()
        .join("bin")
        .join("mysql")
        .join(m::versions("mysql")[0].clone())
        .join("bin")
        .join("mysql.exe");
    let out = Command::new(&mysql)
        .args(["-u", "root", "-h", "127.0.0.1", "-P", "3307", "-N", "-e", "SELECT 42"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    println!("mysql says: {text}");
    assert!(text.trim() == "42", "mysql query failed: {text} / {}", String::from_utf8_lossy(&out.stderr));

    // --- per-site PHP version
    let other = php_versions[1].clone();
    m::set_site_config(core.emit(), "e2e", &other, false).unwrap();
    core.apply_site_changes().unwrap();
    let body = curl(&["-H", "Host: e2e.test", "http://127.0.0.1:8081/"]);
    println!("per-site php body: {body}");
    assert!(body.contains(&format!("PHP {other}")), "per-site PHP not applied: {body}");

    // --- HTTPS (certificate is generated, CA trust is not installed in the test)
    m::fetch_mkcert(core.emit()).unwrap();
    let mut s = m::load_settings();
    s.ssl_ready = true;
    m::save_settings(&s).unwrap();
    m::set_site_config(core.emit(), "e2e", &other, true).unwrap();
    core.apply_site_changes().unwrap();
    let body = curl(&["-k", "-H", "Host: e2e.test", "https://127.0.0.1:8443/"]);
    println!("https body: {body}");
    assert!(body.contains("e2e.test is live"), "https did not work: {body}");

    // --- several sites on different PHP versions at the same time
    m::create_site("e2e-b").unwrap();
    m::create_site("e2e-c").unwrap();
    m::set_site_config(core.emit(), "e2e-b", &php_versions[0], false).unwrap(); // 8.3.12
    m::set_site_config(core.emit(), "e2e-c", &other, false).unwrap(); // 8.2.24
    core.apply_site_changes().unwrap();
    for (site, want) in [("e2e", &other), ("e2e-b", &php_versions[0]), ("e2e-c", &other)] {
        let host = format!("Host: {site}.test");
        let body = curl(&["-H", &host, "http://127.0.0.1:8081/"]);
        assert!(body.contains(&format!("PHP {want}")), "{site} should run PHP {want}: {body}");
        println!("{site} -> PHP {want} OK");
    }

    // --- per-site terminal: PATH starts with this site's PHP, cwd is the project folder
    let (path, title, cwd, phprc) = m::terminal_env(Some("e2e")).unwrap();
    assert!(phprc.unwrap().join("php.ini").is_file(), "terminal PHP should get a php.ini");
    let first_php = path.split(';').find(|p| p.contains("php")).unwrap_or("").to_string();
    println!("terminal PATH php entry: {first_php}");
    assert!(first_php.replace('\\', "/").contains(&format!("/php/{other}")), "terminal uses wrong PHP: {first_php}");
    assert!(title.contains("e2e") && cwd.ends_with("e2e"));
    let php_exe = PathBuf::from(&first_php).join("php.exe");
    let out = Command::new(php_exe).arg("-r").arg("echo PHP_VERSION;").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), other);
    assert!(m::terminal_env(Some("missing-site")).is_err());

    // --- custom domain suffix (certificate is re-issued for the new name)
    assert!(m::set_tld(core.emit(), "dev").is_err(), "real TLD .dev must be rejected");
    assert!(m::set_tld(core.emit(), "1x").is_err(), "invalid suffix must be rejected");
    m::set_tld(core.emit(), ".loc").unwrap();
    core.apply_site_changes().unwrap();
    let body = curl(&["-H", "Host: e2e.loc", "http://127.0.0.1:8081/"]);
    assert!(body.contains("e2e.loc") || body.contains("is live"), "custom suffix not served: {body}");
    let body = curl(&["-k", "-H", "Host: e2e.loc", "https://127.0.0.1:8443/"]);
    assert!(body.contains("is live"), "https for custom suffix failed: {body}");

    // --- clean shutdown frees the ports
    core.stop_all();
    std::thread::sleep(Duration::from_millis(500));
    for p in [8081u16, 8443, 3307, 9100, 9101] {
        assert!(!port_open(p), "port {p} still open after stop_all");
    }
}
