pub mod dns;
pub mod manager;
pub mod projects;
pub mod redis;
pub mod tools;
pub mod workers;

use manager::{CatalogEntry, Core, Emit, Overview};
use std::sync::{Arc, OnceLock};
use tauri_plugin_updater::UpdaterExt;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, RunEvent, State, WindowEvent,
};

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_overview(core: State<'_, Core>) -> Overview {
    core.overview()
}

#[derive(serde::Serialize)]
struct CatalogInfo {
    entries: Vec<CatalogEntry>,
    checked_at: Option<u64>,
}

#[tauri::command]
fn get_catalog() -> CatalogInfo {
    CatalogInfo { entries: manager::catalog(), checked_at: manager::catalog_last_checked() }
}

#[tauri::command]
async fn refresh_catalog(core: State<'_, Core>) -> Result<usize, String> {
    let core = core.inner().clone();
    blocking(move || manager::refresh_catalog(core.emit())).await
}

#[tauri::command]
async fn start_service(core: State<'_, Core>, id: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.start(&id)).await
}

#[tauri::command]
async fn stop_service(core: State<'_, Core>, id: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.stop(&id)).await
}

#[tauri::command]
async fn restart_service(core: State<'_, Core>, id: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.restart(&id)).await
}

#[tauri::command]
async fn start_all(core: State<'_, Core>) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        core.start_all();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn restart_all(core: State<'_, Core>) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        core.restart_running();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn stop_all(core: State<'_, Core>) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        core.stop_all();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn set_active_version(core: State<'_, Core>, id: String, version: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        manager::set_active(&id, &version)?;
        if core.is_running(&id) {
            core.restart(&id)?;
            if id == "php" {
                core.restart_web()?;
            }
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn set_port(core: State<'_, Core>, id: String, port: u16) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        manager::set_port(&id, port)?;
        // The web servers share one HTTP port, so whichever one is running is restarted.
        match id.as_str() {
            "https" | "nginx" | "apache" => core.restart_web()?,
            "smtp" => {
                if core.is_running("mailpit") {
                    core.restart("mailpit")?;
                }
            }
            other => {
                if core.is_running(other) {
                    core.restart(other)?;
                }
            }
        }
        if id == "php" {
            core.restart_web()?;
        }
        if id == "smtp" {
            core.reload_php()?; // PHP's mail() setting changed
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn install_version(core: State<'_, Core>, id: String, version: String, url: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.install(&id, &version, &url)).await
}

#[tauri::command]
async fn install_recommended(core: State<'_, Core>) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.install_recommended()).await
}

#[tauri::command]
async fn install_composer(core: State<'_, Core>) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.install_composer()).await
}

#[tauri::command]
async fn install_adminer(core: State<'_, Core>) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.install_adminer()).await
}

#[tauri::command]
async fn remove_version(core: State<'_, Core>, id: String, version: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.remove_version(&id, &version)).await
}

#[tauri::command]
async fn create_site(core: State<'_, Core>, name: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        manager::create_site(&name)?;
        core.apply_site_changes()
    })
    .await
}

#[tauri::command]
async fn set_site_config(core: State<'_, Core>, name: String, php: String, ssl: bool) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        manager::set_site_config(core.emit(), &name, &php, ssl)?;
        core.apply_site_changes()
    })
    .await
}

#[tauri::command]
async fn set_tld(core: State<'_, Core>, tld: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        manager::set_tld(core.emit(), &tld)?;
        // The built-in DNS server must answer for the new suffix.
        if let Err(e) = core.dns_start_if_enabled() {
            manager::log(core.emit(), "dns", "err", e);
        }
        core.apply_site_changes()
    })
    .await
}

#[tauri::command]
async fn setup_ssl(core: State<'_, Core>) -> Result<String, String> {
    let core = core.inner().clone();
    blocking(move || manager::setup_ssl(core.emit())).await
}

#[tauri::command]
async fn sync_hosts() -> Result<String, String> {
    blocking(manager::sync_hosts).await
}

// ---- Updates (JOJO Server itself)

/// Where new versions are published (the `latest.json` file made by scripts/build-release.ps1).
/// Empty means "not decided yet": the app then says that updates are not set up.
/// Set at build time with the JOJO_UPDATE_URL environment variable (see scripts/build-release.ps1).
const UPDATE_URL: &str = match option_env!("JOJO_UPDATE_URL") {
    Some(u) => u,
    None => "",
};

