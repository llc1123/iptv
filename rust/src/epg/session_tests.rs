use std::future::pending;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::{Client, Url};
use tokio::sync::Barrier;

use super::{AuthCoordinator, Clock, EpgError, EpgErrorStage, SessionBundle};
use crate::epg::EpgErrorClass;

#[derive(Clone)]
struct TestClock {
    now: Arc<Mutex<Instant>>,
}

impl TestClock {
    fn new(now: Instant) -> Self {
        Self {
            now: Arc::new(Mutex::new(now)),
        }
    }

    fn set(&self, now: Instant) {
        *self.now.lock().expect("lock test clock") = now;
    }
}

impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self.now.lock().expect("read test clock")
    }
}

fn fixture_bundle(generation: u64, created_at: Instant) -> SessionBundle {
    let client = Client::builder().no_proxy().build().expect("client");
    SessionBundle::new(
        generation,
        created_at,
        Url::parse("https://example.invalid:8443").expect("origin"),
        "fixture-user-token".into(),
        "fixture-session-id".into(),
        "fixture-temp-key".into(),
        client.clone(),
        client,
        Arc::new(tokio::sync::Semaphore::new(1)),
        Duration::from_secs(1),
        vec!["session=fixture-cookie".into()],
    )
}

#[test]
fn session_freshness_expires_at_exactly_six_hundred_seconds() {
    let now = Instant::now();
    let bundle = fixture_bundle(
        7,
        now.checked_sub(Duration::from_secs(599))
            .expect("recent instant"),
    );
    assert!(bundle.is_fresh(now));
    let expired = fixture_bundle(
        7,
        now.checked_sub(Duration::from_secs(600))
            .expect("expired instant"),
    );
    assert!(!expired.is_fresh(now));
}

#[test]
fn session_debug_keeps_safe_generation_and_role_metadata_only() {
    let bundle = fixture_bundle(7, Instant::now());
    let rendered = format!("{bundle:?}");
    for secret in [
        "https://example.invalid:8443",
        "fixture-user-token",
        "fixture-session-id",
        "fixture-temp-key",
        "fixture-cookie",
    ] {
        assert!(!rendered.contains(secret), "debug leaked {secret}");
    }
    assert!(rendered.contains("generation: 7"));
    assert!(rendered.contains("origin_scheme: \"https\""));
    assert!(rendered.contains("control_client_role: \"control\""));
    assert!(rendered.contains("media_client_role: \"media\""));
    assert!(rendered.contains("cookie_snapshot_present: true"));
}

#[tokio::test]
async fn stale_callers_share_one_flight_even_after_the_previous_flight_completes() {
    let base = Instant::now();
    let clock = TestClock::new(base);
    let coordinator = AuthCoordinator::new(Arc::new(clock.clone()));
    let calls = Arc::new(AtomicUsize::new(0));

    let barrier = Arc::new(Barrier::new(3));
    let first = tokio::spawn(join_call(
        coordinator.clone(),
        Arc::clone(&calls),
        Arc::clone(&barrier),
    ));
    let second = tokio::spawn(join_call(
        coordinator.clone(),
        Arc::clone(&calls),
        Arc::clone(&barrier),
    ));
    barrier.wait().await;
    let (first, second) = tokio::join!(first, second);
    assert_eq!(
        first.expect("first task").expect("first auth").generation(),
        0
    );
    assert_eq!(
        second
            .expect("second task")
            .expect("second auth")
            .generation(),
        0
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    clock.set(base + Duration::from_secs(SESSION_TTL_SECS + 1));
    let barrier = Arc::new(Barrier::new(3));
    let first = tokio::spawn(join_call(
        coordinator.clone(),
        Arc::clone(&calls),
        Arc::clone(&barrier),
    ));
    let second = tokio::spawn(join_call(
        coordinator.clone(),
        Arc::clone(&calls),
        Arc::clone(&barrier),
    ));
    barrier.wait().await;
    let (first, second) = tokio::join!(first, second);
    assert_eq!(
        first
            .expect("refresh task")
            .expect("refresh auth")
            .generation(),
        1
    );
    assert_eq!(
        second
            .expect("refresh task")
            .expect("refresh auth")
            .generation(),
        1
    );
    assert_eq!(calls.load(Ordering::Relaxed), 2);
}

const SESSION_TTL_SECS: u64 = super::SESSION_TTL.as_secs();

async fn join_call(
    coordinator: AuthCoordinator,
    calls: Arc<AtomicUsize>,
    barrier: Arc<Barrier>,
) -> Result<Arc<SessionBundle>, EpgError> {
    barrier.wait().await;
    let created_at = Instant::now();
    coordinator
        .ensure_authenticated(move |generation| {
            calls.fetch_add(1, Ordering::Relaxed);
            async move { Ok(Arc::new(fixture_bundle(generation, created_at))) }
        })
        .await
}

#[tokio::test]
async fn failed_refresh_preserves_the_previous_stale_bundle() {
    let now = Instant::now();
    let clock = TestClock::new(now);
    let coordinator = AuthCoordinator::new(Arc::new(clock.clone()));
    let previous = coordinator
        .ensure_authenticated(move |generation| async move {
            Ok(Arc::new(fixture_bundle(generation, now)))
        })
        .await
        .expect("initial auth");
    clock.set(now + Duration::from_secs(SESSION_TTL_SECS + 1));
    let result = coordinator
        .ensure_authenticated(|_| async { Err(EpgError::cancelled(EpgErrorStage::Authentication)) })
        .await;
    assert_eq!(
        result.expect_err("refresh failure").class_code(),
        EpgErrorClass::Cancelled
    );
    assert_eq!(
        coordinator
            .current()
            .await
            .expect("previous bundle")
            .generation(),
        previous.generation()
    );
}

#[tokio::test]
async fn caller_shutdown_cancels_the_authentication_flight() {
    let source = crate::lifecycle::CancellationSource::new();
    let flight_shutdown = source.token();
    let coordinator = AuthCoordinator::new(Arc::new(TestClock::new(Instant::now())));
    let task = tokio::spawn(async move {
        coordinator
            .ensure_authenticated_with_shutdown(
                |_| async { pending::<Result<Arc<SessionBundle>, EpgError>>().await },
                flight_shutdown,
            )
            .await
    });
    tokio::task::yield_now().await;
    source.cancel();
    let result = task.await.expect("cancellation waiter");
    assert_eq!(
        result.expect_err("shutdown error").class_code(),
        EpgErrorClass::Cancelled
    );
}
