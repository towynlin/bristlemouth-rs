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
//!   node's identity and its real-time clock — as traits rather than
//!   link-time symbols, so a test and the firmware can have different ones.
//! * [`node::Node`] is the protocol. Its three receive/timer entry points are
//!   synchronous and take the current time, so they are testable without an
//!   executor.
//! * [`node::Node::run`] joins them, and is the only async code here.
//! * [`mock`], behind the `mock` feature, is a PHY that replays a script and
//!   records what was sent.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(feature = "mock")]
pub mod mock;
pub mod node;
pub mod port;

pub use node::{Event, MTU, Node, Outbound, Owed, Reflood, deliver, transmit};
pub use port::{Egress, Identity, NoRtc, Phy, Rtc, RtcTimeAndDate, SoftRtc};
