//! Cancellation, request draining, instance ownership, and port binding.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, watch};

/// Maximum graceful shutdown period accepted by the lifecycle core.
pub const MAX_SHUTDOWN_GRACE: Duration = Duration::from_secs(60);

/// A source that can broadcast one-way cancellation to any number of tokens.
#[derive(Debug, Clone)]
pub struct CancellationSource {
    sender: watch::Sender<bool>,
}

impl Default for CancellationSource {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationSource {
    /// Creates a non-cancelled source.
    #[must_use]
    pub fn new() -> Self {
        let (sender, _) = watch::channel(false);
        Self { sender }
    }

    /// Creates a token subscribed to this source.
    #[must_use]
    pub fn token(&self) -> CancellationToken {
        CancellationToken {
            receiver: self.sender.subscribe(),
        }
    }

    /// Marks this source cancelled. Repeated calls are harmless.
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }
}

/// A cloneable view of a [`CancellationSource`].
#[derive(Debug, Clone)]
pub struct CancellationToken {
    receiver: watch::Receiver<bool>,
}

impl CancellationToken {
    /// Waits until cancellation is observed.
    pub async fn cancelled(&self) {
        let mut receiver = self.receiver.clone();
        if *receiver.borrow() {
            return;
        }
        while receiver.changed().await.is_ok() {
            if *receiver.borrow() {
                return;
            }
        }
    }

    /// Returns whether cancellation has already been observed.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }
}

/// Why a request could not be admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionError {
    /// Shutdown has begun and new requests are refused.
    Draining,
}

/// The result of waiting for active requests to finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainResult {
    /// All active requests finished within the effective grace period.
    Drained,
    /// At least one request remained active when grace expired.
    TimedOut,
}

/// Shared shutdown and request admission state.
#[derive(Debug, Clone)]
pub struct Lifecycle {
    inner: Arc<LifecycleInner>,
}

#[derive(Debug)]
struct LifecycleInner {
    active: AtomicUsize,
    draining: AtomicBool,
    changed: Notify,
    cancellation: CancellationSource,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl Lifecycle {
    /// Creates an accepting lifecycle.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(LifecycleInner {
                active: AtomicUsize::new(0),
                draining: AtomicBool::new(false),
                changed: Notify::new(),
                cancellation: CancellationSource::new(),
            }),
        }
    }

    /// Returns a token cancelled when shutdown begins.
    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.inner.cancellation.token()
    }

    /// Attempts to admit one request, closing the race with shutdown.
    ///
    /// # Errors
    /// Returns [`AdmissionError::Draining`] when shutdown has begun.
    pub fn try_enter_request(&self) -> Result<RequestGuard, AdmissionError> {
        if self.inner.draining.load(Ordering::Acquire) {
            return Err(AdmissionError::Draining);
        }
        self.inner.active.fetch_add(1, Ordering::AcqRel);
        if self.inner.draining.load(Ordering::Acquire) {
            self.inner.active.fetch_sub(1, Ordering::AcqRel);
            self.inner.changed.notify_waiters();
            return Err(AdmissionError::Draining);
        }
        Ok(RequestGuard {
            inner: Arc::clone(&self.inner),
        })
    }

    /// Begins shutdown, refusing admissions and waking cancellation waiters.
    pub fn begin_shutdown(&self) {
        if !self.inner.draining.swap(true, Ordering::AcqRel) {
            self.inner.cancellation.cancel();
            self.inner.changed.notify_waiters();
        }
    }

    /// Returns whether new requests are currently refused.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.inner.draining.load(Ordering::Acquire)
    }

    /// Returns the number of admitted requests not yet dropped.
    #[must_use]
    pub fn active_requests(&self) -> usize {
        self.inner.active.load(Ordering::Acquire)
    }

    /// Begins shutdown and waits for active requests, capped at 60 seconds.
    pub async fn drain(&self, grace: Duration) -> DrainResult {
        self.begin_shutdown();
        let effective = effective_grace(grace);
        let wait = async {
            loop {
                let notified = self.inner.changed.notified();
                if self.active_requests() == 0 {
                    return;
                }
                notified.await;
            }
        };
        if tokio::time::timeout(effective, wait).await.is_ok() {
            DrainResult::Drained
        } else {
            DrainResult::TimedOut
        }
    }
}

fn effective_grace(grace: Duration) -> Duration {
    grace.min(MAX_SHUTDOWN_GRACE)
}

