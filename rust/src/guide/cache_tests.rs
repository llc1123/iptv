use std::{fs, io, path::Path, time::Duration};

use super::*;

fn fingerprint() -> CacheFingerprint {
    CacheFingerprint::for_test("fingerprint")
}

fn programme() -> Programme {
    Programme {
        title: "Morning & <News>".to_owned(),
        start_time_ms: 1_699_971_200_000,
        end_time_ms: 1_699_974_800_000,
        programme_id: "prog/1".to_owned(),
        channel_id: "c1".to_owned(),
    }
}

#[test]
fn serializes_python_cache_bytes_without_ascii_escaping() {
    let actual = GuideCache::serialize(&[programme()]).unwrap();
    let expected = "[\n  {\n    \"name\": \"Morning & <News>\",\n    \"startTime\": 1699971200000,\n    \"endTime\": 1699974800000,\n    \"ID\": \"prog/1\",\n    \"channelID\": \"c1\"\n  }\n]\n";
    assert_eq!(String::from_utf8(actual).unwrap(), expected);
}

#[test]
fn invalid_future_cache_is_not_served() {
    let root = std::env::temp_dir().join(format!("guide-cache-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let cache = GuideCache::new(root.join("guide.json"));
    cache
        .save(&[programme()], 2_000, &fingerprint(), &NeverCancelled)
        .unwrap();
    assert_eq!(
        cache.load(1_000, Duration::from_secs(10), &fingerprint()),
        CacheState::Invalid(CacheInvalidReason::FutureTime)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn age_boundary_is_fresh_and_next_millisecond_is_stale() {
    let root = std::env::temp_dir().join(format!("guide-cache-age-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let cache = GuideCache::new(root.join("guide.json"));
    cache
        .save(&[programme()], 1_000, &fingerprint(), &NeverCancelled)
        .unwrap();
    assert!(matches!(
        cache.load(3_000, Duration::from_secs(2), &fingerprint()),
        CacheState::Fresh { age_ms: 2_000, .. }
    ));
    assert!(matches!(
        cache.load(3_001, Duration::from_secs(2), &fingerprint()),
        CacheState::Stale { age_ms: 2_001 }
    ));
    fs::remove_dir_all(root).unwrap();
}

struct Cancelled;

impl Cancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

struct FailBeforeRename;

impl AtomicWriteHooks for FailBeforeRename {
    fn before_rename(&self) -> io::Result<()> {
        Err(io::Error::other("injected rename failure"))
    }
}

#[test]
fn cancellation_and_injected_failure_leave_no_temporary_file() {
    let root = std::env::temp_dir().join(format!("guide-cache-failure-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let cache = GuideCache::new(root.join("guide.json"));
    assert!(matches!(
        cache.save(&[programme()], 1, &fingerprint(), &Cancelled),
        Err(CacheError::Cancelled)
    ));
    assert!(matches!(
        cache.save_with_hooks(
            &[programme()],
            1,
            &fingerprint(),
            &NeverCancelled,
            &FailBeforeRename
        ),
        Err(CacheError::Io(_))
    ));
    let entries = fs::read_dir(&root)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(entries.is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn relative_cache_path_syncs_the_current_directory() {
    let path = format!("guide-cache-current-dir-{}.json", std::process::id());
    let metadata_path = format!("{path}.meta.json");
    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(&metadata_path);
    let cache = GuideCache::new(path.clone().into());

    cache
        .save(&[programme()], 1_000, &fingerprint(), &NeverCancelled)
        .unwrap();

    assert!(Path::new(&path).is_file());
    assert!(Path::new(&metadata_path).is_file());
    fs::remove_file(path).unwrap();
    fs::remove_file(metadata_path).unwrap();
}
