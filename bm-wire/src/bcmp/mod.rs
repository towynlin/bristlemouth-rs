//! BCMP — the Bristlemouth Control Message Protocol.
//!
//! BCMP rides directly on IPv6 as protocol [`IP_PROTO_BCMP`][crate::frame::IP_PROTO_BCMP],
//! with no UDP layer: a 13-byte header ([`BcmpHeader`]) followed by a body whose
//! shape depends on [`MessageType`].
//!
//! The two halves of the wire path are [`tx::serialize`] and [`rx::accept`],
//! ported from `serialize` and `process_received_message` in `bcmp/packet.c`.
//! Both are pure functions over a caller-owned frame. The state that decides
//! what they are called with — the packet registry, the outgoing sequence
//! counter and the list of requests still waiting for a reply — is protocol
//! state rather than wire format, and lives in [`registry`].

pub mod header;
pub mod heartbeat;
pub mod info;
pub mod neighbors;
pub mod registry;
pub mod rx;
pub mod tx;

pub use header::{
    BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, BcmpHeader, CHECKSUM_FIELD_OFFSET, MIN_BCMP_FRAME_SIZE,
    MessageType,
};
pub use heartbeat::Heartbeat;
pub use info::{DeviceInfo, DeviceInfoReply, DeviceInfoRequest};
pub use neighbors::{
    NeighborInfo, NeighborTableReply, NeighborTableRequest, PortInfo, encode_neighbor_table_reply,
    neighbor_table_reply_len,
};
pub use registry::{
    DEFAULT_MESSAGE_TIMEOUT_MS, Delivery, MESSAGE_TIMER_EXPIRY_PERIOD_MS, Outgoing, PacketCfg,
    PendingRequest, Registry, RegistryError,
};
pub use rx::{Received, RxError, accept};
pub use tx::serialize;
