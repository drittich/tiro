//! Lists KWin's windows over D-Bus. KWin has no method for that, so each request loads a one-shot KWin script
//! that sends the window list back to this app's D-Bus connection, bottom of the stacking order first. Tested
//! by hand.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use dbus::blocking::Connection;
use dbus::channel::{MatchingReceiver, Sender as _};
use dbus::message::MatchRule;
use tiro_core::detection::Window;

const INTERFACE: &str = "org.tiro.Detection";
const PATH: &str = "/org/tiro/Detection";
const METHOD: &str = "Windows";
const TIMEOUT: Duration = Duration::from_secs(2);

pub struct WindowLister {
    script: PathBuf,
    plugin: String,
}

impl WindowLister {
    /// Writes the script and has `conn` pass every window list it sends back to `on_windows`. The lists arrive
    /// while `conn` is processing messages, some time after `request`. `name` keeps the script of each lister
    /// (detection, screenshots) apart.
    pub fn new(conn: &Connection, name: &str, on_windows: Sender<Vec<Window>>) -> std::io::Result<Self> {
        let plugin = format!("tiro-{name}-{}", std::process::id());
        let script = std::env::temp_dir().join(format!("{plugin}.js"));
        let source = format!(
            "callDBus({service:?}, {PATH:?}, {INTERFACE:?}, {METHOD:?}, JSON.stringify(\n\
             \x20   (workspace.stackingOrder || workspace.windowList())\n\
             \x20   .filter(w => w.normalWindow).map(w => [w.internalId.toString(), w.resourceClass, w.caption])));\n",
            service = conn.unique_name().to_string(),
        );
        std::fs::write(&script, source)?;

        let rule = MatchRule::new_method_call().with_interface(INTERFACE).with_member(METHOD);
        conn.start_receive(
            rule,
            Box::new(move |message, conn| {
                let _ = conn.send(message.method_return());
                match message.read1::<&str>().map(serde_json::from_str::<Vec<(String, String, String)>>) {
                    Ok(Ok(list)) => {
                        let windows = list.into_iter().map(|(id, class, caption)| Window { id, class, caption }).collect();
                        let _ = on_windows.send(windows);
                    }
                    _ => tracing::warn!("unreadable window list from KWin"),
                }
                true
            }),
        );
        Ok(Self { script, plugin })
    }

    /// Asks KWin for the window list.
    pub fn request(&self, conn: &Connection) -> Result<(), dbus::Error> {
        let scripting = conn.with_proxy("org.kde.KWin", "/Scripting", TIMEOUT);
        // A script left loaded by a failed request would make loading it again fail.
        let _: Result<(bool,), _> = scripting.method_call("org.kde.kwin.Scripting", "unloadScript", (&self.plugin,));
        let (id,): (i32,) = scripting.method_call(
            "org.kde.kwin.Scripting",
            "loadScript",
            (self.script.to_string_lossy().as_ref(), &self.plugin),
        )?;
        let script = conn.with_proxy("org.kde.KWin", format!("/Scripting/Script{id}"), TIMEOUT);
        let ran: Result<(), _> = script.method_call("org.kde.kwin.Script", "run", ());
        let _: Result<(bool,), _> = scripting.method_call("org.kde.kwin.Scripting", "unloadScript", (&self.plugin,));
        ran
    }
}

impl Drop for WindowLister {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.script);
    }
}
