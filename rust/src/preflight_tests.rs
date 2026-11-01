use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use super::*;

fn fixture_root(label: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("iptv-preflight-{}-{}", std::process::id(), label));
    let _ = fs::remove_dir_all(&path);
    assert!(fs::create_dir_all(&path).is_ok());
    path
}

fn write_process(root: &Path, pid: &str, argv: &[&str], uid: &str, interpreter: &Path, cwd: &Path) {
    let process = root.join(pid);
    assert!(fs::create_dir_all(&process).is_ok());
    let mut cmdline = argv.join("\0").into_bytes();
    cmdline.push(0);
    assert!(fs::write(process.join("cmdline"), cmdline).is_ok());
    assert!(
        fs::write(
            process.join("status"),
            format!("Name:\tpython\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
        )
        .is_ok()
    );
    assert!(symlink(interpreter, process.join("exe")).is_ok());
    assert!(symlink(cwd, process.join("cwd")).is_ok());
}

fn spec(interpreter: &Path, cwd: &Path) -> PythonProcessSpec {
    PythonProcessSpec::new(
        fs::canonicalize(interpreter).unwrap_or_else(|_| interpreter.to_path_buf()),
        "iptv-server".to_owned(),
        1000,
        fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf()),
        "eth0".to_owned(),
        "127.0.0.1".to_owned(),
        8080,
        PathBuf::from(DEFAULT_GUIDE_CACHE),
        PathBuf::from(DEFAULT_LOGO_CATALOG),
        fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf()),
        vec![
            "iptv-server".to_owned(),
            "--port".to_owned(),
            "8080".to_owned(),
        ],
    )
}

#[test]
fn exact_candidate_is_conflict() {
    let root = fixture_root("exact");
    let interpreter = root.join("python");
    let cwd = root.join("cwd");
    assert!(fs::write(&interpreter, b"python").is_ok());
    assert!(fs::create_dir_all(&cwd).is_ok());
    write_process(
        &root,
        "12",
        &["iptv-server", "--port", "8080"],
        "1000",
        &interpreter,
        &cwd,
    );
    let result = PythonPreflight::new(root.clone(), spec(&interpreter, &cwd)).inspect();
    assert!(matches!(result, Err(StartupRefusal::PythonConflict)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn mismatch_wrapper_inaccessible_invalid_utf8_multiple_and_empty_are_distinct() {
    let root = fixture_root("table");
    let interpreter = root.join("python");
    let cwd = root.join("cwd");
    assert!(fs::write(&interpreter, b"python").is_ok());
    assert!(fs::create_dir_all(&cwd).is_ok());
    let expected = spec(&interpreter, &cwd);
    write_process(
        &root,
        "12",
        &["iptv-server", "--port", "8081"],
        "1000",
        &interpreter,
        &cwd,
    );
    assert_eq!(
        PythonPreflight::new(root.clone(), expected.clone()).inspect(),
        Err(StartupRefusal::PythonIdentityMismatch)
    );
    let _ = fs::remove_dir_all(root.join("12"));
    write_process(
        &root,
        "13",
        &["uv", "run", "iptv-server"],
        "1000",
        &interpreter,
        &cwd,
    );
    assert_eq!(
        PythonPreflight::new(root.clone(), expected.clone()).inspect(),
        Err(StartupRefusal::WrapperMismatch)
    );
    let _ = fs::remove_dir_all(root.join("13"));
    write_process(
        &root,
        "14",
        &["iptv-server", "--port", "8080"],
        "1000",
        &interpreter,
        &cwd,
    );
    let _ = fs::remove_file(root.join("14/status"));
    assert_eq!(
        PythonPreflight::new(root.clone(), expected.clone()).inspect(),
        Err(StartupRefusal::ProcFieldInaccessible)
    );
    let _ = fs::remove_dir_all(root.join("14"));
    write_process(
        &root,
        "17",
        &["iptv-server", "--port", "8080"],
        "1000",
        &interpreter,
        &cwd,
    );
    assert!(fs::write(root.join("17/cmdline"), [0xff_u8, 0]).is_ok());
    assert_eq!(
        PythonPreflight::new(root.clone(), expected.clone()).inspect(),
        Err(StartupRefusal::ProcFieldInaccessible)
    );
    let _ = fs::remove_dir_all(root.join("17"));
    write_process(
        &root,
        "15",
        &["iptv-server", "--port", "8080"],
        "1000",
        &interpreter,
        &cwd,
    );
    write_process(
        &root,
        "16",
        &["iptv-server", "--port", "8081"],
        "1000",
        &interpreter,
        &cwd,
    );
    assert_eq!(
        PythonPreflight::new(root.clone(), expected.clone()).inspect(),
        Err(StartupRefusal::MultiplePythonCandidates)
    );
    let _ = fs::remove_dir_all(root);
    let empty = fixture_root("empty");
    assert_eq!(
        PythonPreflight::new(empty.clone(), expected).inspect(),
        Ok(())
    );
    let _ = fs::remove_dir_all(empty);
}

#[test]
fn public_debug_for_identity_is_redacted() {
    let root = fixture_root("debug");
    let interpreter = root.join("python");
    let cwd = root.join("cwd");
    let identity = spec(&interpreter, &cwd);
    assert_eq!(format!("{identity:?}"), "PythonProcessSpec([redacted])");
    let preflight = PythonPreflight::new(root.clone(), identity);
    assert_eq!(format!("{preflight:?}"), "PythonPreflight([redacted])");
    let _ = fs::remove_dir_all(root);
}
