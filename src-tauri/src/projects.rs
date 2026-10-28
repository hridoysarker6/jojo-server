//! "New project": create a ready-to-run Laravel, WordPress or Symfony site (or a blank PHP site).

use crate::manager::{self, composer_installed, e2s, fwd, hide, load_settings, log, pipe_logs, root, sanitize, versions, www_dir, Core};
use serde::Deserialize;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Deserialize)]
pub struct NewProject {
    pub name: String,
    /// "blank", "laravel", "wordpress" or "symfony"
    pub kind: String,
    /// create a MySQL database and wire it into the project
    pub database: bool,
    /// run `npm install` afterwards (Laravel)
    pub npm: bool,
}

/// Set `KEY=value` in a .env file's text: replaces the line (even a commented-out one) or adds a new one.
pub fn set_env(text: &str, key: &str, value: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let prefix = format!("{key}=");
    let mut done = false;
    for l in lines.iter_mut() {
        let bare = l.trim_start().trim_start_matches('#').trim_start();
        if bare.starts_with(&prefix) {
            *l = format!("{key}={value}");
            done = true;
            break;
        }
    }
    if !done {
        lines.push(format!("{key}={value}"));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn db_name(site: &str) -> String {
    let n: String = site.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    if n.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true) { format!("db_{n}") } else { n }
}

fn mysql_client() -> Option<PathBuf> {
    let s = load_settings();
    let ver = manager::active_of(&s, "mysql")?;
    let base = manager::base_dir(&root().join("bin").join("mysql").join(ver), manager::def("mysql")?)?;
    let p = base.join("bin").join("mysql.exe");
    p.is_file().then_some(p)
}

impl Core {
    /// Run a command line the way a terminal would (default PHP, Node.js and Composer on PATH),
    /// copying its output to the Live log. Fails with a short message if it exits with an error.
    fn run_project_command(&self, cwd: &Path, line: &str) -> Result<(), String> {
        let (path, _title, _cwd, phprc) = manager::terminal_env(None)?;
        let mut c = Command::new("cmd");
        c.raw_arg(format!("/D /S /C \"{line}\""));
        c.current_dir(cwd).env("PATH", path).env("COMPOSER_NO_INTERACTION", "1");
        if let Some(p) = phprc {
            c.env("PHPRC", p);
        }
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        hide(&mut c);
        log(&self.emit, "project", "info", format!("> {line}"));
        let mut child = c.spawn().map_err(e2s)?;
        let mut readers = vec![];
        if let Some(o) = child.stdout.take() {
            readers.push(pipe_logs(self.emit.clone(), "project".into(), o));
        }
        if let Some(o) = child.stderr.take() {
            readers.push(pipe_logs(self.emit.clone(), "project".into(), o));
        }
        let status = child.wait().map_err(e2s)?;
        for r in readers {
            let _ = r.join();
        }
        if status.success() {
            Ok(())
        } else {
            Err(format!("The command failed ({status}): {line}. See the Live log for details."))
        }
    }

    /// Make sure MySQL is installed and answering, then create the database. Returns the database name.
    fn create_database(&self, site: &str) -> Result<(String, u16), String> {
        let client = mysql_client().ok_or("MySQL is not installed. Install it from the Versions tab, or untick 'Create a MySQL database'.")?;
        if !self.is_running("mysql") {
            log(&self.emit, "project", "info", "Starting MySQL...");
            self.start("mysql")?;
        }
        let port = manager::port_of(&load_settings(), "mysql");
        let db = db_name(site);
        let mut last = String::new();
        for _ in 0..30 {
            let mut c = Command::new(&client);
            c.arg("-u").arg("root").arg("-h").arg("127.0.0.1").arg(format!("-P{port}")).arg("-e").arg(format!(
                "CREATE DATABASE IF NOT EXISTS `{db}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"
            ));
            hide(&mut c);
            match c.output() {
                Ok(o) if o.status.success() => {
                    log(&self.emit, "project", "ok", format!("Database '{db}' is ready (MySQL port {port})"));
                    return Ok((db, port));
                }
                Ok(o) => last = String::from_utf8_lossy(&o.stderr).trim().to_string(),
                Err(e) => last = e.to_string(),
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Err(format!("Could not create the database: {last}"))
    }

    pub fn create_project(&self, p: NewProject) -> Result<String, String> {
        let name = sanitize(&p.name);
        if name.is_empty() {
            return Err("Invalid site name. Use letters, numbers and '-' only.".into());
        }
        let dir = www_dir().join(&name);
        if dir.exists() {
            return Err(format!("A folder named '{name}' already exists."));
        }
        let tld = manager::tld_of(&load_settings());
        let url = format!("http://{name}.{tld}");
        log(&self.emit, "project", "info", format!("Creating {} project '{name}'...", p.kind));

        match p.kind.as_str() {
            "blank" => manager::create_site(&p.name)?,
            "laravel" | "symfony" => {
                if versions("php").is_empty() {
                    return Err("PHP is not installed. Install it from the Versions tab first.".into());
                }
                self.ensure_composer();
                if !composer_installed() {
                    return Err("Composer could not be installed (is the internet connection working?).".into());
                }
                fs::create_dir_all(www_dir()).map_err(e2s)?;
                let package = if p.kind == "laravel" { "laravel/laravel" } else { "symfony/skeleton" };
                self.run_project_command(&www_dir(), &format!("composer create-project {package} \"{name}\" --no-interaction --no-ansi --no-progress"))?;

                if p.kind == "laravel" {
                    let env_path = dir.join(".env");
                    if !env_path.is_file() && dir.join(".env.example").is_file() {
                        fs::copy(dir.join(".env.example"), &env_path).map_err(e2s)?;
                    }
                    let smtp = manager::smtp_port_of(&load_settings());
                    let mut env = fs::read_to_string(&env_path).unwrap_or_default();
                    env = set_env(&env, "APP_URL", &url);
                    env = set_env(&env, "MAIL_MAILER", "smtp");
                    env = set_env(&env, "MAIL_HOST", "127.0.0.1");
                    env = set_env(&env, "MAIL_PORT", &smtp.to_string());
                    env = set_env(&env, "MAIL_USERNAME", "null");
                    env = set_env(&env, "MAIL_PASSWORD", "null");
                    env = set_env(&env, "MAIL_FROM_ADDRESS", "\"dev@jojo.test\"");
                    let mut migrate = false;
                    if p.database {
                        match self.create_database(&name) {
                            Ok((db, port)) => {
                                env = set_env(&env, "DB_CONNECTION", "mysql");
                                env = set_env(&env, "DB_HOST", "127.0.0.1");
                                env = set_env(&env, "DB_PORT", &port.to_string());
                                env = set_env(&env, "DB_DATABASE", &db);
                                env = set_env(&env, "DB_USERNAME", "root");
                                env = set_env(&env, "DB_PASSWORD", "");
                                migrate = true;
                            }
                            Err(e) => log(&self.emit, "project", "warn", format!("{e} The project keeps its default (SQLite) database.")),
                        }
                    }
                    fs::write(&env_path, env).map_err(e2s)?;
                    if migrate {
                        self.run_project_command(&dir, "php artisan migrate --force --no-interaction --no-ansi")?;
                    }
                }
                if p.npm && dir.join("package.json").is_file() {
                    self.install_node_lts()?;
                    self.run_project_command(&dir, "npm install --no-audit --no-fund --loglevel=error").map_err(|e| {
                        format!("{e} Tip: if the log mentions 'edgesOut', your Node.js is too old. Install the newest LTS on the Versions tab and make it the active one in Settings.")
                    })?;
                }
            }
            "wordpress" => {
                fs::create_dir_all(www_dir()).map_err(e2s)?;
                let zip = root().join("downloads").join("wordpress-latest.zip");
                fs::create_dir_all(root().join("downloads")).map_err(e2s)?;
                log(&self.emit, "project", "info", "Downloading WordPress...");
                manager::download_file(&self.emit, "wordpress", "latest", "https://wordpress.org/latest.zip", &zip)?;
                let tmp = www_dir().join(format!(".tmp-{name}"));
                let _ = fs::remove_dir_all(&tmp);
                fs::create_dir_all(&tmp).map_err(e2s)?;
                if let Err(e) = manager::extract_zip(&zip, &tmp) {
                    let _ = fs::remove_dir_all(&tmp);
                    return Err(format!("Extraction failed: {e}"));
                }
                fs::rename(&tmp, &dir).map_err(e2s)?;
                let _ = fs::remove_file(&zip);

                let mut cfg = fs::read_to_string(dir.join("wp-config-sample.php")).map_err(|_| "wp-config-sample.php is missing from the WordPress download.".to_string())?;
                if p.database {
                    match self.create_database(&name) {
                        Ok((db, port)) => {
                            cfg = cfg
                                .replace("database_name_here", &db)
                                .replace("username_here", "root")
                                .replace("password_here", "")
                                .replace("'localhost'", &format!("'127.0.0.1:{port}'"));
                        }
                        Err(e) => log(&self.emit, "project", "warn", format!("{e} You will be asked for database details when you open the site.")),
                    }
                }
                cfg = put_wp_salts(cfg);
                cfg = cfg.replace("/* That's all, stop editing!", "define( 'FS_METHOD', 'direct' );\n\n/* That's all, stop editing!");
                fs::write(dir.join("wp-config.php"), cfg).map_err(e2s)?;
            }
            other => return Err(format!("Unknown project type '{other}'.")),
        }
        log(&self.emit, "project", "ok", format!("Project '{name}' is ready: {url}"));
        let _ = fwd(&dir);
        Ok(url)
    }
}

/// Replace WordPress's placeholder keys with fresh ones from wordpress.org (kept as-is if offline).
fn put_wp_salts(cfg: String) -> String {
    let Ok(api) = manager::http_text("https://api.wordpress.org/secret-key/1.1/salt/") else { return cfg };
    let keys = ["AUTH_KEY", "SECURE_AUTH_KEY", "LOGGED_IN_KEY", "NONCE_KEY", "AUTH_SALT", "SECURE_AUTH_SALT", "LOGGED_IN_SALT", "NONCE_SALT"];
    let mut out = String::new();
    for line in cfg.lines() {
        let mut replaced = false;
        for k in keys {
            let a = format!("define( '{k}'");
            let b = format!("define('{k}'");
            if line.trim_start().starts_with(&a) || line.trim_start().starts_with(&b) {
                if let Some(new_line) = api.lines().find(|l| l.contains(&format!("'{k}'"))) {
                    out.push_str(new_line);
                    out.push('\n');
                    replaced = true;
                }
                break;
            }
        }
        if !replaced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}
