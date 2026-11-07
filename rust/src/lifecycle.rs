use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use tokio::sync::{Notify, watch};

pub const MAX_SHUTDOWN_GRACE: Duration = Duration::from_secs(60);

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
    pub fn new() -> Self {
        let (sender, _) = watch::channel(false);
        Self { sender }
    }
    pub fn token(&self) -> CancellationToken {
        CancellationToken {
            receiver: self.sender.subscribe(),
        }
    }
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }
}

#[derive(Debug, Clone)]
pub struct CancellationToken {
    receiver: watch::Receiver<bool>,
}
impl CancellationToken {
    pub async fn cancelled(&self) {
        let mut receiver = self.receiver.clone();
        while !*receiver.borrow() && receiver.changed().await.is_ok() {}
    }
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionError {
    Draining,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainResult {
    Drained,
    TimedOut,
}

#[derive(Debug, Clone)]
pub struct Lifecycle {
    inner: Arc<Inner>,
}
#[derive(Debug)]
struct Inner {
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
    pub fn new() -> Self {
        Self::with_cancellation(CancellationSource::new())
    }
    pub fn with_cancellation(cancellation: CancellationSource) -> Self {
        Self {
            inner: Arc::new(Inner {
                active: AtomicUsize::new(0),
                draining: AtomicBool::new(false),
                changed: Notify::new(),
                cancellation,
            }),
        }
    }
    pub fn cancellation_source(&self) -> CancellationSource {
        self.inner.cancellation.clone()
    }
    pub fn cancellation_token(&self) -> CancellationToken {
        self.inner.cancellation.token()
    }
    pub fn try_enter_request(&self) -> Result<RequestGuard, AdmissionError> {
        if self.inner.draining.load(Ordering::Acquire) {
            return Err(AdmissionError::Draining);
        }
        self.inner.active.fetch_add(1, Ordering::AcqRel);
        if self.inner.draining.load(Ordering::Acquire) {
            self.inner.active.fetch_sub(1, Ordering::AcqRel);
            return Err(AdmissionError::Draining);
        }
        Ok(RequestGuard {
            inner: Arc::clone(&self.inner),
        })
    }
    pub fn begin_shutdown(&self) {
        if !self.inner.draining.swap(true, Ordering::AcqRel) {
            self.inner.cancellation.cancel();
            self.inner.changed.notify_waiters();
        }
    }
    pub fn is_draining(&self) -> bool {
        self.inner.draining.load(Ordering::Acquire)
    }
    pub fn active_requests(&self) -> usize {
        self.inner.active.load(Ordering::Acquire)
    }
    pub async fn drain(&self, grace: Duration) -> DrainResult {
        self.begin_shutdown();
        let wait = async {
            loop {
                let notified = self.inner.changed.notified();
                if self.active_requests() == 0 {
                    break;
                }
                notified.await;
            }
        };
        if tokio::time::timeout(grace.min(MAX_SHUTDOWN_GRACE), wait)
            .await
            .is_ok()
        {
            DrainResult::Drained
        } else {
            DrainResult::TimedOut
        }
    }
}

#[derive(Debug)]
pub struct RequestGuard {
    inner: Arc<Inner>,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.inner.active.fetch_sub(1, Ordering::AcqRel);
        self.inner.changed.notify_waiters();
    }
}
