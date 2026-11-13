//! `~/.config/tiro/window.json`: the main window's size, position and maximized state, restored at launch and
//! fitted inside a monitor's work area so none of it is offscreen (ticket 32). On Wayland an app can neither read
//! nor set its window's position, so only the size and maximized state are kept there and KWin places the window.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewWindow, Window, WindowEvent};
use tiro_core::xdg;

/// The window's last normal (not maximized) geometry in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    /// The outer top-left corner; `None` where it can't be known.
    pub position: Option<(i32, i32)>,
    /// The inner size.
    pub size: (u32, u32),
    /// The title bar and borders: outer size minus inner size.
    pub frame: (u32, u32),
    pub maximized: bool,
}

/// A monitor's work area (without panels) in physical pixels, and its scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

/// The latest geometry seen by [`track`], written by [`save`].
#[derive(Default)]
pub struct Tracked(Mutex<Option<WindowState>>);

/// `saved`, moved and shrunk so the whole window fits one monitor's work area: the one it overlaps most, or the
/// primary one when it overlaps none or has no position. Its inner size never drops below `min` (logical pixels).
pub fn fit(saved: &WindowState, areas: &[Area], primary: Option<&Area>, min: (f64, f64)) -> WindowState {
    let (outer_w, outer_h) = (saved.size.0 + saved.frame.0, saved.size.1 + saved.frame.1);
    let overlap = |a: &Area, (x, y): (i32, i32)| {
        let w = (i64::from(x) + i64::from(outer_w)).min(i64::from(a.x) + i64::from(a.width)) - i64::from(x.max(a.x));
        let h = (i64::from(y) + i64::from(outer_h)).min(i64::from(a.y) + i64::from(a.height)) - i64::from(y.max(a.y));
        w.max(0) * h.max(0)
    };
    let most = saved
        .position
        .and_then(|pos| areas.iter().map(|a| (overlap(a, pos), a)).filter(|(o, _)| *o > 0).max_by_key(|(o, _)| *o));
    let Some(area) = most.map(|(_, a)| a).or(primary).or(areas.first()) else { return *saved };

    let min_w = (min.0 * area.scale).round() as u32;
    let min_h = (min.1 * area.scale).round() as u32;
    let width = outer_w.min(area.width).saturating_sub(saved.frame.0).max(min_w);
    let height = outer_h.min(area.height).saturating_sub(saved.frame.1).max(min_h);
    let clamp = |at: i32, start: i32, room: u32, used: u32| {
        let last = i64::from(start) + i64::from(room) - i64::from(used);
        i64::from(at).min(last).max(i64::from(start)) as i32
    };
    let position = saved.position.map(|(x, y)| {
        (clamp(x, area.x, area.width, width + saved.frame.0), clamp(y, area.y, area.height, height + saved.frame.1))
    });
    WindowState { position, size: (width, height), frame: saved.frame, maximized: saved.maximized }
}

/// Applies the saved geometry to the (still hidden) main window; nothing happens before the first save.
pub fn restore(window: &WebviewWindow) {
    let Some(saved) = load() else { return };
    let areas: Vec<Area> = window.available_monitors().unwrap_or_default().iter().map(area).collect();
    let primary = window.primary_monitor().ok().flatten().map(|m| area(&m));
    let min = window
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == window.label())
        .map_or((0.0, 0.0), |w| (w.min_width.unwrap_or(0.0), w.min_height.unwrap_or(0.0)));
    let saved = WindowState { position: saved.position.filter(|_| positions_work()), ..saved };
    let fitted = fit(&saved, &areas, primary.as_ref(), min);
    let _ = window.set_size(PhysicalSize::new(fitted.size.0, fitted.size.1));
    if let Some((x, y)) = fitted.position {
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
    if fitted.maximized {
        let _ = window.maximize();
    }
}

/// Keeps the main window's latest geometry; while it is maximized, the normal geometry from before is kept.
pub fn track(window: &Window, event: &WindowEvent) {
    if window.label() != "main" || !matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_)) {
        return;
    }
    let tracked = window.state::<Tracked>();
    let mut state = tracked.0.lock().unwrap();
    if window.is_maximized().unwrap_or(false) {
        if let Some(state) = state.as_mut() {
            state.maximized = true;
        }
        return;
    }
    let (Ok(inner), Ok(outer)) = (window.inner_size(), window.outer_size()) else { return };
    let position = window.outer_position().ok().filter(|_| positions_work()).map(|p| (p.x, p.y));
    *state = Some(WindowState {
        position,
        size: (inner.width, inner.height),
        frame: (outer.width.saturating_sub(inner.width), outer.height.saturating_sub(inner.height)),
        maximized: false,
    });
}

