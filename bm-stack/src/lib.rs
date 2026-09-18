//! An async Bristlemouth node on embassy, built on [`bm_wire`].
//!
//! `bm-wire` is deliberately inert: pure codecs and sans-io state machines,
//! no dependencies, no clock, no I/O. This crate is where those become a node
//! that talks — it supplies the clock, the timer and the PHY, and nothing else.
//!
//! The split is not decoration. Everything that decides what goes on the wire
//! is in `bm-wire` and is compared byte for byte against the real C by
//! `bm-wire-diff`. What is here is the part that cannot be compared that way:
//! scheduling, and the driver.
//!
//! # Shape
//!
//! * [`port`] holds the seams an integrator fills — a port-aware PHY and the
//!   node's identity — as traits rather than as link-time symbols, so a test
//!   and the firmware can have different ones.
//! * [`node::Node`] is the protocol. Its two entry points are synchronous and
//!   take the current time, which is what makes them testable without an
//!   executor.
//! * [`node::Node::run`] is the loop that joins the two, and is the only async
//!   code here.
//! * [`mock`], behind the `mock` feature, is a PHY that replays a script and
//!   records what was sent — enough to exercise the whole node before any
//!   hardware exists.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(feature = "mock")]
pub mod mock;
pub mod node;
pub mod port;

pub use node::{MTU, Node, Outbound, transmit};
pub use port::{Egress, Identity, Phy};
