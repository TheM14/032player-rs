//! Authentication lifecycle: saved-token restore, login, sign-out and
//! serialized secret-store operations gated by a monotonic `auth_seq`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use api::{Capabilities, Client, Feature};
use session::{AdapterStatus, KeyValueStorage};

use super::m032;
use super::playback::TransportReset;
use super::{
    Core, Event, Internal, Library, LoginFailure, SERVER_KEY, SavedLogin, ServerFeatures,
    SignOutReason, Sink, cleanup_media_covers, reporter_channel,
};
use crate::browse::{AlbumFilter, TrackFilter};
use crate::images::Images;
use crate::reporter::Reporter;
use crate::secrets::{SecretStore, TokenKind, token_key};
use crate::storage::FileStorage;

/// Serializes secret-store operations and supersedes stale login/restore tasks.
pub(super) struct AuthState {
    secret_lock: Arc<tokio::sync::Mutex<()>>,
    seq: Arc<AtomicU64>,
}

impl Default for AuthState {
    fn default() -> Self {
        Self {
            secret_lock: Arc::new(tokio::sync::Mutex::new(())),
            seq: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl AuthState {
    pub(super) fn bump(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub(super) fn current(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }
}

/// Runs a blocking secret-store call off the async workers.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

async fn read_token(secrets: &Arc<dyn SecretStore>, server: &str, kind: TokenKind) -> String {
    let secrets = secrets.clone();
    let key = token_key(server, kind);
    match blocking(move || secrets.get(&key)).await {
        Some(Ok(value)) => value.unwrap_or_default(),
        Some(Err(e)) => {
            tracing::warn!("reading saved {kind:?} token: {e}");
            String::new()
        }
        None => String::new(),
    }
}

/// Saves (`Some`) or deletes (`None`) a token.
async fn write_token(
    secrets: &Arc<dyn SecretStore>,
    server: &str,
    kind: TokenKind,
    value: Option<String>,
) {
    let secrets = secrets.clone();
    let key = token_key(server, kind);
    let result = blocking(move || match value {
        Some(value) => secrets.set(&key, &value),
        None => secrets.delete(&key),
    })
    .await;
    if let Some(Err(e)) = result {
        tracing::warn!("saving {kind:?} token: {e}");
    }
}

/// Signs in; empty tokens come from the store. Saves both tokens on
/// success; forgets a saved token the server rejected. All secret-store
/// operations are serialized via `secret_lock` and gated by `auth_seq` so
/// concurrent sign-out / restore / login tasks never overwrite newer credentials.
#[allow(clippy::too_many_arguments)]
async fn sign_in(
    secrets: Arc<dyn SecretStore>,
    secret_lock: Arc<tokio::sync::Mutex<()>>,
    auth_seq: Arc<AtomicU64>,
    my_auth_seq: u64,
    sink: Sink,
    server_url: String,
    api_token: String,
    media_token: String,
) -> Internal {
    let Ok(server) = api::normalize_base_url(&server_url) else {
        return Internal::LoginFailed(my_auth_seq, LoginFailure::InvalidUrl);
    };
    let api_saved = api_token.is_empty();
    let media_saved = media_token.is_empty();
    let (api_token, media_token) = if api_saved || media_saved {
        let _guard = secret_lock.lock().await;
        if auth_seq.load(Ordering::Acquire) != my_auth_seq {
            return Internal::LoginFailed(my_auth_seq, LoginFailure::CredentialsRequired);
        }
        let api_token = if api_saved {
            read_token(&secrets, &server, TokenKind::Api).await
        } else {
            api_token
        };
        let media_token = if media_saved {
            read_token(&secrets, &server, TokenKind::Media).await
        } else {
            media_token
        };
        (api_token, media_token)
    } else {
        (api_token, media_token)
    };
    match api::authenticate(&server, &api_token, &media_token).await {
        Ok(ok) => {
            let _guard = secret_lock.lock().await;
            if auth_seq.load(Ordering::Acquire) == my_auth_seq {
                write_token(&secrets, &server, TokenKind::Api, Some(api_token)).await;
                write_token(&secrets, &server, TokenKind::Media, Some(media_token)).await;
            }
            Internal::LoggedIn(my_auth_seq, Box::new(ok))
        }
        Err(e) => {
            let failure = LoginFailure::from(e);
            let rejected = match failure {
                LoginFailure::ApiTokenInvalid if api_saved && !api_token.is_empty() => {
                    Some(TokenKind::Api)
                }
                LoginFailure::MediaTokenInvalid if media_saved && !media_token.is_empty() => {
                    Some(TokenKind::Media)
                }
                _ => None,
            };
            if let Some(kind) = rejected {
                let _guard = secret_lock.lock().await;
                if auth_seq.load(Ordering::Acquire) == my_auth_seq {
                    write_token(&secrets, &server, kind, None).await;
                    sink(Event::SavedLogin(SavedLogin {
                        server_url: server,
                        has_api_token: kind != TokenKind::Api && !api_token.is_empty(),
                        has_media_token: kind != TokenKind::Media && !media_token.is_empty(),
                        signing_in: false,
                    }));
                }
            }
            Internal::LoginFailed(my_auth_seq, failure)
        }
    }
}

/// The message to send when `e` means the API token was rejected.
pub(super) fn unauthorized(client: &Client, e: &api::ApiError) -> Option<Internal> {
    e.is_unauthorized().then(|| Internal::Unauthorized {
        api_token: client.api_token().to_owned(),
    })
}

impl Core {
    pub(super) fn restore_login(&mut self) {
        if self.library.is_some() {
            return;
        }
        let my_auth_seq = self.auth.bump();
        let server = self.session_file.get(SERVER_KEY).unwrap_or_default();
        if server.is_empty() {
            self.emit(Event::SavedLogin(SavedLogin::default()));
            return;
        }
        let secrets = self.config.secrets.clone();
        let secret_lock = self.auth.secret_lock.clone();
        let auth_seq = self.auth.seq.clone();
        let sink = self.sink.clone();
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let (api, media) = {
                let _guard = secret_lock.lock().await;
                if auth_seq.load(Ordering::Acquire) != my_auth_seq {
                    return;
                }
                let api = read_token(&secrets, &server, TokenKind::Api).await;
                let media = read_token(&secrets, &server, TokenKind::Media).await;
                (api, media)
            };
            if auth_seq.load(Ordering::Acquire) != my_auth_seq {
                return;
            }
            let signing_in = !api.is_empty() && !media.is_empty();
            sink(Event::SavedLogin(SavedLogin {
                server_url: server.clone(),
                has_api_token: !api.is_empty(),
                has_media_token: !media.is_empty(),
                signing_in,
            }));
            if signing_in {
                // Empty tokens: sign_in reads (and may forget) the saved ones.
                let msg = sign_in(
                    secrets,
                    secret_lock,
                    auth_seq,
                    my_auth_seq,
                    sink,
                    server,
                    String::new(),
                    String::new(),
                )
                .await;
                let _ = tx.send(msg);
            }
        });
    }

    pub(super) fn login(&mut self, server_url: String, api_token: String, media_token: String) {
        if self.library.is_some() {
            return;
        }
        let my_auth_seq = self.auth.bump();
        let secrets = self.config.secrets.clone();
        let secret_lock = self.auth.secret_lock.clone();
        let auth_seq = self.auth.seq.clone();
        let sink = self.sink.clone();
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let msg = sign_in(
                secrets,
                secret_lock,
                auth_seq,
                my_auth_seq,
                sink,
                server_url,
                api_token,
                media_token,
            )
            .await;
            let _ = tx.send(msg);
        });
    }

