# Tiro

A tray app for Fedora KDE Plasma that records online meetings as two Tracks (You: your microphone; Others: whatever plays through your speakers), transcribes them live through OpenRouter, and summarizes them afterwards.

## Install

### From a release

Download the RPM from the [latest release](https://github.com/drittich/tiro/releases/latest), then install it:

```sh
sudo dnf install ./Tiro-0.6.6-1.x86_64.rpm
```

The RPM pulls in `opus`, PipeWire (`pipewire`, `pipewire-libs`, `pipewire-utils`) and `pulseaudio-utils` (for `pactl`). The Silero VAD model is built into the binary, so there is nothing else to download. See [Which systems it runs on](#which-systems-it-runs-on) before installing on anything other than Fedora KDE Plasma.

Start **Tiro** from the application launcher. To start it at login, tick **Start Tiro when I log in** in Settings: this writes `~/.config/autostart/tiro.desktop`, which starts Tiro in the tray without opening its window. Untick it to remove the file.

### Build from source

You need Node.js, Rust (`cargo`, usually in `~/.cargo/bin`), `rpm-build` and `opus-devel`. From the repo root:

```sh
npm install
scripts/build-rpm.sh
```

The script asks whether to raise the minor version, builds the RPM, and prints the command to install it, for example:

```sh
sudo dnf install ./target/release/bundle/rpm/Tiro-0.6.6-1.x86_64.rpm
```

See [Versions](#versions) for how the version changes on each build.

### Which systems it runs on

Tiro has been tested only on **Fedora 44 and 45 with KDE Plasma 6, on x86_64**. The release RPM is built on Fedora and names Fedora's packages as dependencies.

| System | Status |
|---|---|
| Fedora 44 or 45, KDE Plasma 6 | Tested. Install the release RPM. |
| Other Fedora-based KDE systems (e.g. Fedora Kinoite with `rpm-ostree install`) | Should work with the release RPM, but untested. |
| Fedora with another desktop (GNOME, etc.) | Untested. Recording, transcription and summaries need only PipeWire, but browser meeting detection and screenshots need KWin, so they won't work. GNOME needs the AppIndicator extension to show the tray icon. |
| Other RPM distros (openSUSE, RHEL and its rebuilds) | The release RPM won't install as is, because its dependencies use Fedora's package names. Build from source. Untested. |
| Debian, Ubuntu, Arch and others | No package. Building from source may work where the requirements below are met, but it's untested. |
| ARM (aarch64) | No release build. |

### Update

Quit Tiro from its tray menu, then install the newer RPM the same way, either downloaded from [Releases](https://github.com/drittich/tiro/releases) or built with `scripts/build-rpm.sh`. Each version has a new number, so `dnf install` upgrades the installed one. Your settings and Meetings are kept.

### Upgrading from moneypenny

Tiro used to be called moneypenny. Quit moneypenny from its tray menu first, then install Tiro as above: installing the `tiro` package removes the `moneypenny` one. On first launch, Tiro moves your settings, API key and Meetings from `~/.config/moneypenny` and `~/.local/share/moneypenny` to `~/.config/tiro` and `~/.local/share/tiro`, and replaces a moneypenny login autostart entry with its own.

### Uninstall

```sh
sudo dnf remove tiro
```

This removes the program but keeps your data. To remove that too, delete `~/.config/tiro` (settings and API key), `~/.local/share/tiro` (Meetings) and `~/.config/autostart/tiro.desktop`.

## Set your OpenRouter API key

Either paste the key into **Settings → OpenRouter API key**, or create the file yourself:

```sh
mkdir -p ~/.config/tiro
printf '%s\n' 'sk-or-v1-…' > ~/.config/tiro/openrouter.key
chmod 600 ~/.config/tiro/openrouter.key
```

Tiro refuses a key file that other users can read. The key is stored nowhere else.

## Use headphones

Without headphones your microphone picks up the remote voices, and the You Track would repeat much of what Others said. Tiro reduces this in two ways:

- **Echo-cancel**: if PipeWire's echo-cancel module is installed (it ships with `pipewire` on Fedora), the You Track records an echo-cancelled copy of your mic. If the module isn't available, the plain mic is recorded instead.
- **Echo dedupe**: You words that match what Others said at the same moment are dropped from the Transcript.

Both are fallbacks. Headphones give the cleanest Transcript, and they keep your short interjections ("yeah", "okay") from being mistaken for echo.

## Meeting detection

When Teams (`teams-for-linux`), Zoom, or a browser showing Teams, Zoom or Meet has had your microphone open for about 10 s, a notification asks whether to record. Tiro never starts recording unless you accept. When the meeting's microphone stream ends, it asks whether to stop, and stops on its own after a minute without an answer. For browsers, the meeting is confirmed from the window title, so the meeting tab must be the one showing at least once while the microphone is open.

## Meeting screenshots

Tiro can save screenshots of the meeting app's window while a Meeting records. It's off by default: turn on **Capture screenshots of the meeting window** in Settings and accept the dialog. Anything drawn in that window is captured, including shared screens, video, chat and the app's own notifications.

While recording, Tiro checks the window every 2 seconds and saves a screenshot when its content changes and then holds still, such as a new slide. Video or scrolling that never holds still gets at most one screenshot a minute. Screenshots are saved as WebP files in the Meeting's `screenshots/` folder, named by their number and their time in the Meeting, for example `00007-000152250.webp` for screenshot 7 at 2:32.25. They're deleted with the Meeting.

Tiro uses the window detection found the meeting in, or after a manual start, any meeting app's window. If there isn't one, the Live view says so and nothing is captured. If three captures in a row fail, the Live view says so too, until one works again; the first failure's reason is in Tiro's log (`journalctl --user -t tiro`). A minimized window isn't captured, because KWin keeps returning its last frame.

Screenshots use KWin's `org.kde.KWin.ScreenShot2`, which refuses every app whose `.desktop` file doesn't allow it. KWin finds that file by matching the running executable against an absolute `Exec=` path, with no `$PATH` lookup. The RPM's `Tiro.desktop` has `Exec=/usr/bin/tiro` and the key, so installed builds need no setup.

For a dev build, create `~/.local/share/applications/tiro-dev.desktop`, with `Exec=` set to the absolute path of `target/debug/tiro` in your checkout:

```ini
[Desktop Entry]
Type=Application
Name=Tiro (dev)
Exec=/home/you/src/tiro/target/debug/tiro
NoDisplay=true
X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2
```

KWin picks the file up within seconds. You don't need to log out or run `kbuildsycoca6`.

## Rename or delete a Meeting

In **Meetings**, hover over a Meeting and choose **Rename** or **Delete**. You can also edit the title at the top of an open Meeting, or delete it with the **Delete** button there. Deleting removes the Meeting's folder from `~/.local/share/tiro`, with its Transcript, Summary, Notes and audio, and cannot be undone. You can't delete a Meeting while it is recording or transcribing, or while it is being retried or summarized.

## Requirements

- Linux with PipeWire (`pw-record`, `pw-dump`, `pactl`); tested on Fedora 44 and 45
- KDE Plasma 6 (KWin) for browser meeting detection and screenshots
- `opus-devel` to build
- An OpenRouter API key in `~/.config/tiro/openrouter.key` (mode `0600`), which you can set in Settings

Silence is skipped with the bundled [Silero VAD](https://github.com/snakers4/silero-vad) model (MIT, `crates/core/models/`). It runs on ONNX Runtime, which is downloaded and linked statically at build time.

## Development

```sh
npm install
npm run tauri dev
cargo test
```

### Versions

`scripts/build-rpm.sh` builds the RPM into `target/release/bundle/rpm/`. Each build raises the patch number (0.1.1 → 0.1.2) and asks whether to raise the minor number instead (0.1.1 → 0.2.0). The version lives in the workspace `Cargo.toml` and `package.json`; if the build fails, both are put back. Commit the version change with the rest of your work. `npm run tauri build -- --bundles rpm` builds without changing the version.

The `ort` crate downloads ONNX Runtime from `cdn.pyke.io` during the first build and caches it in `~/.cache/ort.pyke.io`, so building needs network access once. To build offline, point `ORT_LIB_LOCATION` at a local static ONNX Runtime build.

Set `TIRO_LIVE_TESTS=1` to also run the test that calls OpenRouter.

## License

MIT. See [LICENSE](LICENSE).
