//! Waiting, for a limited time, until the UI confirms it has done something the backend asked for (such as
//! saving Notes still waiting on autosave before a Summary reads them).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::oneshot;

#[derive(Default)]
pub struct Acks {
    next: AtomicU64,
    waiting: Mutex<HashMap<u64, oneshot::Sender<()>>>,
}

/// A request sent to the UI, to `wait` on.
pub struct Request {
    id: u64,
    done: oneshot::Receiver<()>,
}

impl Request {
    /// What the UI passes back to `Acks::ack`.
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Acks {
    pub fn request(&self) -> Request {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, done) = oneshot::channel();
        self.waiting.lock().unwrap().insert(id, sender);
        Request { id, done }
    }

    /// The UI has done what request `id` asked. Unknown or late ids are ignored.
    pub fn ack(&self, id: u64) {
        if let Some(sender) = self.waiting.lock().unwrap().remove(&id) {
            let _ = sender.send(());
        }
    }

    /// Whether the UI acknowledged `request` within `timeout`.
    pub async fn wait(&self, request: Request, timeout: Duration) -> bool {
        let acked = matches!(tokio::time::timeout(timeout, request.done).await, Ok(Ok(())));
        self.waiting.lock().unwrap().remove(&request.id);
        acked
    }
}
