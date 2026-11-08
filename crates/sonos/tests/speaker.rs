use std::time::Duration;

use sonos::fake::{FakeSpeaker, UUID};
use sonos::{Item, PlayMode, Speaker, TransportState};

fn item(n: usize) -> Item {
    Item {
        uri: format!("http://10.0.0.9:4533/api/v1/tracks/{n}/stream?mediaToken=m"),
        mime: "audio/flac".into(),
        title: format!("曲 {n}"),
        duration_ms: 180_000,
        id: format!("032-track-{n}"),
        ..Item::default()
    }
}

#[tokio::test]
async fn replace_queue_and_follow_status() {
    let fake = FakeSpeaker::start().await;
    let speaker = Speaker::new(sonos::http_client(), &fake.room());
    let items: Vec<Item> = (1..=20).map(item).collect();
    speaker.replace(&items, 3, 65_000, true).await.unwrap();

    let log = fake.log();
    assert_eq!(
        log,
        vec![
            "RemoveAllTracksFromQueue".to_owned(),
            "AddMultipleURIsToQueue 16".into(),
            "AddMultipleURIsToQueue 4".into(),
            format!("SetAVTransportURI x-rincon-queue:{UUID}#0"),
            "Seek TRACK_NR 4".into(),
            "Play".into(),
            "Seek REL_TIME 0:01:05".into(),
        ]
    );
    let status = speaker.status().await.unwrap();
    assert_eq!(status.state, TransportState::Playing);
    assert_eq!(status.index, Some(3));
    assert_eq!(status.position_ms, 65_000);
    assert!(status.queue_active);
    assert_eq!(status.queue_length, 20);
    assert_eq!(status.track_uri, items[3].uri);

    let queue = speaker.queue(1000).await.unwrap();
    assert_eq!(queue.len(), 20);
    assert_eq!(queue[19].uri, items[19].uri);
    assert_eq!(queue[0].title, "曲 1");

    // Edits.
    speaker.insert(&[item(99)], Some(5)).await.unwrap();
    assert_eq!(fake.state().queue[5].0, item(99).uri);
    speaker.move_item(5, 0).await.unwrap();
    assert_eq!(fake.state().queue[0].0, item(99).uri);
    speaker.move_item(0, 2).await.unwrap();
    assert_eq!(fake.state().queue[2].0, item(99).uri);
    speaker.remove(2).await.unwrap();
    assert_eq!(fake.state().queue.len(), 20);
    speaker.replace_item(4, &item(77)).await.unwrap();
    assert_eq!(fake.state().queue[4].0, item(77).uri);
    assert_eq!(fake.state().queue.len(), 20);

    speaker.pause().await.unwrap();
    assert_eq!(
        speaker.status().await.unwrap().state,
        TransportState::Paused
    );
    // Pausing a paused speaker is UPnP 701.
    let err = speaker.pause().await.unwrap_err();
    assert_eq!(err.upnp_code(), Some(701));

    speaker.play_index(7, 0).await.unwrap();
    assert_eq!(speaker.status().await.unwrap().index, Some(7));
    speaker.set_play_mode(PlayMode::RepeatAll).await.unwrap();
    assert_eq!(speaker.play_mode().await.unwrap(), "REPEAT_ALL");
    speaker.set_volume(42).await.unwrap();
    assert_eq!(speaker.volume().await.unwrap(), 42);
}

#[tokio::test]
async fn discovers_configured_hosts() {
    let fake = FakeSpeaker::start().await;
    let rooms = sonos::discover(
        &sonos::http_client(),
        Duration::from_millis(200),
        &[fake.host()],
    )
    .await
    .unwrap();
    assert_eq!(rooms.len(), 1);
    assert_eq!(rooms[0].id, UUID);
    assert_eq!(rooms[0].name, "测试房间");
    assert_eq!(rooms[0].base_url, fake.server.uri());
    assert_eq!(rooms[0].member_count, 1);
}
