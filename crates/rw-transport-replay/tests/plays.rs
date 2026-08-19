//! A recording, subscribed to as if it were a robot.

use std::time::Duration;

use rw_canonical::CanonicalValue;
use rw_record::{Recording, Topic};
use rw_transport::{ConnectionStatus, Transport, TransportError};
use rw_transport_replay::ReplayTransport;

fn topic(name: &str) -> Topic {
    Topic {
        name: name.into(),
        schema_name: "std_msgs/Int64".into(),
        schema_definition: Some("int64 data\n".into()),
    }
}

fn recording() -> Recording {
    let mut recording = Recording::new("test");
    for index in 0..10u64 {
        recording.push(
            index * 20_000_000,
            topic("/replayed"),
            CanonicalValue::Uint(index),
        );
    }
    recording
}

#[tokio::test]
async fn a_recording_announces_its_topics_as_discovery() {
    let transport = ReplayTransport::new(recording());
    let discovery = transport.discovery().borrow().clone();
    assert_eq!(discovery.topics.len(), 1);
    assert_eq!(discovery.topics[0].name, "/replayed");
    assert_eq!(discovery.topics[0].schema_name, "std_msgs/Int64");
    assert!(
        discovery.topics[0].schema_definition.is_some(),
        "the definition travels with the recording so a form can be built"
    );
}

#[tokio::test]
async fn subscribing_delivers_the_recorded_messages_in_order() {
    let transport = ReplayTransport::new(recording());
    let mut subscription = transport
        .subscribe_topic("/replayed")
        .await
        .expect("subscribes");
    transport.connect().await.expect("connects");

    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while seen.len() < 10 && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), subscription.frames.recv()).await {
            Ok(Some(frame)) => match frame.value {
                CanonicalValue::Uint(value) => seen.push(value),
                other => panic!("unexpected value {other:?}"),
            },
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    transport.disconnect().await.expect("disconnects");

    assert_eq!(seen, (0..10).collect::<Vec<u64>>());
}

#[tokio::test]
async fn connecting_reports_connected_and_starts_playing() {
    let transport = ReplayTransport::new(recording());
    transport.connect().await.expect("connects");
    assert_eq!(*transport.status().borrow(), ConnectionStatus::Connected);
    assert!(
        transport.progress().borrow().playing,
        "opening a recording plays it rather than waiting to be told"
    );
    assert_eq!(transport.progress().borrow().duration_ns, 180_000_000);
    transport.disconnect().await.expect("disconnects");
    assert_eq!(*transport.status().borrow(), ConnectionStatus::Disconnected);
}

#[tokio::test]
async fn pausing_stops_the_clock() {
    let transport = ReplayTransport::new(recording());
    transport.connect().await.expect("connects");
    transport.set_playing(false).await;
    let at = transport.progress().borrow().at_ns;
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(
        transport.progress().borrow().at_ns,
        at,
        "a paused recording does not advance"
    );
    transport.disconnect().await.expect("disconnects");
}

#[tokio::test]
async fn seeking_moves_the_playhead() {
    let transport = ReplayTransport::new(recording());
    transport.connect().await.expect("connects");
    transport.set_playing(false).await;
    transport.seek(0.5).await;
    let at = transport.progress().borrow().at_ns;
    assert!(
        (85_000_000..=95_000_000).contains(&at),
        "seek to the middle landed at {at}"
    );
    transport.disconnect().await.expect("disconnects");
}

#[tokio::test]
async fn a_topic_the_recording_never_saw_is_refused() {
    let transport = ReplayTransport::new(recording());
    assert!(matches!(
        transport.subscribe_topic("/never").await,
        Err(TransportError::UnknownTopic(_))
    ));
}

#[tokio::test]
async fn a_recording_cannot_be_published_to_or_called() {
    let transport = ReplayTransport::new(recording());
    assert!(transport
        .publish("/replayed", CanonicalValue::Null)
        .await
        .is_err());
    assert!(transport
        .call_service("/anything", CanonicalValue::Null)
        .await
        .is_err());
    assert!(transport
        .send_action_goal("/anything", CanonicalValue::Null)
        .await
        .is_err());
}
