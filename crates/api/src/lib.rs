//! 032 Music Server HTTP client and types. No UI, no mpv.

mod client;
mod error;
mod m032;
mod query;
mod types;
mod update;

pub use client::{
    API_REVISION, ArtistTrackSort, Client, FavoriteKind, MAX_PAGE_SIZE, SimilarTracksOptions,
    Transcode, authenticate, authenticate_client, normalize_base_url, redact_media_token,
};
pub use error::{ApiError, AuthError};
pub use m032::*;
pub use query::{
    AlbumListQuery, AlbumSort, ArtistListQuery, ArtistRole, ArtistSort, Query, RandomSeeds,
    SortOrder, TrackListQuery, TrackSort, seed_key,
};
pub use reqwest::Method;
pub use types::*;
pub use update::{GITHUB_API, RELEASE_REPO, Release, is_newer, latest_release};
pub use url::Url;
