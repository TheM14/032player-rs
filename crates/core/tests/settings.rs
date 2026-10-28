//! Settings: output changes go through the engine and persist.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use client_core::{
    AudioDevice, Command, CoreConfig, Engine, EngineEvent, Event, MemorySecrets, Settings,
};

#[derive(Default)]
struct FakeEngine {
    calls: Mutex<Vec<String>>,
    /// Exclusive mode is refused (the engine rolled back).
    refuse_exclusive: AtomicBool,
}

impl FakeEngine {
    fn push(&self, call: String) -> Result<(), String> {
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

impl Engine for FakeEngine {
    fn load(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn append(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn clear_upcoming(&self) -> Result<(), String> {
        Ok(())
    }
    fn stop(&self) -> Result<(), String> {
        Ok(())
    }
    fn set_paused(&self, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn seek(&self, _: f64) -> Result<(), String> {
        Ok(())
    }
    fn set_volume(&self, _: f64) -> Result<(), String> {
        Ok(())
    }
    fn audio_devices(&self) -> Result<Vec<AudioDevice>, String> {
        Ok(vec![AudioDevice {
            name: "wasapi/{a}".into(),
            description: "Speakers".into(),
        }])
    }
    fn set_audio_device(&self, name: &str) -> Result<(), String> {
        self.push(format!("device {name}"))
    }
    fn set_exclusive(&self, enabled: bool) -> Result<(), String> {
        if self.refuse_exclusive.load(Ordering::Relaxed) {
            return Err("mpv did not apply exclusive mode".into());
        }
        self.push(format!("exclusive {enabled}"))
    }
}

fn wait(rx: &mpsc::Receiver<Event>, what: &str, pred: impl Fn(&Event) -> bool) -> Event {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(e) if pred(&e) => return e,
            Ok(_) => {}
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }
}

fn start(dir: &Path, engine: &Arc<FakeEngine>) -> (client_core::CoreHandle, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let config = CoreConfig {
        data_dir: dir.to_path_buf(),
        secrets: Arc::new(MemorySecrets::default()),
        settings: Settings::load(dir),
    };
    let core = client_core::start(engine.clone(), config, move |e| {
        let _ = tx.lock().unwrap().send(e);
    });
    (core, rx)
}

#[tokio::test(flavor = "multi_thread")]
async fn output_changes_and_volume_are_saved() {
    let dir = std::env::temp_dir().join(format!("032player-settings-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Arc::new(FakeEngine::default());
    let (core, rx) = start(&dir, &engine);

    core.send(Command::RefreshAudioDevices);
    let devices = wait(&rx, "devices", |e| matches!(e, Event::AudioDevices(_)));
    assert!(matches!(devices, Event::AudioDevices(d) if d[0].description == "Speakers"));

    core.send(Command::SetAudioDevice(Some("wasapi/{a}".into())));
    wait(&rx, "device", |e| {
        *e == Event::AudioDevice(Some("wasapi/{a}".into()))
    });
    core.send(Command::SetExclusive(true));
    wait(&rx, "exclusive", |e| *e == Event::Exclusive(true));

    // Refused: the old value comes back and nothing is saved.
    engine.refuse_exclusive.store(true, Ordering::Relaxed);
    core.send(Command::SetExclusive(false));
    wait(&rx, "notice", |e| matches!(e, Event::Notice(_)));
    wait(&rx, "rollback", |e| *e == Event::Exclusive(true));

    core.engine_event(EngineEvent::Volume(35.0));
    core.send(Command::SetShowLyrics(false));
    wait(&rx, "volume", |e| *e == Event::Volume(35.0));
    // Debounced write; shutdown flushes anything pending.
    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();

    assert_eq!(
        engine.calls.lock().unwrap().clone(),
        ["device wasapi/{a}", "exclusive true"]
    );
    assert_eq!(
        Settings::load(&dir),
        Settings {
            volume: 35.0,
            audio_device: Some("wasapi/{a}".into()),
            exclusive: true,
            show_lyrics: false,
            ..Settings::default()
        }
    );

    // Back to the system default.
    let engine = Arc::new(FakeEngine::default());
    let (core, rx) = start(&dir, &engine);
    core.send(Command::SetAudioDevice(None));
    wait(&rx, "default device", |e| *e == Event::AudioDevice(None));
    tokio::task::spawn_blocking(move || core.shutdown())
        .await
        .unwrap();
    assert_eq!(engine.calls.lock().unwrap().clone(), ["device auto"]);
    assert_eq!(Settings::load(&dir).audio_device, None);
    let _ = std::fs::remove_dir_all(&dir);
}
