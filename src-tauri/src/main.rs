// No console window on Windows release builds; harmless elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod asking;
mod capture;
mod commands;
mod detection;
mod embedding;
mod jobs;
mod kwin;
mod notes;
mod recorder;
mod screenshot_protocol;
mod screenshots;
mod search;
mod settings;
mod single_instance;
mod streams;
mod tray;
mod updates;
mod window_state;

use std::sync::{Arc, Mutex};

use tiro_core::acks::Acks;
use tiro_core::spend::SpendLedger;
use tiro_core::store::MeetingStore;
use tauri::{Emitter, Manager, WindowEvent};

use recorder::AppState;

/// Something was added to the spend ledger; carries the Meeting's id, or null. Matches `EVENTS.spend` in src/lib/api.ts.
const SPEND_EVENT: &str = "spend";

fn main() {
    // WebKitGTK's DMA-BUF renderer kills the app with "Error 71 (Protocol error) dispatching to Wayland
    // display" on NVIDIA under Wayland. Set before GTK starts and before any other thread exists.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
    // GTK reports this as the Wayland app id, which KDE matches against the installed Tiro.desktop for the
    // window's title bar and taskbar icons. It defaults to the binary name, "tiro", which matches nothing.
    glib::set_prgname(Some("Tiro"));

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // Before touching the Meeting store: a second copy would mark the running copy's recording interrupted.
    let Some(_instance_lock) = single_instance::acquire() else {
        single_instance::report_already_running();
        return;
    };

    let data = settings::data_dir();
    let store = Arc::new(MeetingStore::open(&data.join("meetings"), &data.join("index.sqlite")).expect("open the Meeting store"));
    // The index is only a cache of the folders (ADR-0005), so refresh it on every launch.
    if let Err(e) = store.rebuild_index() {
        tracing::warn!("could not rebuild the Meeting index: {e}");
    }
    // Only this process records, so anything still marked recording or transcribing was cut short.
    if let Err(e) = store.mark_interrupted() {
        tracing::warn!("could not mark interrupted Meetings: {e}");
    }
    // Thin Screenshots kept before ticket 26, once. Only Meetings that exist now: capture marks the ones it starts.
    match store.list() {
        Ok(meetings) => screenshots::thin_in_background(meetings),
        Err(e) => tracing::warn!("could not list Meetings to thin their screenshots: {e}"),
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol(screenshot_protocol::SCHEME, {
            let store = store.clone();
            move |_ctx, request, responder| {
                let store = store.clone();
                std::thread::spawn(move || responder.respond(screenshot_protocol::serve(&store, &request)));
            }
        })
        .manage(window_state::Tracked::default())
        .manage(updates::Updates::default())
        .manage(AppState {
            store,
            spend: Arc::new(SpendLedger::new(data.join("spend.jsonl"))),
            active: Mutex::new(None),
            jobs: Mutex::default(),
            acks: Acks::default(),
            indexing: Mutex::default(),
            meaning: Arc::default(),
            embedding: Default::default(),
            conversation: Default::default(),
        })
        .setup(|app| {
            let handle = app.handle().clone();
            app.state::<AppState>().spend.on_record(move |entry| {
                let _ = handle.emit(SPEND_EVENT, &entry.meeting_id);
            });
            tray::build(app.handle())?;
            if let Some(window) = app.get_webview_window("main") {
                window.set_title(&format!("Tiro v{}", env!("CARGO_PKG_VERSION")))?;
                window_state::restore(&window);
            }
            // The window starts hidden (tauri.conf.json); the login autostart entry passes --hidden to stay in the tray.
            if !std::env::args().any(|arg| arg == "--hidden") {
                tray::show_window(app.handle(), None);
            }
            detection::spawn(app.handle().clone());
            jobs::tag_in_background(app.handle().clone());
            // Every Meeting indexed from now on is embedded again; only its changed Passages cost anything.
            let handle = app.handle().clone();
            app.state::<AppState>().store.on_indexed(move |id| embedding::enqueue(&handle, [id.to_owned()]));
            embedding::spawn_worker(app.handle().clone());
            search::index_in_background(app.handle().clone());
            updates::spawn(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            window_state::track(window, event);
            // Closing the window hides it to the tray; Quit lives in the tray menu.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                window_state::save(window.app_handle());
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::start_recording,
            commands::stop_recording,
            commands::active_meeting,
            commands::list_meetings,
            commands::transcript,
            commands::meeting_screenshots,
            commands::get_settings,
            commands::save_settings,
            commands::list_microphones,
            commands::save_summary_settings,
            commands::set_autostart,
            commands::set_meeting_screenshots,
            commands::list_fonts,
            commands::save_appearance,
            commands::meeting_summary,
            commands::summarize,
            commands::meeting_spend,
            commands::spend_summary,
            commands::retry_failed_chunks,
            commands::set_title,
            commands::delete_meeting,
            commands::export_default_path,
            commands::export_meeting,
            commands::notes,
            commands::save_notes,
            commands::notes_flushed,
            commands::search,
            commands::index_meeting,
            commands::indexing_progress,
            commands::embedding_progress,
            commands::ask_off,
            commands::ask,
            commands::new_conversation,
            commands::update_status,
            commands::set_check_for_updates,
            commands::install_update,
        ])
        .build(tauri::generate_context!())
        .expect("build the Tauri app")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                window_state::save(app);
                recorder::stop(app, &app.state::<AppState>());
            }
        });
}