/// Writes the tracked geometry, if the window has been shown or changed since launch.
pub fn save(app: &AppHandle) {
    let Some(state) = *app.state::<Tracked>().0.lock().unwrap() else { return };
    let path = path();
    let written = std::fs::create_dir_all(path.parent().expect("a config folder"))
        .and_then(|()| xdg::write_atomic(&path, serde_json::to_string_pretty(&state)?.as_bytes()));
    if let Err(e) = written {
        tracing::warn!("could not remember the window's position: {e}");
    }
}

fn load() -> Option<WindowState> {
    serde_json::from_str(&std::fs::read_to_string(path()).ok()?).ok()
}

fn path() -> PathBuf {
    xdg::config_home().join("tiro").join("window.json")
}

fn area(monitor: &tauri::Monitor) -> Area {
    let work = monitor.work_area();
    Area {
        x: work.position.x,
        y: work.position.y,
        width: work.size.width,
        height: work.size.height,
        scale: monitor.scale_factor(),
    }
}

/// Whether the app can read and set its window's position: not on Wayland, unless GTK was sent to XWayland.
fn positions_work() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_none() || std::env::var("GDK_BACKEND").is_ok_and(|b| b.starts_with("x11"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: Area = Area { x: 0, y: 0, width: 1920, height: 1040, scale: 1.0 };
    const RIGHT: Area = Area { x: 1920, y: 0, width: 2560, height: 1400, scale: 2.0 };

    fn state(position: Option<(i32, i32)>, size: (u32, u32)) -> WindowState {
        WindowState { position, size, frame: (0, 30), maximized: false }
    }

    #[test]
    fn a_window_that_fits_is_left_where_it_was() {
        let saved = state(Some((100, 50)), (960, 720));
        assert_eq!(fit(&saved, &[LEFT, RIGHT], Some(&LEFT), (480.0, 400.0)), saved);
    }

    #[test]
    fn a_window_hanging_off_the_right_and_bottom_is_moved_back_on() {
        let saved = state(Some((1500, 600)), (960, 720));
        let fitted = fit(&saved, &[LEFT], Some(&LEFT), (480.0, 400.0));
        assert_eq!(fitted.position, Some((960, 290)));
        assert_eq!(fitted.size, (960, 720));
    }

    #[test]
    fn a_window_on_a_monitor_that_is_gone_goes_to_the_primary_one() {
        let saved = state(Some((5000, 200)), (960, 720));
        assert_eq!(fit(&saved, &[LEFT], Some(&LEFT), (480.0, 400.0)).position, Some((960, 200)));
    }

    #[test]
    fn a_window_across_two_monitors_moves_onto_the_one_it_overlaps_most() {
        let saved = state(Some((1800, 100)), (960, 720));
        assert_eq!(fit(&saved, &[LEFT, RIGHT], Some(&LEFT), (480.0, 400.0)).position, Some((1920, 100)));
    }

    #[test]
    fn a_window_larger_than_the_work_area_is_shrunk_to_fit_with_its_frame() {
        let saved = state(Some((-50, -50)), (3000, 2000));
        let fitted = fit(&saved, &[LEFT], Some(&LEFT), (480.0, 400.0));
        assert_eq!(fitted.position, Some((0, 0)));
        assert_eq!(fitted.size, (1920, 1010));
    }

    #[test]
    fn the_size_never_drops_below_the_minimum_at_the_monitors_scale() {
        let tiny = Area { x: 0, y: 0, width: 800, height: 600, scale: 2.0 };
        assert_eq!(fit(&state(None, (2000, 2000)), &[tiny], None, (480.0, 400.0)).size, (960, 800));
    }

    #[test]
    fn without_a_position_only_the_size_is_fitted_to_the_primary_monitor() {
        let fitted = fit(&state(None, (3000, 720)), &[RIGHT, LEFT], Some(&LEFT), (480.0, 400.0));
        assert_eq!(fitted.position, None);
        assert_eq!(fitted.size, (1920, 720));
    }

    #[test]
    fn with_no_monitors_the_saved_geometry_is_used_as_is() {
        let saved = state(Some((5000, 5000)), (960, 720));
        assert_eq!(fit(&saved, &[], None, (480.0, 400.0)), saved);
    }
}
