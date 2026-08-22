//! Differential comparator for [`bm_wire::bcmp::info`] and
//! [`bm_wire::bcmp::neighbors`], driven through bm_core's live stack.
//!
//! # What is being compared
//!
//! `bcmp_send_info` and `bcmp_send_neighbor_table` are both `static`, so there
//! is no encoder to call. Instead the oracle is asked a question and its answer
//! is read off the wire: inject a device-info request, pump the tasks, and the
//! C assembles a reply from the `DeviceCfg` the stack was brought up with and
//! hands it to the network device.
//!
//! That gives a two-sided check on one exchange:
//!
//! * **Decode.** [`bm_wire::bcmp::rx::accept`] validates the frame the C built,
//!   and the codec parses the body. Every field is then compared against the
//!   `DeviceCfg` in [`crate::stack`] — so this checks the port against the
//!   configuration bm_core was given, not merely against bm_core.
//! * **Encode.** The parsed message is re-encoded and compared byte for byte
//!   with the body the C produced.
//!
//! # Input domain
//!
//! The C is fed only well-formed requests. It cannot be fed a malformed
//! *reply*: `populate_neighbor_info` and `topology.c`'s `neighbor_request_cb`
//! both copy attacker-declared lengths out of the frame without checking them
//! against the frame's size, so a short reply is an out-of-bounds read there
//! with no defined behaviour to compare against. That is divergence #14. The
//! `decode_probe` field exercises the Rust decoders with arbitrary bytes
//! instead, and is deliberately never handed to the C.
//!
//! This module brings the stack up and so must not share a process with
//! [`crate::bcmp`] — see [`crate::stack`].

use arbitrary::{Arbitrary, Result, Unstructured};

use bm_wire::bcmp::info::{DeviceInfoReply, DeviceInfoRequest};
use bm_wire::bcmp::neighbors::{NeighborTableReply, NeighborTableRequest};
use bm_wire::bcmp::{BCMP_HEADER_LEN, MessageType, rx, tx};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET,
    MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::util::BmIpAddr;

use crate::Domain;
use crate::stack::{self, NUM_PORTS, drain, inject, oracle};

/// Node id the injected request appears to come from. Anything but the
/// oracle's own.
pub const PEER_NODE_ID: u64 = 0x0000_0000_55AA_0011;

/// Which question to ask the oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// `BcmpDeviceInfoRequestMessage`, answered with a `BcmpDeviceInfoReply`.
    DeviceInfo,
    /// `BcmpNeighborTableRequestMessage`, answered with a
    /// `BcmpNeighborTableReply`.
    NeighborTable,
}

impl Request {
    fn request_type(self) -> MessageType {
        match self {
            Self::DeviceInfo => MessageType::DEVICE_INFO_REQUEST,
            Self::NeighborTable => MessageType::NEIGHBOR_TABLE_REQUEST,
        }
    }

    fn reply_type(self) -> MessageType {
        match self {
            Self::DeviceInfo => MessageType::DEVICE_INFO_REPLY,
            Self::NeighborTable => MessageType::NEIGHBOR_TABLE_REPLY,
        }
    }
}

/// Who the request is addressed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Zero, which every node answers.
    All,
    /// The oracle's own node id, which it answers.
    ThisNode,
    /// Somebody else's, which it must ignore.
    OtherNode,
}

impl Target {
    fn node_id(self) -> u64 {
        match self {
            Self::All => 0,
            Self::ThisNode => stack::NODE_ID,
            Self::OtherNode => PEER_NODE_ID,
        }
    }

    fn expects_reply(self) -> bool {
        !matches!(self, Self::OtherNode)
    }
}

/// One request put to the oracle, plus bytes for the decoders alone.
#[derive(Debug, Clone)]
pub struct BcmpMessagesInput {
    /// Which question to ask.
    pub request: Request,
    /// Who to address it to.
    pub target: Target,
    /// Port to inject on, clamped to 1..=[`NUM_PORTS`] by [`Domain`].
    pub ingress_port: u8,
    /// Address the request to `FF03::1` rather than `FF02::1`. The reply goes
    /// back to whichever it was, so this also decides whether the reply is
    /// stamped with an egress port.
    pub global_multicast: bool,
    /// Arbitrary bytes fed to the Rust decoders and **never** to the C, for
    /// the reason the module docs give.
    pub decode_probe: Vec<u8>,
}

