//! Bounded HLS resource state and upstream admission policy.

pub mod playlist;
pub mod tokens;

#[derive(Clone, Debug, Default)]
pub struct RequestHeaders {
    pub accept: Option<String>,
    pub range: Option<String>,
    pub user_agent: Option<String>,
}

pub use playlist::{
    PLAYLIST_MAX_BYTES, PLAYLIST_MAX_LINE_BYTES, PLAYLIST_MAX_URI_REFERENCES, PlaylistRewrite,
    rewrite_playlist,
};
pub use tokens::{TokenStore, TokenStoreError};