    /// Stops playback, drops the signed-in state and forgets the rejected
    /// (expired) or all (user) saved tokens.
    pub(super) fn sign_out(&mut self, reason: SignOutReason) {
        let Some(library) = self.library.take() else {
            return;
        };
        let my_auth_seq = self.auth.bump();
        self.list_generation += 1;
        self.abort_all_tasks();
        if let Some(mut reporter) = self.reporter.take()
            && !self.pb.idle
        {
            // Ends the server-side session; with an expired token this
            // is rejected and the server times the session out.
            reporter.act(|a| a.on_status(AdapterStatus::Stopped));
        }
        if self.casting() && !self.pb.idle {
            self.cast_stop();
        }
        self.drop_cast(false);
        self.emit(Event::CastOutput(None));
        let _ = self.engine.stop();
        self.queue.clear();
        self.radio_reset();
        self.radio_clear_recent_recommended();
        self.reset_transport(TransportReset::full());
        self.sleep.cancel();
        self.pb.play_intent_seq += 1;
        self.home = None;
        self.open_mix = None;
        self.sources = m032::SourceTracks::default();
        self.track_works = None;
        // Results of requests still running are dropped.
        self.latest.clear();
        self.track_filter = TrackFilter::default();
        self.favorite_albums = None;
        self.favorite_tracks = None;
        self.favorite_artists = None;
        self.playlists.clear();
        self.open_detail = None;
        self.artist_groups = None;
        self.artist_credit_group = None;
        self.artist_credit_all = None;

        let server = library.client.base_url().to_owned();
        let secrets = self.config.secrets.clone();
        let secret_lock = self.auth.secret_lock.clone();
        let auth_seq = self.auth.seq.clone();
        let has_media_token = reason == SignOutReason::Expired;
        {
            let server = server.clone();
            tokio::spawn(async move {
                let _guard = secret_lock.lock().await;
                if auth_seq.load(Ordering::Acquire) != my_auth_seq {
                    return;
                }
                write_token(&secrets, &server, TokenKind::Api, None).await;
                if !has_media_token {
                    write_token(&secrets, &server, TokenKind::Media, None).await;
                }
            });
        }

        cleanup_media_covers(&self.config.data_dir, None);
        self.emit_now_playing(None);
        self.emit(Event::Paused(true));
        self.emit_position();
        self.reset_lyrics_session();
        self.update_playing_sources();
        self.emit(Event::AlbumsReset);
        self.emit(Event::TracksReset);
        self.emit(Event::ArtistsReset);
        self.emit(Event::FavoriteAlbums(Vec::new()));
        self.emit(Event::FavoriteTracks(Vec::new()));
        self.emit(Event::FavoriteArtists(Vec::new()));
        self.emit(Event::Playlists(Vec::new()));
        self.emit(Event::PlaylistChoices(Vec::new()));
        self.emit(Event::Queue {
            items: Vec::new(),
            current: None,
        });
        self.emit(Event::SleepTimer(None));
        self.emit(Event::SignedOut(reason));
        self.emit(Event::SavedLogin(SavedLogin {
            server_url: server,
            has_api_token: false,
            has_media_token,
            signing_in: false,
        }));
    }

