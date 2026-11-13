//! The tray icon and its menu. Tested by hand.

use tiro_core::detection::MeetingApp;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::notes;
use crate::recorder::{self, AppState};

const TRAY_ID: &str = "main";
const IDLE_ICON: &[u8] = include_bytes!("../icons/tray-idle.png");
const RECORDING_ICON: &[u8] = include_bytes!("../icons/tray-recording.png");

pub const NAVIGATE_EVENT: &str = "navigate";

/// The menu item whose label flips between Start and Stop.
struct ToggleItem(MenuItem<Wry>);

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let toggle = MenuItem::with_id(app, "toggle", "Start recording", true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open window", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&toggle, &open, &settings, &separator, &quit])?;
    app.manage(ToggleItem(toggle));

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(IDLE_ICON)?)
        .tooltip("Tiro")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "toggle" => toggle_recording(app),
            "open" => show_window(app, None),
            "settings" => show_window(app, Some("settings")),
            "quit" => {
                recorder::stop(app, &app.state::<AppState>());
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    // Notes typed just before Quit may still be waiting on autosave.
                    notes::flush(&app).await;
                    app.exit(0);
                });
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn toggle_recording(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.active.lock().unwrap().is_some() {
        recorder::stop(app, &state);
        return;
    }
    start_recording(app, None);
}

/// Starts recording a Meeting, titled after the `detected` meeting app if any (see `recorder::start`), showing
/// the window with the error if that fails.
pub fn start_recording(app: &AppHandle, detected: Option<MeetingApp>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(message) = recorder::start(&app, &app.state::<AppState>(), detected) {
            tracing::error!("could not start recording: {message}");
            show_window(&app, None);
            let _ = app.emit(recorder::PIPELINE_EVENT, serde_json::json!({ "kind": "error", "meetingId": null, "message": message }));
        }
    });
}

/// Shows and focuses the main window, optionally switching it to a view.
pub fn show_window(app: &AppHandle, view: Option<&str>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    if let Some(view) = view {
        let _ = app.emit(NAVIGATE_EVENT, view);
    }
}

pub fn set_recording(app: &AppHandle, recording: bool) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let icon = if recording { RECORDING_ICON } else { IDLE_ICON };
        if let Ok(image) = Image::from_bytes(icon) {
            let _ = tray.set_icon(Some(image));
        }
        let _ = tray.set_tooltip(Some(if recording { "Tiro — recording" } else { "Tiro" }));
    }
    if let Some(item) = app.try_state::<ToggleItem>() {
        let _ = item.0.set_text(if recording { "Stop recording" } else { "Start recording" });
    }
}
