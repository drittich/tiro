use std::sync::Arc;
use std::time::Duration;

use tiro_core::acks::Acks;

#[tokio::test(start_paused = true)]
async fn a_request_the_ui_acknowledges_is_done() {
    let acks = Arc::new(Acks::default());
    let request = acks.request();
    let id = request.id();

    let ui = acks.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        ui.ack(id);
    });

    let started = tokio::time::Instant::now();
    assert!(acks.wait(request, Duration::from_secs(2)).await);
    assert_eq!(started.elapsed(), Duration::from_millis(300), "no longer than the UI takes");
}

#[tokio::test(start_paused = true)]
async fn a_request_nobody_acknowledges_gives_up_after_the_timeout() {
    let acks = Acks::default();
    let request = acks.request();

    let started = tokio::time::Instant::now();
    assert!(!acks.wait(request, Duration::from_secs(2)).await);
    assert_eq!(started.elapsed(), Duration::from_secs(2));
}

#[tokio::test(start_paused = true)]
async fn each_request_waits_for_its_own_ack() {
    let acks = Acks::default();
    let first = acks.request();
    let second = acks.request();
    assert_ne!(first.id(), second.id());

    acks.ack(second.id());
    acks.ack(9_999); // unknown or late acks are ignored

    assert!(acks.wait(second, Duration::from_secs(2)).await);
    assert!(!acks.wait(first, Duration::from_secs(2)).await);
}