#[derive(serde::Serialize, Clone)]
struct UpdateInfo {
    version: String,
    current: String,
    notes: Option<String>,
    date: Option<String>,
}

fn build_updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let url = std::env::var("JOJO_UPDATE_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| UPDATE_URL.to_string());
    if url.is_empty() {
        return Err("Updates are not set up yet: this build has no update address.".into());
    }
    let parsed: url::Url = url.parse().map_err(|e| format!("The update address is not valid: {e}"))?;
    app.updater_builder()
        .endpoints(vec![parsed])
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())
}

async fn check_update_inner(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater = build_updater(app)?;
    let found = updater.check().await.map_err(|e| format!("Could not check for updates: {e}"))?;
    Ok(found.map(|u| UpdateInfo {
        version: u.version.clone(),
        current: u.current_version.clone(),
        notes: u.body.clone(),
        date: u.date.map(|d| d.to_string()),
    }))
}

#[tauri::command]
async fn check_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    check_update_inner(&app).await
}

/// Download the new version, stop every service cleanly, run the installer and start the new app.
#[tauri::command]
async fn install_update(app: AppHandle, core: State<'_, Core>) -> Result<(), String> {
    let updater = build_updater(&app)?;
    let update = updater
        .check()
        .await
        .map_err(|e| format!("Could not check for updates: {e}"))?
        .ok_or("You already have the newest version.")?;
    let handle = app.clone();
    let mut received: u64 = 0;
    let bytes = update
        .download(
            move |chunk, total| {
                received += chunk as u64;
                let _ = handle.emit("update_progress", serde_json::json!({ "received": received, "total": total }));
            },
            || {},
        )
        .await
        .map_err(|e| format!("The download failed: {e}"))?;
    // Services must not be running while the program files are replaced.
    let core = core.inner().clone();
    blocking(move || {
        core.stop_all();
        Ok(())
    })
    .await?;
    update.install(bytes).map_err(|e| format!("The installation failed: {e}"))?;
    app.restart()
}

#[tauri::command]
fn app_update_configured() -> bool {
    !UPDATE_URL.is_empty() || std::env::var("JOJO_UPDATE_URL").map(|u| !u.is_empty()).unwrap_or(false)
}

// ---- New project, tools, workers, sharing, wildcard domains

#[tauri::command]
async fn create_project(core: State<'_, Core>, project: projects::NewProject) -> Result<String, String> {
    let core = core.inner().clone();
    blocking(move || {
        let url = core.create_project(project)?;
        core.apply_site_changes()?;
        Ok(url)
    })
    .await
}

#[tauri::command]
async fn install_node_lts(core: State<'_, Core>) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.install_node_lts()).await
}

#[tauri::command]
async fn install_dbgate(core: State<'_, Core>) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.install_dbgate()).await
}

#[tauri::command]
async fn launch_dbgate() -> Result<(), String> {
    blocking(tools::launch_dbgate).await
}

#[tauri::command]
async fn share_site(core: State<'_, Core>, site: String) -> Result<String, String> {
    let core = core.inner().clone();
    blocking(move || core.share_start(&site)).await
}

#[tauri::command]
async fn unshare_site(core: State<'_, Core>, site: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        core.share_stop(&site);
        Ok(())
    })
    .await
}

#[tauri::command]
async fn add_worker(core: State<'_, Core>, site: String, name: String, command: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.add_worker(&site, &name, &command)).await
}

#[tauri::command]
async fn remove_worker(core: State<'_, Core>, site: String, name: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.remove_worker(&site, &name)).await
}

#[tauri::command]
async fn start_worker(core: State<'_, Core>, site: String, name: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.start_worker(&site, &name)).await
}

#[tauri::command]
async fn stop_worker(core: State<'_, Core>, site: String, name: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.stop_worker(&site, &name)).await
}

#[tauri::command]
async fn set_worker_autostart(core: State<'_, Core>, site: String, name: String, on: bool) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.set_worker_autostart(&site, &name, on)).await
}

#[tauri::command]
async fn enable_wildcard_dns(core: State<'_, Core>) -> Result<String, String> {
    let core = core.inner().clone();
    blocking(move || core.enable_wildcard_dns()).await
}

#[tauri::command]
async fn disable_wildcard_dns(core: State<'_, Core>) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.disable_wildcard_dns()).await
}

#[tauri::command]
async fn check_wildcard_dns() -> Result<bool, String> {
    blocking(|| Ok(dns::resolves_locally(&manager::tld_of(&manager::load_settings())))).await
}

