//! Authenticated EPG transport and generation-keyed session state.

pub mod boot_parser;
mod client;
pub mod models;
mod responses;

pub use boot_parser::parse_boot_channels;
pub use client::{EpgClient, EpgConfig, EpgError, ProgrammeQuery};
pub use models::{
    BootChannel, Channel, ChannelId, ChannelListResponse, EpgChannelId, HlsAddress, Programme,
    ProgrammeResponse, TvodAddress, TvodAddressResponse,
};
pub use responses::{parse_channel_list, parse_programmes, parse_tvod};

#[cfg(test)]
mod client_tests;
