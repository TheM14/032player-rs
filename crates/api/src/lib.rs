//! 032 Music Server HTTP client and types. No UI, no mpv.

mod client;
mod error;
mod query;
mod types;

pub use client::{
    API_REVISION, ArtistTrackSort, Client, FavoriteKind, MAX_PAGE_SIZE, Transcode, authenticate,
    authenticate_client, normalize_base_url,
};
pub use error::{ApiError, AuthError};
pub use query::{
    AlbumListQuery, AlbumSort, ArtistListQuery, ArtistRole, ArtistSort, Query, RandomSeeds,
    SortOrder, TrackListQuery, TrackSort, seed_key,
};
pub use reqwest::Method;
pub use types::*;
pub use url::Url;
