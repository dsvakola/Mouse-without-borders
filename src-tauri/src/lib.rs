//! Tauri shell: all real work happens in `mwb-core`; this file only exposes
//! it to the settings window and manages the tray icon.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use mwb_core::store::{self, Cell, Settings};
use mwb_core::{AppError, SHARED_PIN};
use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WindowEvent};

const LOG_LINES: usize = 50;

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
    machines: Vec<MachineDto>,
    connected_to: Option<String>,
    engine_error: Option<String>,
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
        }
        #[cfg(windows)]
        Engine::Secondary(h) => connected_to = h.connected_to(),
        Engine::Failed(e) => engine_error = Some(e.clone()),
    }
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
        machines,
        connected_to,
        engine_error,
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
    let mut s = state.settings.lock().unwrap();
    let restart = s.role != req.role || s.master_ip.trim() != req.master_ip.trim();
    s.role = req.role;
    s.master_ip = req.master_ip.trim().to_string();
    s.share_clipboard = req.share_clipboard;
    s.transfer_files = req.transfer_files;
    s.two_row = req.two_row;
    store::save(&state.dir, &s)?;
    if restart {
        drop(s);
        app.restart();
    }
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
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
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
            let engine = start_engine(&dir, &settings, &log);
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
            tray.build(app)?;
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
            hide_window
        ])
        .run(tauri::generate_context!())
        .expect("error while running the application");
}
