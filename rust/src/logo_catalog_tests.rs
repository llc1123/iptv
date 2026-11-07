use super::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("iptv-logo-catalog-{id}-{}", std::process::id()));
        fs::create_dir_all(path.join("logos")).expect("fixture root");
        Self(path)
    }
    fn catalog(&self, body: &str) -> PathBuf {
        let path = self.0.join("catalog.json");
        fs::write(&path, body).expect("catalog");
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap_or(());
    }
}

fn valid_catalog(path: &str) -> String {
    let path = path.replace('\\', "\\\\");
    format!(
        r#"{{"version":1,"channels":[{{"channel_id":"c1","stream_url":"s","display_name":"One","logo_path":"{path}"}}]}}"#
    )
}

#[test]
fn valid_catalog_reads_bounded_bytes_and_content_type() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("logos/one.PNG"), b"png-bytes").expect("logo");
    let catalog =
        LogoCatalog::load(fixture.catalog(&valid_catalog("logos/one.PNG"))).expect("valid catalog");
    let asset = catalog.read("c1").expect("read").expect("asset");
    assert_eq!(asset.bytes, b"png-bytes");
    assert_eq!(asset.content_type, "image/png");
    assert_eq!(asset.cache_control, "public, max-age=86400");
    assert!(catalog.read("unknown").expect("unknown id").is_none());
}

#[test]
fn missing_and_duplicate_entries_follow_python_compatibility() {
    let fixture = Fixture::new();
    let body = r#"{"version":1,"channels":[{"channel_id":"c1","stream_url":"first","display_name":"First"},{"channel_id":"c1","stream_url":"last","display_name":"Last","logo_path":"logos/missing.jpg"}]}"#;
    let catalog = LogoCatalog::load(fixture.catalog(body)).expect("duplicate is last-wins");
    assert!(catalog.read("c1").expect("missing logo").is_none());
    assert!(
        LogoCatalog::load_if_present(fixture.0.join("missing.json"))
            .expect("optional catalog")
            .is_none()
    );
}

#[test]
fn required_logo_metadata_fields_are_invalid_schema_when_missing() {
    let fixture = Fixture::new();
    for body in [
        r#"{"version":1,"channels":[{"channel_id":"c1","display_name":"One"}]}"#,
        r#"{"version":1,"channels":[{"channel_id":"c1","stream_url":"s"}]}"#,
    ] {
        assert!(matches!(
            LogoCatalog::load(fixture.catalog(body)),
            Err(LogoCatalogError::InvalidSchema)
        ));
    }
}

#[test]
fn malformed_schema_version_and_limits_are_fixed_failures() {
    let fixture = Fixture::new();
    assert!(matches!(
        LogoCatalog::load(fixture.catalog("{")),
        Err(LogoCatalogError::MalformedJson)
    ));
    assert!(matches!(
        LogoCatalog::load(fixture.catalog(r"[]")),
        Err(LogoCatalogError::InvalidSchema)
    ));
    assert!(matches!(
        LogoCatalog::load(fixture.catalog(r#"{"version":"1","channels":[]}"#)),
        Err(LogoCatalogError::InvalidSchema)
    ));
    assert!(matches!(
        LogoCatalog::load(fixture.catalog(r#"{"version":1,"channels":{}}"#)),
        Err(LogoCatalogError::InvalidSchema)
    ));
    assert!(matches!(
        LogoCatalog::load(fixture.catalog(r#"{"version":2,"channels":[]}"#)),
        Err(LogoCatalogError::UnsupportedVersion)
    ));
    assert!(matches!(
        LogoCatalog::load(fixture.catalog(r#"{"version":1,"raw_m3u":"","channels":[]}"#)),
        Err(LogoCatalogError::InvalidSchema)
    ));
    let limits = LogoCatalogLimits {
        max_catalog_bytes: 1,
        ..LogoCatalogLimits::default()
    };
    assert!(matches!(
        LogoCatalog::load_with_limits(fixture.catalog(&valid_catalog("logos/a.png")), limits),
        Err(LogoCatalogError::CatalogTooLarge)
    ));
}

#[test]
fn traversal_absolute_encoded_and_normalized_paths_are_rejected() {
    let fixture = Fixture::new();
    for path in [
        "../x.png",
        "/tmp/x.png",
        "logos/../x.png",
        "logos/%2e%2e/x.png",
        "logos\\x.png",
    ] {
        assert!(matches!(
            LogoCatalog::load(fixture.catalog(&valid_catalog(path))),
            Err(LogoCatalogError::UnsafePath)
        ));
    }
}

#[test]
fn unsupported_extension_non_regular_and_oversize_files_are_rejected() {
    let fixture = Fixture::new();
    assert!(matches!(
        LogoCatalog::load(fixture.catalog(&valid_catalog("logos/logo.txt"))),
        Err(LogoCatalogError::UnsupportedExtension)
    ));
    fs::create_dir(fixture.0.join("logos/dir.png")).expect("directory");
    let catalog =
        LogoCatalog::load(fixture.catalog(&valid_catalog("logos/dir.png"))).expect("path schema");
    assert_eq!(catalog.read("c1"), Err(LogoCatalogError::Unavailable));
    fs::write(fixture.0.join("logos/big.png"), b"12345").expect("large logo");
    let catalog = LogoCatalog::load_with_limits(
        fixture.catalog(&valid_catalog("logos/big.png")),
        LogoCatalogLimits {
            max_file_bytes: 4,
            ..LogoCatalogLimits::default()
        },
    )
    .expect("catalog");
    assert_eq!(catalog.read("c1"), Err(LogoCatalogError::FileTooLarge));
}

#[cfg(unix)]
#[test]
fn symlinked_root_and_entry_are_rejected_without_following() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fs::write(fixture.0.join("outside.png"), b"outside").expect("outside");
    symlink(
        fixture.0.join("outside.png"),
        fixture.0.join("logos/link.png"),
    )
    .expect("entry symlink");
    assert!(matches!(
        LogoCatalog::load(fixture.catalog(&valid_catalog("logos/link.png"))),
        Err(LogoCatalogError::UnsafePath)
    ));
    let root = fixture.0.join("linked-root");
    fs::create_dir(fixture.0.join("real-root")).expect("real root");
    symlink(fixture.0.join("real-root"), &root).expect("root symlink");
    assert!(matches!(
        LogoCatalog::load(root.join("catalog.json")),
        Err(LogoCatalogError::UnsafeRoot)
    ));
}

#[test]
fn range_reads_are_bounded_and_invalid_ranges_do_not_leak_bytes() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("logos/one.jpg"), b"abcdef").expect("logo");
    let catalog =
        LogoCatalog::load(fixture.catalog(&valid_catalog("logos/one.jpg"))).expect("catalog");
    assert_eq!(
        catalog
            .read_range("c1", Some((1, 3)))
            .expect("range")
            .expect("asset")
            .bytes,
        b"bcd"
    );
    assert_eq!(
        catalog.read_range("c1", Some((3, 1))),
        Err(LogoCatalogError::InvalidRange)
    );
}

#[test]
fn has_logo_requires_a_served_file() {
    let fixture = Fixture::new();
    let catalog =
        LogoCatalog::load(fixture.catalog(&valid_catalog("logos/one.png"))).expect("valid catalog");
    assert!(!catalog.has_logo("c1"));
    fs::write(fixture.0.join("logos/one.png"), b"png").expect("logo");
    assert!(catalog.has_logo("c1"));
    assert!(!catalog.has_logo("c2"));
}
