# Patched tray-icon 0.25.1 and libappindicator 0.9.0

Copies of [tray-icon](https://crates.io/crates/tray-icon) and
[libappindicator](https://crates.io/crates/libappindicator), used through `[patch.crates-io]` in the workspace
`Cargo.toml`.

Upstream tray-icon ignores the tooltip on Linux, so Plasma shows nothing when you hover over the tray icon. The
patch (search for "tiro patch") adds `AppIndicator::set_tooltip_title` to libappindicator, which calls
`app_indicator_set_tooltip_title` from libayatana-appindicator 0.6+ when the library has it, and makes tray-icon
call it with the tooltip.

When Tauri moves to newer versions of these crates, copy them here, apply the same changes and update the
versions in this file, or drop the patch if upstream shows the tooltip itself.
