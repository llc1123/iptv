use crate::epg::Programme;
use serde_json::Value;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct GuideCache {
    path: PathBuf,
}

impl GuideCache {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Option<Vec<Programme>> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(%error, "cannot read guide cache");
                return None;
            }
        };
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "guide cache is invalid JSON");
                return None;
            }
        };
        if !value.is_array() {
            tracing::warn!("guide cache is not a JSON list");
            return None;
        }
        match serde_json::from_value(value) {
            Ok(programmes) => Some(programmes),
            Err(error) => {
                tracing::warn!(%error, "guide cache programme is invalid");
                None
            }
        }
    }

    pub fn serialize(programmes: &[Programme]) -> Result<Vec<u8>, serde_json::Error> {
        let mut bytes = serde_json::to_vec_pretty(programmes)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn save(&self, programmes: &[Programme]) -> io::Result<()> {
        let bytes = Self::serialize(programmes).map_err(io::Error::other)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, &self.path)
    }
}