// ---- Mail and logs

#[tauri::command]
async fn send_test_mail() -> Result<String, String> {
    blocking(manager::send_test_mail).await
}

#[tauri::command]
fn get_log_sources() -> Vec<manager::LogSource> {
    manager::log_sources()
}

#[tauri::command]
async fn read_log(source: String, lines: usize) -> Result<manager::LogText, String> {
    blocking(move || manager::read_log(&source, lines)).await
}

#[tauri::command]
async fn clear_log(source: String) -> Result<(), String> {
    blocking(move || manager::clear_log(&source)).await
}

// ---- PHP settings and extensions

#[derive(serde::Serialize)]
struct ExtOption {
    name: &'static str,
    description: &'static str,
}

#[tauri::command]
fn get_php_extensions(version: String) -> Result<Vec<manager::ExtInfo>, String> {
    manager::php_extensions(&version)
}

#[tauri::command]
fn get_extension_catalog() -> Vec<ExtOption> {
    manager::EXT_CATALOG
        .iter()
        .map(|e| ExtOption { name: e.name, description: e.description })
        .collect()
}

#[tauri::command]
async fn set_php_extension(core: State<'_, Core>, version: String, name: String, enabled: bool) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        manager::set_php_extension(&version, &name, enabled)?;
        core.reload_php()
    })
    .await
}

#[tauri::command]
async fn install_php_extension(core: State<'_, Core>, version: String, name: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        manager::install_php_extension(core.emit(), &version, &name)?;
        core.reload_php()
    })
    .await
}

#[tauri::command]
async fn remove_php_extension(core: State<'_, Core>, version: String, name: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        core.stop("php")?;
        manager::remove_php_extension(&version, &name)?;
        core.start("php")
    })
    .await
}

#[tauri::command]
async fn set_web_server(core: State<'_, Core>, id: String) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || core.set_web_server(&id)).await
}

#[tauri::command]
async fn set_php_conf(core: State<'_, Core>, conf: manager::PhpConf) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        manager::set_php_conf(conf)?;
        core.reload_php()
    })
    .await
}

#[tauri::command]
async fn set_www_dir(core: State<'_, Core>, dir: String, move_existing: bool) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || manager::set_www_dir(&core, &dir, move_existing)).await
}

#[tauri::command]
async fn pick_folder(initial: String) -> Result<Option<String>, String> {
    blocking(move || manager::pick_folder(&initial)).await
}

#[tauri::command]
async fn set_general(general: manager::General) -> Result<(), String> {
    blocking(move || manager::set_general(general)).await
}

// ---- Redis browser (each call runs on a worker thread so the window never freezes)

#[tauri::command]
async fn redis_keys(db: u8, pattern: String, cursor: String, limit: usize) -> Result<redis::KeyPage, String> {
    blocking(move || redis::keys(db, &pattern, &cursor, limit)).await
}

#[tauri::command]
async fn redis_get(db: u8, key: String) -> Result<redis::KeyDetail, String> {
    blocking(move || redis::get(db, &key)).await
}

#[tauri::command]
async fn redis_set_string(db: u8, key: String, value: String, ttl: Option<u64>) -> Result<(), String> {
    blocking(move || redis::set_string(db, &key, &value, ttl)).await
}

#[tauri::command]
async fn redis_delete(db: u8, key: String) -> Result<(), String> {
    blocking(move || redis::delete(db, &key)).await
}

#[tauri::command]
async fn redis_expire(db: u8, key: String, seconds: Option<u64>) -> Result<(), String> {
    blocking(move || redis::expire(db, &key, seconds)).await
}

#[tauri::command]
async fn redis_flush(db: u8) -> Result<(), String> {
    blocking(move || redis::flush_db(db)).await
}

#[tauri::command]
async fn redis_exec(db: u8, line: String) -> Result<redis::ExecResult, String> {
    blocking(move || redis::exec(db, &line)).await
}

#[tauri::command]
async fn redis_info(db: u8) -> Result<redis::InfoOut, String> {
    blocking(move || redis::info(db)).await
}

#[tauri::command]
fn open_path(kind: String) -> Result<(), String> {
    manager::open_path(&kind)
}

#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    manager::open_url(&url)
}