/// An admitted request. Dropping it releases one active request slot.
#[derive(Debug)]
pub struct RequestGuard {
    inner: Arc<LifecycleInner>,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.inner.active.fetch_sub(1, Ordering::AcqRel);
        self.inner.changed.notify_waiters();
    }
}

const LOCK_VERSION: u8 = 1;
const LOCK_OWNER: &str = "iptv-media-server";

#[derive(Debug, Serialize, Deserialize)]
struct LockRecord {
    version: u8,
    owner: String,
}

/// Errors acquiring or validating the instance lock.
#[derive(Debug, thiserror::Error)]
pub enum InstanceLockError {
    /// The lock path is already occupied by a valid matching record.
    #[error("instance lock is already held")]
    AlreadyHeld,
    /// The existing lock record is not valid JSON for this lock format.
    #[error("instance lock record is malformed")]
    Malformed,
    /// The existing lock record belongs to another lock format or owner.
    #[error("instance lock record does not match")]
    Mismatched,
    /// The state directory or lock file could not be accessed.
    #[error("instance lock could not be accessed")]
    Io(#[source] io::Error),
    /// The lock record could not be encoded.
    #[error("instance lock record could not be written")]
    Encode(#[source] serde_json::Error),
}

/// An owned instance lock whose file handle remains open for its lifetime.
#[derive(Debug)]
pub struct InstanceLock {
    path: PathBuf,
    file: File,
}

impl InstanceLock {
    /// Acquires `instance.lock` below `state_dir` using atomic create-new.
    ///
    /// # Errors
    /// Returns an [`InstanceLockError`] when the record is occupied, invalid,
    /// or cannot be created.
    pub fn create_new(state_dir: impl AsRef<Path>) -> Result<Self, InstanceLockError> {
        let state_dir = state_dir.as_ref();
        fs::create_dir_all(state_dir).map_err(InstanceLockError::Io)?;
        let path = state_dir.join("instance.lock");
        let record = serde_json::to_vec(&LockRecord {
            version: LOCK_VERSION,
            owner: LOCK_OWNER.to_owned(),
        })
        .map_err(InstanceLockError::Encode)?;
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(&record).map_err(InstanceLockError::Io)?;
                file.sync_all().map_err(InstanceLockError::Io)?;
                Ok(Self { path, file })
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(classify_existing_lock(&path))
            }
            Err(error) => Err(InstanceLockError::Io(error)),
        }
    }

    /// Returns the lock file path without exposing its contents.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the retained lock file handle for ownership-sensitive callers.
    #[must_use]
    pub fn file(&self) -> &File {
        &self.file
    }
}

fn classify_existing_lock(path: &Path) -> InstanceLockError {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => return InstanceLockError::Io(error),
    };
    let Ok(record) = serde_json::from_slice::<LockRecord>(&bytes) else {
        return InstanceLockError::Malformed;
    };
    if record.version != LOCK_VERSION || record.owner != LOCK_OWNER {
        return InstanceLockError::Mismatched;
    }
    InstanceLockError::AlreadyHeld
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Errors acquiring the HTTP listener.
#[derive(Debug, thiserror::Error)]
pub enum PortGuardError {
    /// The requested address is already occupied or otherwise unavailable.
    #[error("port could not be bound")]
    Bind(#[source] io::Error),
}

/// A held TCP listener that reserves a port for later HTTP use.
#[derive(Debug)]
pub struct PortGuard {
    listener: TcpListener,
}

impl PortGuard {
    /// Binds and retains a listener; no disposable probe is used.
    ///
    /// # Errors
    /// Returns [`PortGuardError::Bind`] when the address cannot be held.
    pub fn bind(address: SocketAddr) -> Result<Self, PortGuardError> {
        TcpListener::bind(address)
            .map(|listener| Self { listener })
            .map_err(PortGuardError::Bind)
    }

    /// Returns the bound local address.
    ///
    /// # Errors
    /// Returns the operating-system error when the listener address cannot be
    /// read.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Borrows the listener for integration with a later HTTP server.
    #[must_use]
    pub fn listener(&self) -> &TcpListener {
        &self.listener
    }

