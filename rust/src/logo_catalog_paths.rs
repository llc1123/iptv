use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use super::{CatalogEntry, LogoCatalogError};

pub(super) fn regular_metadata(path: &Path) -> io::Result<Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    Ok(metadata)
}

pub(super) fn safe_root(path: &Path) -> Result<PathBuf, LogoCatalogError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| LogoCatalogError::UnsafeRoot)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LogoCatalogError::UnsafeRoot);
    }
    fs::canonicalize(path).map_err(|_| LogoCatalogError::UnsafeRoot)
}

pub(super) fn validate_entry(entry: &CatalogEntry, root: &Path) -> Result<(), LogoCatalogError> {
    let fields = [
        &entry.channel_id,
        &entry.stream_url,
        &entry.display_name,
        &entry.tvg_id,
        &entry.tvg_name,
        &entry.tvg_logo,
        &entry.group_title,
        &entry.logo_path,
    ];
    if entry.channel_id.is_empty()
        || fields
            .iter()
            .any(|field| field.chars().any(char::is_control))
    {
        return Err(LogoCatalogError::InvalidEntry);
    }
    if entry.logo_path.is_empty() {
        return Ok(());
    }
    validate_relative_path(&entry.logo_path)?;
    reject_symlink_components(root, &entry.logo_path)?;
    let path = root.join(&entry.logo_path);
    if let Ok(resolved) = fs::canonicalize(&path) {
        if !resolved.starts_with(root) {
            return Err(LogoCatalogError::UnsafePath);
        }
    }
    content_type(&entry.logo_path).ok_or(LogoCatalogError::UnsupportedExtension)?;
    Ok(())
}

pub(super) fn validate_relative_path(value: &str) -> Result<(), LogoCatalogError> {
    if value.is_empty()
        || value.contains('%')
        || value.contains("//")
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return Err(LogoCatalogError::UnsafePath);
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(LogoCatalogError::UnsafePath);
    }
    Ok(())
}

pub(super) fn reject_symlink_components(
    root: &Path,
    relative: &str,
) -> Result<(), LogoCatalogError> {
    let mut current = root.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(name) = component else {
            return Err(LogoCatalogError::UnsafePath);
        };
        current.push(name);
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() {
                return Err(LogoCatalogError::UnsafePath);
            }
        }
    }
    Ok(())
}

pub(super) fn content_type(path: &str) -> Option<&'static str> {
    let extension = Path::new(path).extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("gif") {
        Some("image/gif")
    } else if extension.eq_ignore_ascii_case("jpeg") || extension.eq_ignore_ascii_case("jpg") {
        Some("image/jpeg")
    } else if extension.eq_ignore_ascii_case("png") {
        Some("image/png")
    } else if extension.eq_ignore_ascii_case("svg") {
        Some("image/svg+xml")
    } else if extension.eq_ignore_ascii_case("webp") {
        Some("image/webp")
    } else {
        None
    }
}

pub(super) fn read_bounded<E: Copy>(path: &Path, limit: usize, error: E) -> Result<Vec<u8>, E> {
    let file = File::open(path).map_err(|_| error)?;
    let capacity = limit.checked_add(1).ok_or(error)?;
    let mut bytes = Vec::with_capacity(capacity.min(8192));
    file.take(u64::try_from(capacity).map_err(|_| error)?)
        .read_to_end(&mut bytes)
        .map_err(|_| error)?;
    if bytes.len() > limit {
        Err(error)
    } else {
        Ok(bytes)
    }
}
