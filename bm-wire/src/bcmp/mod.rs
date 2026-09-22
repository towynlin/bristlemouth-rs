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
//!
//! [`forward`] is the third piece of the wire path: re-flooding a link-local
//! message out the other ports, which `bcmp/time.c`, `bcmp/config.c` and
//! `bcmp/dfu_core.c` all do for anything not addressed to this node.
//!
//! The message bodies each have a module of their own — [`heartbeat`],
//! [`info`], [`neighbors`], [`ping`] — and are codecs only. What a node *does*
//! with one is `bm-stack`'s business. The two exceptions are
//! [`info::InfoRequests`] and [`info::InfoCache`], which are `bcmp/info.c`'s
//! file-scope state rather than wire format, and are here for the same reason
//! [`registry`] is.

pub mod forward;
pub mod header;
pub mod heartbeat;
pub mod info;
pub mod neighbors;
pub mod ping;
pub mod registry;
pub mod rx;
pub mod time;
pub mod tx;

pub use forward::{
    apply_port_specific_destination, egress_ports, ll_forward_is_a_no_op,
    port_specific_destination, serialize_forwarded,
};
pub use header::{
    BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, BcmpHeader, CHECKSUM_FIELD_OFFSET, MIN_BCMP_FRAME_SIZE,
    MessageType,
};
pub use heartbeat::Heartbeat;
pub use info::{
    CACHED_STRING_BYTES, CachedInfo, DeviceInfo, DeviceInfoReply, DeviceInfoRequest, InfoCache,
    InfoRequestKind, InfoRequests,
};
pub use neighbors::{
    NeighborInfo, NeighborTableReply, NeighborTableRequest, PortInfo, encode_neighbor_table_reply,
    neighbor_table_reply_len,
};
pub use ping::{ECHO_HEADER_LEN, EchoReply, EchoRequest, MAX_ECHO_PAYLOAD};
pub use registry::{
    DEFAULT_MESSAGE_TIMEOUT_MS, Delivery, MESSAGE_TIMER_EXPIRY_PERIOD_MS, Outgoing, PacketCfg,
    PendingRequest, Registry, RegistryError,
};
pub use rx::{Received, RxError, accept};
pub use time::{SystemTimeHeader, SystemTimeRequest, SystemTimeResponse, SystemTimeSet};
pub use tx::serialize;