    /// Transfers ownership of the listener to the later HTTP server.
    #[must_use]
    pub fn into_listener(self) -> TcpListener {
        self.listener
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::*;

    fn unused_state_dir() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "iptv-lifecycle-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[tokio::test]
    async fn cancellation_wakes_waiters_and_is_idempotent() {
        let source = CancellationSource::new();
        let token = source.token();
        let waiter_token = token.clone();
        let waiter = tokio::spawn(async move { waiter_token.cancelled().await });
        source.cancel();
        let result = waiter.await;
        assert!(result.is_ok());
        assert!(source.token().is_cancelled());
        source.cancel();
    }

    #[tokio::test]
    async fn lifecycle_token_is_cancelled_when_subscribed_after_shutdown() {
        let lifecycle = Lifecycle::new();

        lifecycle.begin_shutdown();

        assert!(lifecycle.cancellation_token().is_cancelled());
    }

    #[tokio::test]
    async fn shutdown_refuses_admission_and_drains() {
        let lifecycle = Lifecycle::new();
        let request = lifecycle.try_enter_request();
        assert!(request.is_ok());
        lifecycle.begin_shutdown();
        assert!(matches!(
            lifecycle.try_enter_request(),
            Err(AdmissionError::Draining)
        ));
        drop(request);
        assert_eq!(
            lifecycle.drain(Duration::from_secs(1)).await,
            DrainResult::Drained
        );
    }

    #[tokio::test]
    async fn drain_times_out_and_caps_grace() {
        let lifecycle = Lifecycle::new();
        let request = lifecycle.try_enter_request();
        assert!(request.is_ok());
        assert_eq!(effective_grace(Duration::from_secs(61)), MAX_SHUTDOWN_GRACE);
        assert_eq!(lifecycle.drain(Duration::ZERO).await, DrainResult::TimedOut);
        drop(request);
    }

    #[tokio::test]
    async fn drain_observes_guard_drop_after_waiter_registration() {
        let lifecycle = Lifecycle::new();
        let request = lifecycle.try_enter_request();
        assert!(request.is_ok());
        let request = request.ok();
        lifecycle.begin_shutdown();

        let notified = lifecycle.inner.changed.notified();
        drop(request);

        assert_eq!(lifecycle.active_requests(), 0);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), notified)
                .await
                .is_ok()
        );
    }

    #[test]
    fn concurrent_lock_attempts_and_drop_reacquire_are_exclusive() {
        let state_dir = unused_state_dir();
        let barrier = Arc::new(Barrier::new(2));
        let results = std::thread::scope(|scope| {
            let first_barrier = Arc::clone(&barrier);
            let first_dir = state_dir.clone();
            let first = scope.spawn(move || {
                if let Ok(lock) = InstanceLock::create_new(first_dir) {
                    first_barrier.wait();
                    drop(lock);
                    true
                } else {
                    first_barrier.wait();
                    false
                }
            });
            let second_barrier = Arc::clone(&barrier);
            let second_dir = state_dir.clone();
            let second = scope.spawn(move || {
                if let Ok(lock) = InstanceLock::create_new(second_dir) {
                    second_barrier.wait();
                    drop(lock);
                    true
                } else {
                    second_barrier.wait();
                    false
                }
            });
            (first.join(), second.join())
        });
        assert!(results.0.is_ok());
        assert!(results.1.is_ok());
        if let (Ok(first), Ok(second)) = results {
            assert_ne!(first, second);
        }
        let reacquired = InstanceLock::create_new(&state_dir);
        assert!(reacquired.is_ok());
        drop(reacquired);
        let _ = fs::remove_dir_all(state_dir);
    }

    #[test]
    fn malformed_and_mismatched_records_are_refused() {
        let malformed_dir = unused_state_dir();
        assert!(fs::create_dir_all(&malformed_dir).is_ok());
        assert!(fs::write(malformed_dir.join("instance.lock"), b"not-json").is_ok());
        assert!(matches!(
            InstanceLock::create_new(&malformed_dir),
            Err(InstanceLockError::Malformed)
        ));
        let _ = fs::remove_dir_all(&malformed_dir);

        let mismatched_dir = unused_state_dir();
        assert!(fs::create_dir_all(&mismatched_dir).is_ok());
        assert!(
            fs::write(
                mismatched_dir.join("instance.lock"),
                br#"{"version":2,"owner":"other"}"#
            )
            .is_ok()
        );
        assert!(matches!(
            InstanceLock::create_new(&mismatched_dir),
            Err(InstanceLockError::Mismatched)
        ));
        let _ = fs::remove_dir_all(mismatched_dir);
    }

    #[test]
    fn port_guard_holds_port_until_drop() {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let guard = PortGuard::bind(address);
        assert!(guard.is_ok());
        let guard = guard.ok();
        let bound = guard.as_ref().and_then(|value| value.local_addr().ok());
        assert!(bound.is_some());
        let occupied = bound.and_then(|value| TcpListener::bind(value).err());
        assert!(occupied.is_some());
        drop(guard);
    }
}
