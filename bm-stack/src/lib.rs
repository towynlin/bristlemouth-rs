//! An async Bristlemouth node on embassy, built on [`bm_wire`].
//!
//! `bm-wire` is inert: pure codecs and sans-io state machines, no
//! dependencies, no clock, no I/O. This crate supplies the clock, the timer
//! and the PHY, and nothing else.
//!
//! Everything deciding what goes on the wire stays in `bm-wire`, where
//! `bm-wire-diff` compares it byte for byte against the real C. What is here
//! is what cannot be compared that way: scheduling, and the driver.
//!
//! # Shape
//!
//! * [`port`] holds the seams an integrator fills — a port-aware PHY, the
//!   node's identity, its real-time clock, its config storage, and the DFU
//!   update slot and no-init RAM — as traits
//!   rather than link-time symbols, so a test and the firmware can have
//!   different ones.
//! * [`config`] loads and saves [`bm_wire::configuration::ConfigStore`]
//!   through [`port::ConfigStorage`], and [`config::Configuration`] is how a
//!   node reaches it.
//! * [`dfu`] is the DFU client on a node, over [`port::DfuSlot`] and
//!   [`port::NoInitRam`].
//! * [`service`] is the service layer's application side: [`Services`], the
//!   handlers a node's `S` supplies.
//! * [`node::Node`] is the protocol. Its three receive/timer entry points are
//!   synchronous and take the current time, so they are testable without an
//!   executor.
//! * [`node::Node::run`] joins them, and is the only async code here.
//!   [`node::Node::run_app`] runs an [`app::App`] in the same loop, which is
//!   how application code sends while the node runs.
//! * [`channel`], behind the `channel` feature, is a [`NodeHandle`] for an
//!   application running as a task of its own.
//! * [`utc_time`] decodes the Spotter's `spotter/utc-time` publication, which
//!   C nodes set their RTC from.
//! * [`mock`], behind the `mock` feature, is a PHY that replays a script and
//!   records what was sent.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app;
#[cfg(feature = "channel")]
pub mod channel;
pub mod config;
pub mod dfu;
#[cfg(feature = "mock")]
pub mod mock;
pub mod node;
pub mod port;
pub mod service;
pub mod utc_time;

pub use app::App;
#[cfg(feature = "channel")]
pub use channel::{ChannelApp, Channels, Command, NodeHandle, Notification};
pub use config::{Config, Configuration, NoConfig};
pub use node::{
    Event, MTU, Node, Outbound, Owed, PublishError, Reflood, SpotterError, SubscribeError,
    UdpBindError, deliver, transmit,
};
pub use port::{
    BootRequests, ConfigStorage, DfuSlot, Egress, Identity, NoDfu, NoInitRam, NoRtc, Phy,
    RamConfigStorage, RamDfuSlot, Rtc, RtcTimeAndDate, SoftRtc,
};
pub use service::{NoServices, Services};
