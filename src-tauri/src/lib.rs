//! Tauri shell: all real work happens in `mwb-core`; this file only exposes
//! it to the settings window and manages the tray icon.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use mwb_core::store::{self, Cell, Settings};
use mwb_core::{AppError, SHARED_PIN};
use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WindowEvent};

const LOG_LINES: usize = 50;

/// Lets a second start of the program ask the running copy to show its window.
static APP: OnceLock<AppHandle> = OnceLock::new();

enum Engine {
    #[cfg(windows)]
    Master(mwb_core::engine::MasterHandle),
    #[cfg(windows)]
    Secondary(mwb_core::engine::SecondaryHandle),
    Failed(String),
}

struct AppState {
    dir: PathBuf,
    settings: Mutex<Settings>,
    engine: Mutex<Engine>,
    log: Arc<Mutex<VecDeque<String>>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MachineDto {
    id: String,
    name: String,
    is_local: bool,
    connected: bool,
    col: u8,
    row: u8,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StateDto {
    role: String,
    machine_name: String,
    shared_pin: String,
    two_row: bool,
    share_clipboard: bool,
    transfer_files: bool,
    master_ip: String,
    start_with_windows: bool,
    machines: Vec<MachineDto>,
    connected_to: Option<String>,
    engine_error: Option<String>,
    /// Something the user should fix (shown in the status line).
    warning: Option<String>,
    /// This PC's network addresses (to type in on a secondary if ever needed).
    addresses: Vec<String>,
    log: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CellDto {
    id: String,
    col: u8,
    row: u8,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApplyRequest {
    role: String,
    share_clipboard: bool,
    transfer_files: bool,
    master_ip: String,
    start_with_windows: bool,
    two_row: bool,
    cells: Vec<CellDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    name: String,
    version: String,
    developers: String,
    organisation: String,
    website: String,
    email: String,
    license: String,
}

#[cfg(windows)]
fn start_engine(
    dir: &std::path::Path,
    settings: &Settings,
    log: &Arc<Mutex<VecDeque<String>>>,
) -> Engine {
    use mwb_core::engine::{start_master, start_secondary, Log};
    let sink = log.clone();
    let logger: Log = Arc::new(move |m| {
        let mut q = sink.lock().unwrap();
        if q.len() >= LOG_LINES {
            q.pop_front();
        }
        q.push_back(m);
    });
    let started = if settings.role == "secondary" {
        start_secondary(dir.to_path_buf(), settings.clone(), logger).map(Engine::Secondary)
    } else {
        start_master(dir.to_path_buf(), settings.clone(), logger).map(Engine::Master)
    };
    started.unwrap_or_else(|e| Engine::Failed(e.to_string()))
}

#[cfg(not(windows))]
fn start_engine(_: &std::path::Path, _: &Settings, _: &Arc<Mutex<VecDeque<String>>>) -> Engine {
    Engine::Failed("This version runs on Windows only.".into())
}

#[tauri::command]
fn get_state(state: State<AppState>) -> StateDto {
    let s = state.settings.lock().unwrap().clone();
    let engine = state.engine.lock().unwrap();
    let mut machines = Vec::new();
    let mut connected_to = None;
    let mut engine_error = None;
    let mut warning = None;
    match &*engine {
        #[cfg(windows)]
        Engine::Master(h) => {
            machines = h
                .machines()
                .into_iter()
                .map(|m| MachineDto {
                    id: m.id,
                    name: m.name,
                    is_local: m.is_local,
                    connected: m.connected,
                    col: m.col,
                    row: m.row,
                })
                .collect();
            warning = h.warning();
        }
        #[cfg(windows)]
        Engine::Secondary(h) => {
            connected_to = h.connected_to();
            warning = h.warning();
        }
        Engine::Failed(e) => engine_error = Some(e.clone()),
    }
    let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "localhost".into());
    let addresses = mwb_core::discovery::local_ipv4s(&host)
        .iter()
        .map(|ip| ip.to_string())
        .collect();
    let log = state
        .log
        .lock()
        .unwrap()
        .iter()
        .rev()
        .take(8)
        .cloned()
        .collect();
    StateDto {
        role: s.role,
        machine_name: s.machine_name,
        shared_pin: SHARED_PIN.to_string(),
        two_row: s.two_row,
        share_clipboard: s.share_clipboard,
        transfer_files: s.transfer_files,
        master_ip: s.master_ip,
        start_with_windows: s.start_with_windows,
        machines,
        connected_to,
        engine_error,
        warning,
        addresses,
        log,
    }
}

/// Saves the settings. Returns true if the app is restarting (role or address changed).
#[tauri::command]
fn apply_settings(
    app: AppHandle,
    state: State<AppState>,
    req: ApplyRequest,
) -> Result<bool, AppError> {
    if req.role != "master" && req.role != "secondary" {
        return Err(AppError::Store("unknown role".into()));
    }
    let master_ip = req.master_ip.trim().to_string();
    if !master_ip.is_empty() && master_ip.parse::<std::net::IpAddr>().is_err() {
        return Err(AppError::Io(format!(
            "\"{master_ip}\" is not a valid address (example: 192.168.1.10)"
        )));
    }
    let mut s = state.settings.lock().unwrap();
    let restart = s.role != req.role || s.master_ip != master_ip;
    let autostart_changed = s.start_with_windows != req.start_with_windows;
    s.role = req.role;
    s.master_ip = master_ip;
    s.share_clipboard = req.share_clipboard;
    s.transfer_files = req.transfer_files;
    s.start_with_windows = req.start_with_windows;
    s.two_row = req.two_row;
    // The running engine keeps its own copy of what it has learnt
    // (pairings, arrangement, last master); read that back before saving.
    if let Ok(Some(on_disk)) = store::load(&state.dir) {
        s.peers = on_disk.peers;
        s.cells = on_disk.cells;
        s.last_master_addr = on_disk.last_master_addr;
        s.last_master_id = on_disk.last_master_id;
    }
    store::save(&state.dir, &s)?;
    if autostart_changed {
        set_autostart(s.start_with_windows)?;
    }
    if restart {
        drop(s);
        restart_app(&app);
        return Ok(true);
    }
    drop(s);
    let engine = state.engine.lock().unwrap();
    match &*engine {
        #[cfg(windows)]
        Engine::Master(h) => {
            h.options().set(req.share_clipboard, req.transfer_files);
            let cells = req
                .cells
                .into_iter()
                .map(|c| Cell {
                    id: c.id,
                    col: c.col,
                    row: c.row,
                })
                .collect();
            h.set_cells(cells, req.two_row)?;
        }
        #[cfg(windows)]
        Engine::Secondary(h) => h.options().set(req.share_clipboard, req.transfer_files),
        Engine::Failed(_) => {}
    }
    Ok(false)
}

#[cfg(windows)]
fn set_autostart(enable: bool) -> Result<(), AppError> {
    mwb_core::platform::set_autostart(enable)
}

#[cfg(not(windows))]
fn set_autostart(_: bool) -> Result<(), AppError> {
    Ok(())
}

/// Start a fresh copy and close this one. The new copy waits until this one
/// has gone, so the two never fight over the network ports.
fn restart_app(app: &AppHandle) {
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe)
            .arg(mwb_core::platform::ARG_RESTARTED)
            .spawn();
    }
    app.exit(0);
}

/// Opens the Windows administrator prompt that adds the firewall permission.
#[tauri::command]
fn allow_firewall() -> Result<(), AppError> {
    #[cfg(windows)]
    {
        mwb_core::platform::request_firewall_rule()
    }
    #[cfg(not(windows))]
    {
        Err(AppError::Io("Windows only".into()))
    }
}

#[tauri::command]
fn get_app_info(app: AppHandle) -> AppInfo {
    AppInfo {
        name: "Mouse without borders".into(),
        version: app.package_info().version.to_string(),
        developers: "Prof. Dattaraj S. Vidyasagar & Prof. Yash D. Vidyasagar".into(),
        organisation: "Vidyasagar Academy".into(),
        website: "https://vsa.edu.in/software/".into(),
        email: "softinquiry@vsa.edu.in".into(),
        license: "GNU General Public License v3.0".into(),
    }
}

#[tauri::command]
fn hide_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    let autostart = {
        use mwb_core::platform::{self, ARG_AUTOSTART, ARG_FIREWALL, ARG_RESTARTED};
        if args.iter().any(|a| a == ARG_FIREWALL) {
            // Short-lived administrator copy: add the firewall rule and leave.
            let _ = platform::add_firewall_rule();
            return;
        }
        let autostart = args.iter().any(|a| a == ARG_AUTOSTART);
        let restarted = args.iter().any(|a| a == ARG_RESTARTED);
        let first = platform::single_instance(autostart, restarted, || {
            if let Some(app) = APP.get() {
                show_main(app);
            }
        });
        if !first {
            return; // already running: that copy now shows its window
        }
        autostart
    };
    #[cfg(not(windows))]
    let autostart = args.iter().any(|a| a == "--autostart");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            let _ = APP.set(app.handle().clone());
            let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
            let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into());
            let settings = match store::load(&dir) {
                Ok(Some(s)) => s,
                _ => {
                    let s = Settings::new(&name);
                    let _ = store::save(&dir, &s);
                    s
                }
            };
            let log = Arc::new(Mutex::new(VecDeque::new()));
            // Keep the Windows start-up entry pointing at this copy of the program.
            // (The installer switches it on; this follows the user's choice.)
            let _ = set_autostart(settings.start_with_windows);
            let engine = start_engine(&dir, &settings, &log);
            // Started by Windows at sign-in: stay quietly in the tray,
            // unless something is wrong and the user must see it.
            let show_now = !autostart || matches!(engine, Engine::Failed(_));
            app.manage(AppState {
                dir,
                settings: Mutex::new(settings),
                engine: Mutex::new(engine),
                log,
            });

            let show = MenuItem::with_id(app, "show", "Show settings", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Exit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let mut tray = TrayIconBuilder::new()
                .tooltip("Mouse without borders")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "show" => show_main(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = ev
                    {
                        show_main(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            // If the tray icon cannot be made, the window is the only way in.
            let tray_ok = tray.build(app).is_ok();
            if show_now || !tray_ok {
                show_main(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps the app running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            apply_settings,
            get_app_info,
            hide_window,
            allow_firewall
        ])
        .run(tauri::generate_context!())
        .expect("error while running the application");
}
