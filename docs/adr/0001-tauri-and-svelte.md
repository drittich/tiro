# Tauri (Rust) with a Svelte frontend

moneypenny is a Tauri app: a Rust backend handles audio capture, chunking and API calls, and a Svelte + TypeScript UI provides the tray menu and main window. We chose this over PySide6 (Qt-native on KDE, quicker to iterate) and Electron (heavy) for a small binary and solid Rust crates for PipeWire, Opus and D-Bus. The app is for personal use and ships as an RPM rather than a Flatpak, because Flatpak's sandbox gets in the way of PipeWire monitor capture and KWin window inspection.
