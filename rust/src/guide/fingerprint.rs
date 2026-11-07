//! Secret-free guide cache configuration fingerprints.

use std::fmt;

use reqwest::Url;
use ring::digest::{SHA256, digest};

use crate::config::Config;

use super::cache::CacheError;

/// A stable, secret-free input to the cache fingerprint.
#[derive(Clone, PartialEq, Eq)]
pub struct CacheFingerprint {
    value: String,
}

impl fmt::Debug for CacheFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CacheFingerprint([redacted])")
    }
}

impl CacheFingerprint {
    /// Builds a canonical length-delimited fingerprint from guide configuration.
    ///
    /// # Errors
    /// Returns [`CacheError::InvalidFingerprint`] when the authentication origin is invalid.
    pub fn from_config(config: &Config, epg_mac: Option<&str>) -> Result<Self, CacheError> {
        let origin =
            Url::parse(&config.authentication_url).map_err(|_| CacheError::InvalidFingerprint)?;
        let authority = origin.host_str().ok_or(CacheError::InvalidFingerprint)?;
        if !matches!(origin.scheme(), "http" | "https")
            || origin.port().is_none()
            || !origin.username().is_empty()
        {
            return Err(CacheError::InvalidFingerprint);
        }
        let mac_digest = epg_mac.map(|mac| hex::encode(digest(&SHA256, mac.as_bytes()).as_ref()));
        let parts = [
            config.guide_past_days.to_string(),
            config.guide_future_days.to_string(),
            config.guide_timezone.clone(),
            format!(
                "{}://{}:{}",
                origin.scheme(),
                authority,
                origin.port().unwrap_or(0)
            ),
            config.dhcp_user.as_str().to_owned(),
            config.epg_user.as_str().to_owned(),
            config.stb_id.clone(),
            config.stb_id_prefix.clone(),
            mac_digest.unwrap_or_default(),
        ];
        let mut bytes = Vec::new();
        for part in parts {
            let length = u64::try_from(part.len()).map_err(|_| CacheError::InvalidFingerprint)?;
            bytes.extend_from_slice(&length.to_be_bytes());
            bytes.extend_from_slice(part.as_bytes());
        }
        Ok(Self {
            value: hex::encode(digest(&SHA256, &bytes).as_ref()),
        })
    }

    /// Returns the opaque fingerprint string stored in the sidecar.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}