impl<'a> Arbitrary<'a> for BcmpMessagesInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let mut input = Self::scalars(u)?;
        let len = u.arbitrary_len::<u8>()?;
        input.decode_probe = u.bytes(len)?.to_vec();
        input.clamp_to_domain();
        Ok(input)
    }

    fn arbitrary_take_rest(mut u: Unstructured<'a>) -> Result<Self> {
        let mut input = Self::scalars(&mut u)?;
        input.decode_probe = u.take_rest().to_vec();
        input.clamp_to_domain();
        Ok(input)
    }
}

impl Domain for BcmpMessagesInput {
    fn clamp_to_domain(&mut self) {
        // Map any byte into 1..=NUM_PORTS without moving values already in range.
        self.ingress_port = self.ingress_port.wrapping_sub(1) % NUM_PORTS + 1;
        self.decode_probe.truncate(2048);
    }
}

impl BcmpMessagesInput {
    fn scalars(u: &mut Unstructured<'_>) -> Result<Self> {
        let request = if u.arbitrary::<bool>()? {
            Request::DeviceInfo
        } else {
            Request::NeighborTable
        };
        let target = match u.arbitrary::<u8>()? % 3 {
            0 => Target::All,
            1 => Target::ThisNode,
            _ => Target::OtherNode,
        };
        Ok(Self {
            request,
            target,
            ingress_port: u.arbitrary()?,
            global_multicast: u.arbitrary()?,
            decode_probe: Vec::new(),
        })
    }

    fn destination(&self) -> BmIpAddr {
        if self.global_multicast {
            BmIpAddr::GLOBAL_MULTICAST
        } else {
            BmIpAddr::LINK_LOCAL_MULTICAST
        }
    }

    /// The request frame, checksummed and ready to inject.
    fn build(&self) -> Vec<u8> {
        let body = self.target.node_id().to_le_bytes();
        let payload_len = BCMP_HEADER_LEN + body.len();
        let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES + payload_len];
        frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
            .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
        frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
            .copy_from_slice(&(payload_len as u16).to_be_bytes());
        frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
        frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16]
            .copy_from_slice(&bm_wire::addr::nodeid_to_ip(0xFE80_0000, PEER_NODE_ID).0);
        frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&self.destination().0);
        tx::serialize(&mut frame, self.request.request_type(), 0, &body)
            .expect("frame is sized for the body");
        frame
    }
}

/// The request frame an input injects, for tests that want to look at it.
#[must_use]
pub fn build_request(input: &BcmpMessagesInput) -> Vec<u8> {
    let mut input = input.clone();
    input.clamp_to_domain();
    input.build()
}

/// A captured frame, parsed far enough to say what it is.
struct Parsed {
    message_type: MessageType,
    body: Vec<u8>,
}

/// Run every captured frame through the port's receive path.
fn parse_captured(captured: Vec<(u8, Vec<u8>)>) -> Vec<Parsed> {
    captured
        .into_iter()
        .map(|(port, mut frame)| {
            // The node at the other end would stamp its ingress port here; the
            // receive path clears it again. Doing it makes the parse the same
            // one a real peer performs.
            let received = rx::accept(&mut frame).unwrap_or_else(|e| {
                panic!("the port could not accept a frame bm_core transmitted on port {port}: {e}")
            });
            Parsed {
                message_type: received.header.message_type,
                body: received.payload.to_vec(),
            }
        })
        .collect()
}

/// Assert the port decodes what bm_core built, and rebuilds the same bytes.
///
/// # Panics
///
/// If the reply is missing or unexpected, if any field disagrees with the
/// `DeviceCfg` the stack was brought up with, or if re-encoding does not
/// reproduce the C's bytes.
pub fn check(input: &BcmpMessagesInput) {
    let mut input = input.clone();
    input.clamp_to_domain();

    // The decoders get their workout whether or not the C is involved.
    probe_decoders(&input.decode_probe);

    let frame = input.build();

    let _guard = oracle();
    assert!(
        drain().is_empty(),
        "the ring was not drained before this run"
    );
    inject(input.ingress_port, &frame);
    let parsed = parse_captured(drain());

    let replies: Vec<&Parsed> = parsed
        .iter()
        .filter(|p| p.message_type == input.request.reply_type())
        .collect();

    if !input.target.expects_reply() {
        assert!(
            replies.is_empty(),
            "a request addressed to another node must not be answered ({input:?})"
        );
        return;
    }

    // Link-local replies are stamped per port, so one per enabled port; a
    // global-multicast reply goes out once, on the device's all-ports encoding.
    let expected = if input.global_multicast {
        1
    } else {
        usize::from(NUM_PORTS)
    };
    assert_eq!(
        replies.len(),
        expected,
        "expected {expected} replies, got {} ({input:?})",
        replies.len()
    );

    for reply in &replies {
        match input.request {
            Request::DeviceInfo => check_device_info(&reply.body),
            Request::NeighborTable => check_neighbor_table(&reply.body),
        }
    }

    // Every copy of the reply must carry the same body, whatever port it left
    // on: L2 stamps the address, not the payload.
    for reply in &replies[1..] {
        assert_eq!(
            reply.body, replies[0].body,
            "the same reply differed between ports ({input:?})"
        );
    }
}

