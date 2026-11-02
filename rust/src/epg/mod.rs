//! Authenticated EPG transport and generation-keyed session state.

mod client;
mod request;
mod session;

pub use client::{EpgClient, EpgConfig, EpgError, EpgErrorClass, EpgErrorStage};
pub use session::{AuthCoordinator, Clock, SessionBundle, SessionClock, Sleeper};
