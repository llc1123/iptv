use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

use super::*;
use crate::preflight::PythonProcessSpec;

fn fixture_root(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("iptv-gate-{}-{}", std::process::id(), label));
    let _ = fs::remove_dir_all(&path);
    assert!(fs::create_dir_all(&path).is_ok());
    path
}

fn preflight(root: &Path) -> PythonPreflight {
    let identity = PythonProcessSpec::new(
        root.join("python"),
        "iptv-server".to_owned(),
        1000,
        root.to_path_buf(),
        "eth0".to_owned(),
        "127.0.0.1".to_owned(),
        8080,
        PathBuf::from("var/logo-catalog/guide.json"),
        PathBuf::from("var/logo-catalog/catalog.json"),
        root.to_path_buf(),
        vec!["iptv-server".to_owned()],
    );
    PythonPreflight::new(root.to_path_buf(), identity)
}

#[test]
fn startup_gate_refuses_python_before_lock_or_port() {
    let root = fixture_root("ordering");
    let process = root.join("12");
    assert!(fs::create_dir_all(&process).is_ok());
    assert!(fs::write(process.join("cmdline"), b"iptv-server\0").is_ok());
    let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0));
    assert!(occupied.is_ok());
    let occupied = occupied.ok();
    let address = occupied
        .as_ref()
        .and_then(|listener| listener.local_addr().ok());
    assert!(address.is_some());
    let state = root.join("state");
    let result = address.map_or(Err(StartupRefusal::PortConflict), |address| {
        acquire_startup_gate_at(&state, &preflight(&root), address)
    });
    assert!(matches!(result, Err(StartupRefusal::ProcFieldInaccessible)));
    assert!(!state.join("instance.lock").exists());
    drop(occupied);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn startup_gate_releases_lock_when_port_bind_fails() {
    let root = fixture_root("port");
    let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0));
    assert!(occupied.is_ok());
    let occupied = occupied.ok();
    let address = occupied
        .as_ref()
        .and_then(|listener| listener.local_addr().ok());
    assert!(address.is_some());
    let state = root.join("state");
    let result = address.map_or(Err(StartupRefusal::PortConflict), |address| {
        acquire_startup_gate_at(&state, &preflight(&root), address)
    });
    assert!(matches!(result, Err(StartupRefusal::PortConflict)));
    assert!(!state.join("instance.lock").exists());
    drop(occupied);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn startup_gate_maps_lock_records_and_holds_successful_resources() {
    let root = fixture_root("locks");
    let state = root.join("state");
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let gate = acquire_startup_gate_at(&state, &preflight(&root), address);
    assert!(gate.is_ok());
    assert!(state.join("instance.lock").exists());
    let gate = gate.ok();
    if let Some(ref gate) = gate {
        assert_eq!(format!("{gate:?}"), "StartupGate([redacted])");
    }
    let bound = gate
        .as_ref()
        .and_then(|value| value.port().local_addr().ok());
    assert!(bound.is_some());
    assert!(
        bound
            .and_then(|value| TcpListener::bind(value).err())
            .is_some()
    );
    assert!(matches!(
        acquire_startup_gate_at(&state, &preflight(&root), address),
        Err(StartupRefusal::InstanceLockBusy)
    ));
    drop(gate);
    assert!(!state.join("instance.lock").exists());
    assert!(fs::create_dir_all(&state).is_ok());
    assert!(fs::write(state.join("instance.lock"), b"not-json").is_ok());
    assert!(matches!(
        acquire_startup_gate_at(&state, &preflight(&root), address),
        Err(StartupRefusal::InstanceLockMalformed)
    ));
    assert!(
        fs::write(
            state.join("instance.lock"),
            br#"{"version":2,"owner":"other"}"#
        )
        .is_ok()
    );
    assert!(matches!(
        acquire_startup_gate_at(&state, &preflight(&root), address),
        Err(StartupRefusal::InstanceLockMismatch)
    ));
    let _ = fs::remove_dir_all(root);
}
