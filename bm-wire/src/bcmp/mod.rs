//! BCMP — the Bristlemouth Control Message Protocol.
//!
//! BCMP rides directly on IPv6 as protocol [`IP_PROTO_BCMP`][crate::frame::IP_PROTO_BCMP],
//! with no UDP layer: a 13-byte header ([`BcmpHeader`]) followed by a body whose
//! shape depends on [`MessageType`].
//!
//! The two halves of the wire path are [`tx::serialize`] and [`rx::accept`],
//! ported from `serialize` and `process_received_message` in `bcmp/packet.c`.
//! Both are pure functions over a caller-owned frame; the packet registry, the
//! sequence-number policy and the per-message state machines that sit above
//! them are protocol state rather than wire format and live elsewhere.

pub mod header;
pub mod heartbeat;
pub mod rx;
pub mod tx;

pub use header::{
    BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, BcmpHeader, CHECKSUM_FIELD_OFFSET, MIN_BCMP_FRAME_SIZE,
    MessageType,
};
pub use heartbeat::Heartbeat;
pub use rx::{Received, RxError, accept};
pub use tx::serialize;