#[tauri::command]
async fn open_terminal(core: State<'_, Core>, site: Option<String>) -> Result<(), String> {
    let core = core.inner().clone();
    blocking(move || {
        // A terminal without `composer` is useless for PHP work: fetch it first if it is missing.
        core.ensure_composer();
        manager::open_terminal(site.as_deref())
    })
    .await
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

pub fn run() {
    manager::ensure_dirs();

    // The Core is created before the window exists, so events go through a cell that is filled in setup().
    let cell: Arc<OnceLock<AppHandle>> = Arc::new(OnceLock::new());
    let cell_for_emit = cell.clone();
    let emit: Emit = Arc::new(move |name, payload| {
        if let Some(h) = cell_for_emit.get() {
            let _ = h.emit(name, payload);
        }
    });

    let app = tauri::Builder::default()
        // Only one JOJO Server may run: a second launch just brings the first window to the front.
        // (Two copies would fight over the same services and leave old ones running.)
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(Core::new(emit))
        .invoke_handler(tauri::generate_handler![
            get_overview,
            get_catalog,
            refresh_catalog,
            start_service,
            stop_service,
            restart_service,
            start_all,
            restart_all,
            stop_all,
            set_active_version,
            set_port,
            install_version,
            install_recommended,
            install_composer,
            install_adminer,
            remove_version,
            create_site,
            set_site_config,
            set_tld,
            setup_ssl,
            sync_hosts,
            check_update,
            install_update,
            app_update_configured,
            create_project,
            install_node_lts,
            install_dbgate,
            launch_dbgate,
            share_site,
            unshare_site,
            add_worker,
            remove_worker,
            start_worker,
            stop_worker,
            set_worker_autostart,
            enable_wildcard_dns,
            disable_wildcard_dns,
            check_wildcard_dns,
            send_test_mail,
            get_log_sources,
            read_log,
            clear_log,
            get_php_extensions,
            get_extension_catalog,
            set_php_extension,
            install_php_extension,
            remove_php_extension,
            set_php_conf,
            set_web_server,
            set_www_dir,
            pick_folder,
            set_general,
            redis_keys,
            redis_get,
            redis_set_string,
            redis_delete,
            redis_expire,
            redis_flush,
            redis_exec,
            redis_info,
            open_path,
            open_url,
            open_terminal,
        ])
        .setup(move |app| {
            let _ = cell.set(app.handle().clone());
            // Started from Windows login: stay in the tray.
            if std::env::args().any(|a| a == "--minimized") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            // Start the services the user chose to run whenever JOJO Server opens
            // (after clearing programs a crashed or force-closed session left behind).
            let auto = manager::load_settings().general.autostart;
            let app_handle_for_update = app.handle().clone();
            {
                let core = app.state::<Core>().inner().clone();
                std::thread::spawn(move || {
                    manager::reap_orphans(core.emit());
                    core.ensure_composer();
                    if let Err(e) = core.dns_start_if_enabled() {
                        manager::log(core.emit(), "dns", "err", e);
                    }
                    for d in manager::DEFS.iter() {
                        if auto.iter().any(|a| a == d.id) && !manager::versions(d.id).is_empty() {
                            if let Err(e) = core.start(d.id) {
                                manager::log(core.emit(), d.id, "err", e);
                            }
                        }
                    }
                    core.start_autostart_workers();
                    // Look for a new JOJO Server version a few seconds after start-up.
                    if manager::load_settings().general.check_updates {
                        std::thread::sleep(std::time::Duration::from_secs(6));
                        let handle = app_handle_for_update.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Ok(Some(info)) = check_update_inner(&handle).await {
                                let _ = handle.emit("update_available", info);
                            }
                        });
                    }
                });
            }
            // Refresh the list of downloadable versions in the background (at most once a day).
            if manager::catalog_cache_stale() {
                let emit = app.state::<Core>().emit().clone();
                std::thread::spawn(move || {
                    let _ = manager::refresh_catalog(&emit);
                });
            }
            let show = MenuItem::with_id(app, "show", "Open JOJO Server", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit (stop all services)", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("JOJO Server")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main(app),
                    "quit" => {
                        let core = app.state::<Core>().inner().clone();
                        let handle = app.clone();
                        std::thread::spawn(move || {
                            core.stop_all();
                            handle.exit(0);
                        });
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if manager::load_settings().general.close_to_tray {
                    // Hide to the tray; services keep running.
                    let _ = window.hide();
                } else {
                    // Quit: stop the services first.
                    let core = window.state::<Core>().inner().clone();
                    let handle = window.app_handle().clone();
                    std::thread::spawn(move || {
                        core.stop_all();
                        handle.exit(0);
                    });
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building JOJO Server");

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            app.state::<Core>().stop_all();
        }
    });
}