fn check_device_info(body: &[u8]) {
    let reply = DeviceInfoReply::decode(body)
        .unwrap_or_else(|e| panic!("could not decode a reply bm_core built: {e}"));

    assert_eq!(reply.info.node_id, stack::NODE_ID, "node_id");
    assert_eq!(reply.info.vendor_id, stack::VENDOR_ID, "vendor_id");
    assert_eq!(reply.info.product_id, stack::PRODUCT_ID, "product_id");
    assert_eq!(reply.info.git_sha, stack::GIT_SHA, "git_sha");
    assert_eq!(reply.info.ver_hw, stack::HW_VERSION, "ver_hw");
    assert_eq!(
        (
            reply.info.ver_major,
            reply.info.ver_minor,
            reply.info.ver_rev
        ),
        stack::FIRMWARE_VERSION,
        "firmware version"
    );
    assert_eq!(reply.info.serial_num, stack::SERIAL_NUMBER, "serial_num");
    assert_eq!(
        reply.version_string,
        stack::VERSION_STRING,
        "version string"
    );
    assert_eq!(reply.device_name, stack::DEVICE_NAME, "device name");

    let mut again = vec![0u8; body.len()];
    let len = reply.encode(&mut again).expect("re-encode");
    assert_eq!(len, body.len(), "re-encoded length differs");
    assert_eq!(again, body, "re-encoded bytes differ from bm_core's");
}

fn check_neighbor_table(body: &[u8]) {
    let reply = NeighborTableReply::decode(body)
        .unwrap_or_else(|e| panic!("could not decode a reply bm_core built: {e}"));

    assert_eq!(reply.node_id, stack::NODE_ID, "node_id");
    assert_eq!(reply.port_count(), NUM_PORTS, "port_count");
    for (index, port) in reply.ports().enumerate() {
        assert!(
            port.is_up(),
            "port {index} should be up; both links were brought up"
        );
        assert_eq!(
            port.port_type, 0,
            "bcmp_send_neighbor_table never sets the type field"
        );
    }
    // The oracle has no neighbours: nothing ever sends it a heartbeat.
    assert_eq!(reply.neighbor_count(), 0, "neighbor_count");

    let mut again = vec![0u8; body.len()];
    let len = reply.encode(&mut again).expect("re-encode");
    assert_eq!(len, body.len(), "re-encoded length differs");
    assert_eq!(again, body, "re-encoded bytes differ from bm_core's");
}

/// Feed arbitrary bytes to every decoder in this module.
///
/// There is no C counterpart to compare against — that is the point of
/// divergence #14 — so the property checked is the port's own: a decoder
/// either refuses the bytes or produces something that re-encodes to a prefix
/// of them, and it never panics.
fn probe_decoders(bytes: &[u8]) {
    if let Ok(reply) = DeviceInfoReply::decode(bytes) {
        let len = reply.encoded_len();
        let mut again = vec![0u8; len];
        assert_eq!(reply.encode(&mut again).unwrap(), len);
        assert_eq!(
            &again[..],
            &bytes[..len],
            "device info reply did not round-trip"
        );
    }
    if let Ok(reply) = NeighborTableReply::decode(bytes) {
        let len = reply.encoded_len();
        let mut again = vec![0u8; len];
        assert_eq!(reply.encode(&mut again).unwrap(), len);
        assert_eq!(
            &again[..],
            &bytes[..len],
            "neighbor table reply did not round-trip"
        );
    }
    // The fixed-size requests, for completeness.
    if let Ok(request) = DeviceInfoRequest::decode(bytes) {
        let mut again = [0u8; DeviceInfoRequest::LEN];
        request.encode(&mut again).unwrap();
        assert_eq!(&again[..], &bytes[..DeviceInfoRequest::LEN]);
    }
    if let Ok(request) = NeighborTableRequest::decode(bytes) {
        let mut again = [0u8; NeighborTableRequest::LEN];
        request.encode(&mut again).unwrap();
        assert_eq!(&again[..], &bytes[..NeighborTableRequest::LEN]);
    }
}
