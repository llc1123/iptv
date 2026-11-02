//! Authenticated EPG transport and generation-keyed session state.

pub mod boot_parser;
pub mod channel_parser;
mod client;
pub mod models;
pub mod programme_parser;
mod request;
mod session;

pub use boot_parser::parse_boot_channels;
pub use channel_parser::{ChannelGroupSource, merge_category_groups, parse_channel_list};
pub use client::{EpgClient, EpgConfig, EpgError, EpgErrorClass, EpgErrorStage};
pub use models::{
    BootChannel, Channel, ChannelId, ChannelListResponse, EpgChannelId, HlsAddress, Programme,
    ProgrammeResponse, StbIdentitySources, TvodAddress, TvodAddressResponse,
};
pub use programme_parser::{parse_programmes, tvod_times_seconds};
pub use session::{AuthCoordinator, Clock, SessionBundle, SessionClock, Sleeper};
