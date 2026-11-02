//! Guide time windows, programme indexes, and the compatible disk cache.

pub mod cache;
mod cache_io;
mod fingerprint;
pub mod index;
pub mod window;

pub use cache::{CacheInvalidReason, CacheMetadata, CacheState, GuideCache};
pub use cache_io::{AtomicWriteHooks, DefaultAtomicWriteHooks};
pub use fingerprint::CacheFingerprint;
pub use index::{IndexError, ProgrammeIndex};
pub use window::{
    FixedGuideClock, GuideClock, GuideTimeError, GuideWindow, GuideZone, SystemGuideClock,
    build_windows, build_windows_from_clock, parse_catchup_start,
};
