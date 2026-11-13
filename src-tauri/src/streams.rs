//! Watches the PipeWire registry for audio capture streams through `pw-dump --monitor`, which prints the
//! registry as JSON and then every change to it. Tested by hand.

use std::collections::{HashMap, HashSet};
use std::io::BufReader;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Duration;

use tiro_core::detection::{Event, StreamProps};
use serde_json::Value;

use crate::capture;

/// Sends `StreamOpened` and `StreamClosed` for every audio capture stream, for the life of the app. Runs on its
/// own long-lived thread: pw-dump is tied to it so it exits with the app.
pub fn watch(events: Sender<Event>) {
    std::thread::Builder::new()
        .name("pipewire-streams".into())
        .spawn(move || loop {
            if let Err(e) = monitor(&events) {
                tracing::warn!("meeting detection cannot watch PipeWire: {e}");
            }
            // pw-dump ended (PipeWire restarted?); the receiver may be gone too.
            std::thread::sleep(Duration::from_secs(5));
        })
        .expect("spawn the PipeWire stream watcher");
}

fn monitor(events: &Sender<Event>) -> Result<(), String> {
    let mut command = Command::new("pw-dump");
    command.args(["--monitor", "--no-colors"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    capture::exit_with_parent(&mut command);
    let mut child = command.spawn().map_err(|e| format!("could not start pw-dump: {e}"))?;
    let stdout = child.stdout.take().expect("piped stdout");

    let mut registry = Registry::default();
    let updates = serde_json::Deserializer::from_reader(BufReader::new(stdout)).into_iter::<Vec<Value>>();
    let mut result = Ok(());
    'read: for update in updates {
        let update = match update {
            Ok(update) => update,
            Err(e) => {
                result = Err(format!("unreadable pw-dump output: {e}"));
                break;
            }
        };
        for object in &update {
            if let Some(event) = registry.apply(object) {
                if events.send(event).is_err() {
                    break 'read;
                }
            }
        }
    }
    for id in registry.streams.drain() {
        let _ = events.send(Event::StreamClosed { id });
    }
    let _ = child.kill();
    let _ = child.wait();
    result
}

#[derive(Default)]
struct Registry {
    /// Clients by id: native PipeWire clients put the process binary on the client rather than the node.
    clients: HashMap<u64, StreamProps>,
    /// Capture streams already reported open.
    streams: HashSet<u32>,
}

impl Registry {
    fn apply(&mut self, object: &Value) -> Option<Event> {
        let id = object["id"].as_u64()?;
        let info = &object["info"];
        if info.is_null() {
            // Removed from the registry.
            self.clients.remove(&id);
            let id = u32::try_from(id).ok()?;
            return self.streams.remove(&id).then_some(Event::StreamClosed { id });
        }
        let props = &info["props"];
        match object["type"].as_str()? {
            "PipeWire:Interface:Client" => {
                self.clients.insert(id, stream_props(props));
                None
            }
            "PipeWire:Interface:Node" if props["media.class"] == "Stream/Input/Audio" => {
                let id = u32::try_from(id).ok()?;
                if !self.streams.insert(id) {
                    return None;
                }
                let mut stream = stream_props(props);
                if let Some(client) = props["client.id"].as_u64().and_then(|c| self.clients.get(&c)) {
                    stream.binary = stream.binary.or_else(|| client.binary.clone());
                    stream.app_name = stream.app_name.or_else(|| client.app_name.clone());
                }
                Some(Event::StreamOpened { id, props: stream })
            }
            _ => None,
        }
    }
}

fn stream_props(props: &Value) -> StreamProps {
    let text = |key: &str| props[key].as_str().map(str::to_owned);
    StreamProps { binary: text("application.process.binary"), app_name: text("application.name") }
}
