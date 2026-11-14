//! Tells the user about newer releases (ticket 38): checks GitHub at startup and about once a day, puts "Update to …"
//! in the tray menu, notifies once per version, and downloads and installs the RPM when asked. Nothing is shown and
//! nothing installs while a Meeting records. Tested by hand; the decisions are in `tiro_core::update`.

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use dbus::blocking::Connection;
use dbus::message::MatchRule;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tiro_core::update::{self, GitHubReleases, InstallOutcome, Update, Version, GITHUB_API};
use tokio::sync::Notify;

use crate::recorder::AppState;
use crate::{settings, tray};

/// The update status changed; the UI reloads it with `update_status`. Matches `EVENTS.update` in src/lib/api.ts.
pub const UPDATE_EVENT: &str = "update";

const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the loop wakes to see whether a check is due, so a suspended laptop still checks daily.
const TICK: Duration = Duration::from_secs(60 * 60);
/// How often to look again whether the Meeting holding back a notification or install prompt has stopped.
const RECORDING_POLL: Duration = Duration::from_secs(30);
const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";
const NOTIFY_ACTION: &str = "update";

#[derive(Default)]
pub struct Updates {
    state: Mutex<UpdateState>,
    /// Wakes the check loop, such as when checking is turned on.
    wake: Notify,
}

#[derive(Default)]
struct UpdateState {
    available: Option<Update>,
    /// The version installed while this copy runs; not offered again until Tiro restarts into it.
    installed: Option<Version>,
    /// Downloading or installing.
    busy: bool,
    /// The command to type, after the privilege prompt could not install the update.
    manual_command: Option<String>,
}

/// What Settings shows about updates.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    current: &'static str,
    available: Option<String>,
    busy: bool,
    manual_command: Option<String>,
}

fn current() -> Version {
    env!("CARGO_PKG_VERSION").parse().expect("the package version is major.minor.patch")
}

fn recording(app: &AppHandle) -> bool {
    app.state::<AppState>().active.lock().unwrap().is_some()
}

async fn until_not_recording(app: &AppHandle) {
    while recording(app) {
        tokio::time::sleep(RECORDING_POLL).await;
    }
}

pub fn status(app: &AppHandle) -> UpdateStatus {
    let state = app.state::<Updates>();
    let state = state.state.lock().unwrap();
    UpdateStatus {
        current: env!("CARGO_PKG_VERSION"),
        available: state.available.as_ref().map(|u| u.version.to_string()),
        busy: state.busy,
        manual_command: state.manual_command.clone(),
    }
}

/// Shows the update state in the tray menu and tells the UI it changed.
pub fn refresh_tray(app: &AppHandle) {
    let Some(updates) = app.try_state::<Updates>() else { return };
    let (label, enabled) = {
        let state = updates.state.lock().unwrap();
        match &state.available {
            None => (None, false),
            Some(u) if state.busy => (Some(format!("Updating to {}…", u.version)), false),
            Some(u) => (Some(format!("Update to {}…", u.version)), true),
        }
    };
    tray::show_update(app, label.as_deref(), enabled);
    let _ = app.emit(UPDATE_EVENT, ());
}

/// Checks at startup and then about once a day while checking is on, for as long as the app runs.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut last_check: Option<SystemTime> = None;
        loop {
            let due = last_check.is_none_or(|at| at.elapsed().map_or(true, |elapsed| elapsed >= CHECK_EVERY));
            if settings::load().check_for_updates && due {
                last_check = Some(SystemTime::now());
                check(&app).await;
            }
            let updates = app.state::<Updates>();
            tokio::select! {
                _ = tokio::time::sleep(TICK) => {}
                // Woken because checking was turned on: check now, not a day after the last check.
                _ = updates.wake.notified() => last_check = None,
            }
        }
    });
}

