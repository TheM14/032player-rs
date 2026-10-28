//! Saved sign-in: restore on startup, 401 sign-out, user sign-out.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use client_core::{
    AudioDevice, Command, CoreConfig, CoreHandle, Engine, Event, LoginFailure, MemorySecrets,
    SavedLogin, SecretStore, Settings, SignOutReason,
};
use serde_json::json;
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct NoEngine;

impl Engine for NoEngine {
    fn load(&self, _: &str) -> Result<(), String> {
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
        Ok(Vec::new())
    }
    fn set_audio_device(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn set_exclusive(&self, _: bool) -> Result<(), String> {
        Ok(())
    }
}

/// Accepts only the API token `good`. An empty library skips the media
/// token probe. `lists_ok` false: the album list answers 401.
async fn server(lists_ok: bool) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/capabilities"))
        .and(header("authorization", "Bearer good"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1", "apiRevision": 3, "features": { "browse": true },
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/capabilities"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(2)
        .mount(&server)
        .await;
    let empty = json!({ "items": [], "limit": 500, "offset": 0, "total": 0 });
    Mock::given(path("/api/v1/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty.clone()))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/browse/tracks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty.clone()))
        .mount(&server)
        .await;
    let albums = if lists_ok {
        ResponseTemplate::new(200).set_body_json(empty)
    } else {
        ResponseTemplate::new(401)
    };
    Mock::given(path("/api/v1/browse/albums"))
        .respond_with(albums)
        .mount(&server)
        .await;
    server
}

fn data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("032player-signin-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

struct Core {
    handle: Option<CoreHandle>,
    events: mpsc::Receiver<Event>,
}

impl Core {
    fn start(data_dir: &Path, secrets: &Arc<MemorySecrets>) -> Self {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let config = CoreConfig {
            data_dir: data_dir.to_path_buf(),
            secrets: secrets.clone(),
            settings: Settings::default(),
        };
        let handle = client_core::start(Arc::new(NoEngine), config, move |e| {
            let _ = tx.lock().unwrap().send(e);
        });
        Self {
            handle: Some(handle),
            events: rx,
        }
    }

    fn send(&self, command: Command) {
        self.handle.as_ref().unwrap().send(command);
    }

    /// Waits for the first event matching `pred`, skipping others.
    fn wait(&self, what: &str, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(e) if pred(&e) => return e,
                Ok(_) => {}
                Err(_) => panic!("timed out waiting for {what}"),
            }
        }
    }

    async fn shutdown(mut self) {
        let handle = self.handle.take().unwrap();
        tokio::task::spawn_blocking(move || handle.shutdown())
            .await
            .unwrap();
    }
}

fn saved(e: &Event) -> Option<&SavedLogin> {
    match e {
        Event::SavedLogin(s) => Some(s),
        _ => None,
    }
}

fn secret(secrets: &MemorySecrets, kind: &str, server: &MockServer) -> Option<String> {
    secrets.get(&format!("{kind}@{}", server.uri())).unwrap()
}

/// Secret writes run in the background: poll until `pred` holds.
async fn eventually(what: &str, pred: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pred() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_login_is_restored_and_user_sign_out_forgets_it() {
    let server = server(true).await;
    let dir = data_dir("restore");
    let secrets = Arc::new(MemorySecrets::default());

    let core = Core::start(&dir, &secrets);
    core.send(Command::Restore);
    let first = core.wait("nothing saved", |e| saved(e).is_some());
    assert_eq!(first, Event::SavedLogin(SavedLogin::default()));
    core.send(Command::Login {
        server_url: format!("{}/", server.uri()),
        api_token: "good".into(),
        media_token: "media".into(),
    });
    core.wait("login", |e| matches!(e, Event::LoggedIn { .. }));
    core.shutdown().await;
    assert_eq!(
        secret(&secrets, "api-token", &server).as_deref(),
        Some("good")
    );
    assert_eq!(
        secret(&secrets, "media-token", &server).as_deref(),
        Some("media")
    );

    // Next start: signs in by itself.
    let core = Core::start(&dir, &secrets);
    core.send(Command::Restore);
    let restored = core.wait("saved login", |e| saved(e).is_some());
    assert_eq!(
        restored,
        Event::SavedLogin(SavedLogin {
            server_url: server.uri(),
            has_api_token: true,
            has_media_token: true,
            signing_in: true,
        })
    );
    core.wait("login", |e| matches!(e, Event::LoggedIn { .. }));

    core.send(Command::Logout);
    core.wait("signed out", |e| {
        *e == Event::SignedOut(SignOutReason::User)
    });
    let after = core.wait("saved login", |e| saved(e).is_some());
    assert_eq!(
        after,
        Event::SavedLogin(SavedLogin {
            server_url: server.uri(),
            ..SavedLogin::default()
        })
    );
    eventually("tokens forgotten", || {
        secret(&secrets, "api-token", &server).is_none()
            && secret(&secrets, "media-token", &server).is_none()
    })
    .await;
    core.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn unauthorized_signs_out_and_keeps_the_media_token() {
    let server = server(false).await;
    let dir = data_dir("expired");
    let secrets = Arc::new(MemorySecrets::default());
    let core = Core::start(&dir, &secrets);

    core.send(Command::Login {
        server_url: server.uri(),
        api_token: "good".into(),
        media_token: "media".into(),
    });
    core.wait("login", |e| matches!(e, Event::LoggedIn { .. }));
    // The album list answers 401.
    core.wait("expired", |e| {
        *e == Event::SignedOut(SignOutReason::Expired)
    });
    let after = core.wait("saved login", |e| saved(e).is_some());
    assert_eq!(
        after,
        Event::SavedLogin(SavedLogin {
            server_url: server.uri(),
            has_api_token: false,
            has_media_token: true,
            signing_in: false,
        })
    );
    eventually("api token forgotten", || {
        secret(&secrets, "api-token", &server).is_none()
    })
    .await;
    assert_eq!(
        secret(&secrets, "media-token", &server).as_deref(),
        Some("media")
    );

    // Only the new API token is entered; the media token comes from the store.
    core.send(Command::Login {
        server_url: server.uri(),
        api_token: "good".into(),
        media_token: String::new(),
    });
    core.wait("login again", |e| matches!(e, Event::LoggedIn { .. }));
    core.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_saved_token_is_forgotten() {
    let server = server(true).await;
    let dir = data_dir("rejected");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("session.json"),
        json!({ "server": server.uri() }).to_string(),
    )
    .unwrap();
    let secrets = Arc::new(MemorySecrets::default());
    let key = |kind: &str| format!("{kind}@{}", server.uri());
    secrets.set(&key("api-token"), "rotated").unwrap();
    secrets.set(&key("media-token"), "media").unwrap();

    let core = Core::start(&dir, &secrets);
    core.send(Command::Restore);
    let first = core.wait("saved login", |e| saved(e).is_some());
    assert!(saved(&first).unwrap().signing_in);
    let second = core.wait("updated saved login", |e| saved(e).is_some());
    assert_eq!(
        second,
        Event::SavedLogin(SavedLogin {
            server_url: server.uri(),
            has_api_token: false,
            has_media_token: true,
            signing_in: false,
        })
    );
    core.wait("failure", |e| {
        *e == Event::LoginFailed(LoginFailure::ApiTokenInvalid)
    });
    assert_eq!(secret(&secrets, "api-token", &server), None);
    assert_eq!(
        secret(&secrets, "media-token", &server).as_deref(),
        Some("media")
    );
    core.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}
