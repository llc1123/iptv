use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use super::{CancellationToken, RenderError, RenderKind, Snapshot};

const MAX_RENDER_BASES: usize = 8;

#[derive(Default)]
pub(super) struct RenderCache {
    pub(super) clock: u64,
    pub(super) bases: Vec<CacheBase>,
}

pub(super) struct CacheBase {
    base: String,
    last_used: u64,
    m3u: Slot,
    xmltv: Slot,
}

impl CacheBase {
    fn is_active(&self) -> bool {
        matches!(self.m3u, Slot::Flight(_)) || matches!(self.xmltv, Slot::Flight(_))
    }

    fn slot_mut(&mut self, kind: RenderKind) -> &mut Slot {
        match kind {
            RenderKind::M3u => &mut self.m3u,
            RenderKind::Xmltv => &mut self.xmltv,
        }
    }
}

enum Slot {
    Empty,
    Ready(Arc<[u8]>),
    Flight(Arc<Flight>),
}

pub(super) struct Flight {
    notify: Notify,
    state: Mutex<FlightState>,
}

enum FlightState {
    Pending,
    Ready,
    Failed,
}

pub(super) enum Acquire {
    Ready(Arc<[u8]>),
    Flight { flight: Arc<Flight>, owner: bool },
}

impl Flight {
    fn new() -> Self {
        Self {
            notify: Notify::new(),
            state: Mutex::new(FlightState::Pending),
        }
    }

    fn complete(&self, result: &Result<Arc<[u8]>, RenderError>) -> Result<(), RenderError> {
        let state = match result {
            Ok(_) => FlightState::Ready,
            Err(_) => FlightState::Failed,
        };
        *self
            .state
            .lock()
            .map_err(|_| RenderError::CacheUnavailable)? = state;
        Ok(())
    }

    fn is_terminal(&self) -> Result<bool, RenderError> {
        let state = self
            .state
            .lock()
            .map_err(|_| RenderError::CacheUnavailable)?;
        Ok(matches!(*state, FlightState::Ready | FlightState::Failed))
    }
}

impl std::fmt::Debug for RenderCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RenderCache")
            .field("bases", &self.bases.len())
            .finish_non_exhaustive()
    }
}

impl Snapshot {
    pub(super) fn acquire_flight(
        &self,
        kind: RenderKind,
        base: &str,
    ) -> Result<Acquire, RenderError> {
        let mut cache = self
            .render_cache
            .lock()
            .map_err(|_| RenderError::CacheUnavailable)?;
        cache.clock = cache
            .clock
            .checked_add(1)
            .ok_or(RenderError::CacheUnavailable)?;
        if let Some(index) = cache.bases.iter().position(|entry| entry.base == base) {
            cache.bases[index].last_used = cache.clock;
            let slot = cache.bases[index].slot_mut(kind);
            return match slot {
                Slot::Ready(bytes) => Ok(Acquire::Ready(Arc::clone(bytes))),
                Slot::Flight(flight) => Ok(Acquire::Flight {
                    flight: Arc::clone(flight),
                    owner: false,
                }),
                Slot::Empty => {
                    let flight = Arc::new(Flight::new());
                    *slot = Slot::Flight(Arc::clone(&flight));
                    Ok(Acquire::Flight {
                        flight,
                        owner: true,
                    })
                }
            };
        }
        if cache.bases.len() == MAX_RENDER_BASES {
            let Some(index) = cache
                .bases
                .iter()
                .enumerate()
                .filter(|(_, entry)| !entry.is_active())
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(index, _)| index)
            else {
                return Err(RenderError::CacheCapacity);
            };
            cache.bases.remove(index);
        }
        let flight = Arc::new(Flight::new());
        let last_used = cache.clock;
        cache.bases.push(CacheBase {
            base: base.to_owned(),
            last_used,
            m3u: if kind == RenderKind::M3u {
                Slot::Flight(Arc::clone(&flight))
            } else {
                Slot::Empty
            },
            xmltv: if kind == RenderKind::Xmltv {
                Slot::Flight(Arc::clone(&flight))
            } else {
                Slot::Empty
            },
        });
        Ok(Acquire::Flight {
            flight,
            owner: true,
        })
    }

    pub(super) fn finish_flight(
        &self,
        kind: RenderKind,
        base: &str,
        flight: &Arc<Flight>,
        result: &Result<Arc<[u8]>, RenderError>,
    ) -> Result<(), RenderError> {
        let mut owned = false;
        let mut cache = self
            .render_cache
            .lock()
            .map_err(|_| RenderError::CacheUnavailable)?;
        if let Some(index) = cache.bases.iter().position(|entry| entry.base == base) {
            let slot = cache.bases[index].slot_mut(kind);
            if let Slot::Flight(current) = slot {
                if Arc::ptr_eq(current, flight) {
                    owned = true;
                    *slot = match &result {
                        Ok(bytes) => Slot::Ready(Arc::clone(bytes)),
                        Err(_) => Slot::Empty,
                    };
                    flight.complete(result)?;
                }
            }
        }
        drop(cache);
        flight.notify.notify_waiters();
        owned.then_some(()).ok_or(RenderError::OwnershipLost)
    }
}

pub(super) async fn wait_for_flight(
    flight: &Arc<Flight>,
    cancellation: Option<&CancellationToken>,
) -> Result<(), RenderError> {
    loop {
        if flight.is_terminal()? {
            return Ok(());
        }
        let notified = flight.notify.notified();
        if flight.is_terminal()? {
            return Ok(());
        }
        if let Some(token) = cancellation {
            tokio::select! {
                () = notified => {},
                () = token.cancelled() => return Err(RenderError::Cancelled),
            }
        } else {
            notified.await;
        }
    }
}

pub(super) use Acquire as CacheAcquire;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn waiter_observes_terminal_state_after_notification_was_sent() {
        let flight = Arc::new(Flight::new());
        let result: Result<Arc<[u8]>, RenderError> = Ok(Arc::from([1_u8]));
        assert!(flight.complete(&result).is_ok());
        flight.notify.notify_waiters();

        let waited =
            tokio::time::timeout(Duration::from_millis(100), wait_for_flight(&flight, None)).await;

        assert!(matches!(waited, Ok(Ok(()))));
    }
}