/// Turns checking on or off. Off forgets the available update, so nothing more is fetched from GitHub; on checks now.
pub fn set_enabled(app: &AppHandle, enabled: bool) -> std::io::Result<()> {
    settings::update(|s| s.check_for_updates = enabled)?;
    let updates = app.state::<Updates>();
    if enabled {
        updates.wake.notify_one();
    } else {
        let mut state = updates.state.lock().unwrap();
        if !state.busy {
            state.available = None;
            state.manual_command = None;
        }
    }
    refresh_tray(app);
    Ok(())
}

async fn check(app: &AppHandle) {
    let update = match update::check(&GitHubReleases::new(GITHUB_API.into()), current()).await {
        Ok(Some(update)) => update,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("update check failed; trying again tomorrow: {e}");
            return;
        }
    };
    let version = update.version;
    {
        let updates = app.state::<Updates>();
        let mut state = updates.state.lock().unwrap();
        // Checking may have been turned off while the request was out.
        if !settings::load().check_for_updates || state.busy || state.installed.is_some_and(|installed| installed >= version) {
            return;
        }
        if state.available.as_ref().is_some_and(|u| u.version != version) {
            state.manual_command = None;
        }
        state.available = Some(update);
    }
    refresh_tray(app);
    tracing::info!("Tiro {version} is available");

    if !update::should_announce(version, settings::load().announced_version) {
        return;
    }
    until_not_recording(app).await;
    if !settings::load().check_for_updates {
        return;
    }
    let app = app.clone();
    std::thread::Builder::new()
        .name("update-notification".into())
        .spawn(move || notify(&app, version))
        .expect("spawn the update notification");
}

/// Shows the notification for `version`, remembers it was announced, and starts the update if its action is chosen.
fn notify(app: &AppHandle, version: Version) {
    let shown = Connection::new_session().map_err(|e| e.to_string()).and_then(|conn| {
        let proxy = conn.with_proxy(NOTIFICATIONS, NOTIFICATIONS_PATH, Duration::from_secs(2));
        let (id,): (u32,) = proxy
            .method_call(
                NOTIFICATIONS,
                "Notify",
                (
                    "Tiro",
                    0u32,
                    "system-software-update",
                    format!("Tiro {version} is available"),
                    format!("You have {}. Choose Update to download and install it.", env!("CARGO_PKG_VERSION")),
                    vec![NOTIFY_ACTION, "Update…"],
                    dbus::arg::PropMap::new(),
                    -1i32,
                ),
            )
            .map_err(|e| e.to_string())?;
        Ok((conn, id))
    });
    let (conn, id) = match shown {
        Ok(shown) => shown,
        Err(e) => {
            tracing::warn!("could not show the update notification: {e}");
            return;
        }
    };
    if let Err(e) = settings::update(|s| s.announced_version = Some(version)) {
        tracing::warn!("could not remember that Tiro {version} was announced: {e}");
    }

    // Listen until the notification is answered or closed; KDE may keep it in its history for a while.
    let (tx, rx) = std::sync::mpsc::channel();
    let invoked = tx.clone();
    let matched = conn
        .add_match(MatchRule::new_signal(NOTIFICATIONS, "ActionInvoked"), move |(shown, action): (u32, String), _, _| {
            if shown == id {
                let _ = invoked.send(action == NOTIFY_ACTION);
            }
            true
        })
        .and_then(|_| {
            conn.add_match(MatchRule::new_signal(NOTIFICATIONS, "NotificationClosed"), move |(shown, _): (u32, u32), _, _| {
                if shown == id {
                    let _ = tx.send(false);
                }
                true
            })
        });
    if let Err(e) = matched {
        tracing::warn!("cannot hear answers to the update notification: {e}");
        return;
    }
    let deadline = SystemTime::now() + CHECK_EVERY;
    while SystemTime::now() < deadline {
        if conn.process(Duration::from_secs(1)).is_err() {
            return;
        }
        if let Ok(chosen) = rx.try_recv() {
            if chosen {
                install(app);
            }
            return;
        }
    }
}