    pub(super) fn logged_in(&mut self, client: Client, capabilities: Capabilities) {
        self.session_file.set(SERVER_KEY, client.base_url());
        self.list_generation += 1;
        let generation = self.list_generation;
        let storage = FileStorage::open(self.config.data_dir.join("playback.json"));
        let reporter_tx = reporter_channel(&self.internal, generation);
        self.reporter = Some(Reporter::new(
            client.clone(),
            &capabilities,
            Box::new(storage),
            reporter_tx,
        ));
        let browse = capabilities.has(Feature::Browse);
        let features = ServerFeatures {
            browse,
            playlist_artwork: capabilities.has(Feature::PlaylistArtwork),
            transcode: ["mp3", "ogg", "flac"]
                .into_iter()
                .filter(|f| capabilities.can_transcode(f))
                .map(str::to_owned)
                .collect(),
            home: capabilities.has(Feature::Home),
            works: capabilities.has(Feature::Works),
            series: capabilities.has(Feature::Series),
            daily_mixes: capabilities.has(Feature::DailyMixes),
            lyrics_translation: capabilities.has(Feature::LyricsTranslation),
            artist_credit_roles: capabilities.has(Feature::ArtistCreditRoles),
            similar_tracks: capabilities.has(Feature::SimilarTracks),
            similarity_method: crate::radio::SimilarityMethod::from_capability(
                capabilities.similarity_method(),
            ),
            loudness: capabilities.has(Feature::Loudness)
                || capabilities.loudness.as_ref().is_some_and(|l| l.available),
            loudness_median_lufs: capabilities
                .loudness
                .as_ref()
                .and_then(|l| l.library_median_lufs),
            loudness_analyzed: capabilities
                .loudness
                .as_ref()
                .map_or(0, |l| l.analyzed_tracks),
            loudness_total: capabilities.loudness.as_ref().map_or(0, |l| l.total_tracks),
        };
        self.library = Some(Library {
            images: Arc::new(Images::new(client.clone())),
            client: client.clone(),
            capabilities,
            lists: HashMap::new(),
            favorite_tracks: Arc::default(),
        });
        self.emit(Event::LoggedIn {
            server_url: client.base_url().to_owned(),
            features,
        });

        self.restore_queue();
        self.emit_radio_state();
        self.restore_cast();
        self.query_albums(false, AlbumFilter::default());
        self.query_tracks(false, TrackFilter::initial(browse));
        self.load_playlists();
    }
}
