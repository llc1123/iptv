//! Operator-maintained channel logos: `<logo-dir>/<channel-id>.png`.

use std::path::{Path, PathBuf};

/// The logo file for a channel, when one exists. Channel IDs are limited to
/// `[A-Za-z0-9_-]`, so a request cannot name a file outside `dir`.
pub fn logo_path(dir: &Path, channel_id: &str) -> Option<PathBuf> {
    let valid = !channel_id.is_empty()
        && channel_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    let path = dir.join(format!("{channel_id}.png"));
    (valid && path.is_file()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_is_the_png_named_by_a_plain_channel_id() {
        let dir = std::env::temp_dir().join(format!("iptv-logos-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("c1.png"), b"png").unwrap();
        std::fs::write(dir.join("c2.jpg"), b"jpg").unwrap();
        assert_eq!(logo_path(&dir, "c1"), Some(dir.join("c1.png")));
        assert_eq!(logo_path(&dir, "c2"), None);
        assert_eq!(logo_path(&dir, "missing"), None);
        assert_eq!(logo_path(&dir.join("sub"), "../c1"), None);
        assert_eq!(logo_path(&dir, ""), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