/// Downloads the available update's RPM to the Downloads folder and offers to install it. Does nothing while a
/// Meeting records or an update is already under way.
pub fn install(app: &AppHandle) {
    if recording(app) {
        tracing::info!("not updating during a Meeting");
        return;
    }
    if !settings::load().check_for_updates {
        return;
    }
    let update = {
        let updates = app.state::<Updates>();
        let mut state = updates.state.lock().unwrap();
        if state.busy {
            return;
        }
        let Some(update) = state.available.clone() else { return };
        state.busy = true;
        update
    };
    refresh_tray(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let installed = download_and_install(&app, &update).await;
        {
            let updates = app.state::<Updates>();
            let mut state = updates.state.lock().unwrap();
            state.busy = false;
            if installed {
                state.installed = Some(update.version);
                state.available = None;
                state.manual_command = None;
            }
        }
        refresh_tray(&app);
    });
}

/// Whether the update was installed.
async fn download_and_install(app: &AppHandle, update: &Update) -> bool {
    let version = update.version;
    let rpm = match update::download_rpm(&update.rpm, &settings::downloads_dir()).await {
        Ok(rpm) => rpm,
        Err(e) => {
            tracing::warn!("{e}");
            // The user asked for this download, so say it failed, but not over a Meeting started meanwhile.
            until_not_recording(app).await;
            message(app, MessageDialogKind::Error, "Update failed", &format!("Tiro {version} could not be downloaded.\n\n{e}")).await;
            return false;
        }
    };
    // The download can take a while; a Meeting may have started meanwhile.
    until_not_recording(app).await;
    let install = confirm(
        app,
        &format!("Install Tiro {version}?"),
        &format!(
            "Tiro {version} was downloaded to {}.\n\nYour system will ask for your password to install it. \
             After that, quit Tiro and start it again to use the new version.",
            rpm.display()
        ),
        "Install",
        "Not now",
    )
    .await;
    // A Meeting may have started while the question was open; the password prompt must not appear over it.
    if !install || recording(app) {
        return false;
    }

    let (program, args) = update::privileged_install(&rpm);
    let ran = tauri::async_runtime::spawn_blocking(move || std::process::Command::new(program).args(args).status()).await;
    let outcome = match ran {
        Ok(Ok(status)) => InstallOutcome::from_exit_code(status.code()),
        Ok(Err(e)) => {
            tracing::warn!("could not run {program}: {e}");
            InstallOutcome::Unauthorized
        }
        Err(e) => {
            tracing::warn!("the install task failed: {e}");
            InstallOutcome::Failed(None)
        }
    };
    match outcome {
        InstallOutcome::Installed => {
            tracing::info!("installed Tiro {version}");
            until_not_recording(app).await;
            let quit = confirm(
                app,
                &format!("Tiro {version} is installed"),
                "Quit Tiro and start it again to use the new version.",
                "Quit Tiro",
                "Later",
            )
            .await;
            if quit {
                tray::quit(app);
            }
            true
        }
        InstallOutcome::Cancelled => false,
        InstallOutcome::Unauthorized | InstallOutcome::Failed(_) => {
            tracing::warn!("installing Tiro {version} through pkexec ended with {outcome:?}");
            app.state::<Updates>().state.lock().unwrap().manual_command = Some(update::manual_install_command(&rpm));
            until_not_recording(app).await;
            tray::show_window(app, Some("settings"));
            false
        }
    }
}

async fn confirm(app: &AppHandle, title: &str, text: &str, ok: &str, cancel: &str) -> bool {
    let dialog = app
        .dialog()
        .message(text)
        .title(title)
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(ok.into(), cancel.into()));
    tauri::async_runtime::spawn_blocking(move || dialog.blocking_show()).await.unwrap_or(false)
}

async fn message(app: &AppHandle, kind: MessageDialogKind, title: &str, text: &str) {
    let dialog = app.dialog().message(text).title(title).kind(kind);
    let _ = tauri::async_runtime::spawn_blocking(move || dialog.blocking_show()).await;
}
