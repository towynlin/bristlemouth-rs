//! Differential comparator for [`bm_wire::bcmp`] against `bcmp/packet.c`.
//!
//! # Why this one is different
//!
//! Every other comparator in this crate calls a pure C function. `serialize`
//! and `process_received_message` are not pure: they read a file-scope
//! `PACKET` struct that `packet_init` fills in with accessor callbacks and a
//! registry of message types, and they take a shim mutex while they run.
//!
//! That has two consequences, and they are the reason for the [`Mutex`] and
//! the [`OnceLock`] below.
//!
//! * **The C calls are serialised.** The shim is process-global and single
//!   threaded; cargo runs tests on several threads.
//! * **`bm_shim_reset` must never be called in this process.** `packet_init`
//!   hands `PACKET` a shim mutex and a shim timer, and `packet.c` has no
//!   deinit. Resetting the shim frees both while `PACKET` still points at
//!   them. So the oracle is initialised exactly once and left up for the life
//!   of the process — which is also why the matching fuzz target needs no fork
//!   mode: nothing accumulates between iterations.
//!
//! # What the oracle is
//!
//! `packet_init` takes four accessors that turn an opaque buffer handle into a
//! source address, a destination address, the BCMP bytes, and a checksum. The
//! implementations here are `network/bm_linux.c`'s `message_get_*` functions,
//! reproduced in Rust: the handle is a pointer to an Ethernet + IPv6 frame,
//! the addresses live at offsets 22 and 38, and the BCMP bytes start at 54.
//! Using bm_linux's layout means the C under test is fed exactly the frames
//! bm_core's own IP backend feeds it.

use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard, OnceLock};

use arbitrary::{Arbitrary, Result, Unstructured};

use bm_wire::bcmp::header::{BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, CHECKSUM_FIELD_OFFSET};
use bm_wire::bcmp::{BcmpHeader, MessageType, rx, tx};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_INGRESS_EGRESS_PORTS_OFFSET, IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET,
    IPV6_SOURCE_ADDRESS_OFFSET, MIN_FRAME_WITH_ADDRESSES,
};

use crate::Domain;

/// Largest body the comparator will hand the C.
///
/// `serialize` does an unchecked `memcpy` of the caller's body into the
/// buffer, so the domain has to guarantee the buffer is big enough. bm_core's
/// own ceiling is `max_payload_len` (1460); this is well inside it and keeps
/// fuzz inputs small.
pub const MAX_BODY: usize = 512;

/// A message type the oracle does *not* register, for exercising the path
/// where `process_received_message` validates a frame it cannot dispatch.
const UNREGISTERED: u16 = 0x4242;

// ---------------------------------------------------------------------------
// The bm_linux-shaped accessors packet_init needs
// ---------------------------------------------------------------------------

unsafe extern "C" fn get_src_ip(payload: *mut c_void) -> *mut bm_wire_sys::BmIpAddr {
    unsafe { payload.cast::<u8>().add(IPV6_SOURCE_ADDRESS_OFFSET).cast() }
}

unsafe extern "C" fn get_dst_ip(payload: *mut c_void) -> *mut bm_wire_sys::BmIpAddr {
    unsafe {
        payload
            .cast::<u8>()
            .add(IPV6_DESTINATION_ADDRESS_OFFSET)
            .cast()
    }
}

unsafe extern "C" fn get_data(payload: *mut c_void) -> *mut c_void {
    unsafe { payload.cast::<u8>().add(BCMP_HEADER_OFFSET).cast() }
}

unsafe extern "C" fn get_checksum(payload: *mut c_void, size: u32) -> u16 {
    unsafe {
        bm_wire_sys::ipv6_pseudo_checksum(
            get_src_ip(payload),
            get_dst_ip(payload),
            IP_PROTO_BCMP,
            size,
            get_data(payload),
        )
    }
}

/// What the C's process callback saw, copied out so the frame can be inspected
/// afterwards without aliasing it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Processed {
    header: BcmpHeader,
    payload: Vec<u8>,
    src: [u8; 16],
    dst: [u8; 16],
    size: u32,
    ingress_port: u8,
}

static PROCESSED: Mutex<Option<Processed>> = Mutex::new(None);

unsafe extern "C" fn record(data: bm_wire_sys::BcmpProcessData) -> bm_wire_sys::BmErr {
    let processed = unsafe {
        let header = std::slice::from_raw_parts(data.header.cast::<u8>(), BCMP_HEADER_LEN);
        Processed {
            header: BcmpHeader::decode(header).expect("13 bytes is a header"),
            payload: std::slice::from_raw_parts(data.payload, data.size as usize).to_vec(),
            src: (*data.src).addr,
            dst: (*data.dst).addr,
            size: data.size,
            ingress_port: data.ingress_port,
        }
    };
    *PROCESSED.lock().unwrap_or_else(|p| p.into_inner()) = Some(processed);
    bm_wire_sys::BmErr_BmOK
}

/// Every type the oracle registers, and the domain the comparator draws from.
///
/// All of them are registered as `sequenced_reply` so that `serialize` writes
/// the sequence number it is handed rather than substituting one of its own —
/// bm_core's registry decides that per message, and which policy each real
/// message uses is protocol state, not wire format. Nothing is registered as a
/// `sequenced_request`, which is what would make the C's sequence list grow
/// across iterations.
pub const REGISTERED: &[MessageType] = &[
    MessageType::ACK,
    MessageType::HEARTBEAT,
    MessageType::ECHO_REQUEST,
    MessageType::ECHO_REPLY,
    MessageType::DEVICE_INFO_REQUEST,
    MessageType::DEVICE_INFO_REPLY,
    MessageType::PROTOCOL_CAPS_REQUEST,
    MessageType::PROTOCOL_CAPS_REPLY,
    MessageType::NEIGHBOR_TABLE_REQUEST,
    MessageType::NEIGHBOR_TABLE_REPLY,
    MessageType::RESOURCE_TABLE_REQUEST,
    MessageType::RESOURCE_TABLE_REPLY,
    MessageType::NEIGHBOR_PROTO_REQUEST,
    MessageType::NEIGHBOR_PROTO_REPLY,
    MessageType::SYSTEM_TIME_REQUEST,
    MessageType::SYSTEM_TIME_RESPONSE,
    MessageType::SYSTEM_TIME_SET,
    MessageType::CONFIG_GET,
    MessageType::CONFIG_VALUE,
    MessageType::NET_STATE_REQUEST,
    MessageType::POWER_STATE_REPLY,
    MessageType::REBOOT_REQUEST,
    MessageType::NET_ASSERT_QUIET,
    MessageType::DFU_START,
    MessageType::DFU_BOOT_COMPLETE,
];

static ORACLE: OnceLock<Mutex<()>> = OnceLock::new();

/// Bring `packet.c` up once, then serialise every use of it.
fn oracle() -> MutexGuard<'static, ()> {
    let lock = ORACLE.get_or_init(|| {
        unsafe {
            assert_eq!(
                bm_wire_sys::packet_init(
                    Some(get_src_ip),
                    Some(get_dst_ip),
                    Some(get_data),
                    Some(get_checksum),
                ),
                bm_wire_sys::BmErr_BmOK,
                "packet_init"
            );
            for ty in REGISTERED {
                let mut cfg = bm_wire_sys::BcmpPacketCfg {
                    sequenced_reply: true,
                    sequenced_request: false,
                    process: Some(record),
                };
                assert_eq!(
                    bm_wire_sys::packet_add(&mut cfg, u32::from(ty.0)),
                    bm_wire_sys::BmErr_BmOK,
                    "packet_add({ty:?})"
                );
            }
        }
        Mutex::new(())
    });
    lock.lock().unwrap_or_else(|p| p.into_inner())
}

// ---------------------------------------------------------------------------
// The input
// ---------------------------------------------------------------------------

/// One BCMP frame, plus the things the wire does to it after it is built.
#[derive(Debug, Clone)]
pub struct BcmpInput {
    /// Source address, as the sender writes it.
    pub src: [u8; 16],
    /// Destination address.
    pub dst: [u8; 16],
    /// Index into [`REGISTERED`]; kept in range by [`Domain`].
    pub type_index: u8,
    /// Use [`UNREGISTERED`] instead, so the receive path validates a frame it
    /// cannot dispatch.
    pub unregistered: bool,
    /// Sequence number.
    pub seq_num: u32,
    /// Message body. Capped at [`MAX_BODY`] by [`Domain`].
    pub body: Vec<u8>,
    /// Ingress port nibble stamped into the source address after the frame was
    /// built, as a receiving node's L2 does.
    pub ingress_stamp: u8,
    /// Source-address bytes 4 and 5, likewise stamped after the fact. These
    /// are what `clear_ports_legacy` zeroes.
    pub legacy_ports: u16,
    /// Flip a bit in the checksum, to exercise the reject path.
    pub corrupt_checksum: bool,
    /// Bytes appended past the IPv6 payload length, which must not be
    /// checksummed.
    pub trailing: u8,
}

impl<'a> Arbitrary<'a> for BcmpInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let mut input = Self::scalars(u)?;
        // `Vec<u8>`'s own `Arbitrary` stops after each element with
        // probability one half, so it almost never yields a body longer than a
        // couple of bytes -- which would leave the fuzzer blind to every
        // message that is not tiny. Take a length up front instead.
        let len = u.arbitrary_len::<u8>()?;
        input.body = u.bytes(len)?.to_vec();
        input.clamp_to_domain();
        Ok(input)
    }

    fn arbitrary_take_rest(mut u: Unstructured<'a>) -> Result<Self> {
        let mut input = Self::scalars(&mut u)?;
        input.body = u.take_rest().to_vec();
        input.clamp_to_domain();
        Ok(input)
    }
}

impl Domain for BcmpInput {
    fn clamp_to_domain(&mut self) {
        // `serialize` memcpys the body without checking the destination size.
        self.body.truncate(MAX_BODY);
        self.type_index %= REGISTERED.len() as u8;
    }
}

impl BcmpInput {
    /// Every field but the body, in the order both `Arbitrary` entry points
    /// consume them. The body comes last so `arbitrary_take_rest` can hand it
    /// whatever is left.
    fn scalars(u: &mut Unstructured<'_>) -> Result<Self> {
        Ok(Self {
            src: u.arbitrary()?,
            dst: u.arbitrary()?,
            type_index: u.arbitrary()?,
            unregistered: u.arbitrary()?,
            seq_num: u.arbitrary()?,
            ingress_stamp: u.arbitrary()?,
            legacy_ports: u.arbitrary()?,
            corrupt_checksum: u.arbitrary()?,
            trailing: u.arbitrary()?,
            body: Vec::new(),
        })
    }

    /// The message type this input serialises as.
    #[must_use]
    pub fn message_type(&self) -> MessageType {
        REGISTERED[usize::from(self.type_index) % REGISTERED.len()]
    }

    /// The type the receive path sees, which may be one nothing handles.
    #[must_use]
    pub fn rx_message_type(&self) -> MessageType {
        if self.unregistered {
            MessageType(UNREGISTERED)
        } else {
            self.message_type()
        }
    }

    fn payload_len(&self) -> usize {
        BCMP_HEADER_LEN + self.body.len()
    }

    /// A frame with the IPv6 header filled in but no BCMP content yet.
    ///
    /// Overlong by `trailing` bytes of filler, so the comparator can check
    /// that neither implementation checksums past the payload length.
    fn blank_frame(&self) -> Vec<u8> {
        let trailing = usize::from(self.trailing);
        let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES + self.payload_len() + trailing];
        frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
            .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
        frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
            .copy_from_slice(&(self.payload_len() as u16).to_be_bytes());
        frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
        frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16]
            .copy_from_slice(&self.src);
        frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&self.dst);
        let end = MIN_FRAME_WITH_ADDRESSES + self.payload_len();
        frame[end..].fill(0xA5);
        frame
    }
}

// ---------------------------------------------------------------------------
// The comparators
// ---------------------------------------------------------------------------

/// Assert `bm_wire::bcmp::tx::serialize` writes the bytes `serialize` writes.
///
/// # Panics
///
/// If the two frames differ anywhere.
pub fn check_serialize(input: &BcmpInput) {
    let mut input = input.clone();
    input.clamp_to_domain();

    let mut frame_c = input.blank_frame();
    let mut frame_rs = frame_c.clone();
    let mut body = input.body.clone();
    let ty = input.message_type();

    let _guard = oracle();
    let err = unsafe {
        bm_wire_sys::serialize(
            frame_c.as_mut_ptr().cast(),
            body.as_mut_ptr().cast(),
            body.len() as u32,
            u32::from(ty.0),
            input.seq_num,
            None,
        )
    };
    assert_eq!(
        err,
        bm_wire_sys::BmErr_BmOK,
        "the oracle registers {ty:?}, so serialize must accept it"
    );

    tx::serialize(&mut frame_rs, ty, input.seq_num, &input.body)
        .expect("blank_frame is sized for the body");

    assert_frames_eq(&frame_c, &frame_rs, "serialize", &input);
}

/// Assert `bm_wire::bcmp::rx::accept` reaches the same verdict, leaves the
/// frame in the same state, and reports the same fields as
/// `process_received_message`.
///
/// # Panics
///
/// If the verdicts, the mutated frames, or the reported fields differ.
pub fn check_accept(input: &BcmpInput) {
    let mut input = input.clone();
    input.clamp_to_domain();

    let ty = input.rx_message_type();
    let mut frame = input.blank_frame();
    tx::serialize(&mut frame, ty, input.seq_num, &input.body)
        .expect("blank_frame is sized for the body");

    // What the wire does to the frame after the sender checksummed it: a
    // receiving node's L2 stamps the ingress port into the source address, and
    // older nodes leave bytes 4 and 5 set. Both are cleared before the
    // checksum is verified, so a frame that was clean when it was built still
    // validates.
    frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET] |= (input.ingress_stamp & 0x0F) << 4;
    frame[IPV6_SOURCE_ADDRESS_OFFSET + 4..IPV6_SOURCE_ADDRESS_OFFSET + 6]
        .copy_from_slice(&input.legacy_ports.to_be_bytes());
    if input.corrupt_checksum {
        frame[BCMP_HEADER_OFFSET + CHECKSUM_FIELD_OFFSET] ^= 0xFF;
    }

    let mut frame_c = frame.clone();
    let mut frame_rs = frame;

    let _guard = oracle();
    *PROCESSED.lock().unwrap_or_else(|p| p.into_inner()) = None;
    let err = unsafe {
        bm_wire_sys::process_received_message(frame_c.as_mut_ptr().cast(), input.body.len() as u32)
    };
    let processed = PROCESSED.lock().unwrap_or_else(|p| p.into_inner()).take();

    // Copied out of the frame straight away so the comparison below can look
    // at the frame itself.
    let accepted = rx::accept(&mut frame_rs).map(|r| Processed {
        header: r.header,
        payload: r.payload.to_vec(),
        src: r.src.0,
        dst: r.dst.0,
        size: r.payload.len() as u32,
        ingress_port: r.ingress_port,
    });

    // The buffer is the contract: whatever each side did to it, it must have
    // done the same thing.
    assert_frames_eq(&frame_c, &frame_rs, "process_received_message", &input);

    let c_rejected = err == bm_wire_sys::BmErr_BmEBADMSG;
    let rs_rejected = accepted == Err(rx::RxError::BadChecksum);
    assert_eq!(
        c_rejected, rs_rejected,
        "checksum verdicts differ: C err {err}, Rust {accepted:?} ({input:?})"
    );

    let received = match accepted {
        Ok(received) => received,
        Err(rx::RxError::BadChecksum) => return,
        // blank_frame always builds a well-formed IPv6 BCMP frame, so nothing
        // else is reachable. Rust's own framing checks -- short frames, a
        // non-IPv6 EtherType, a payload length that overruns or underruns --
        // have no counterpart to compare against here, because the C is handed
        // its length by its caller rather than reading the IPv6 header. They
        // are covered by the unit tests in `bm_wire::bcmp::rx`.
        Err(other) => panic!("comparator built a frame Rust rejected as {other:?}: {input:?}"),
    };

    // A type the oracle does not register is validated but never dispatched,
    // so there is nothing further to compare.
    let Some(processed) = processed else {
        assert!(
            input.unregistered,
            "a registered type must have reached the process callback ({input:?})"
        );
        return;
    };

    assert_eq!(
        received, processed,
        "the two implementations reported different fields for the same frame ({input:?})"
    );
}

fn assert_frames_eq(c: &[u8], rs: &[u8], what: &str, input: &BcmpInput) {
    if c == rs {
        return;
    }
    let at = c
        .iter()
        .zip(rs)
        .position(|(a, b)| a != b)
        .unwrap_or(c.len().min(rs.len()));
    panic!(
        "{what} diverged at byte {at}: C {:#04x?}, Rust {:#04x?}\n  input: {input:?}\n  C:    {c:02x?}\n  Rust: {rs:02x?}",
        c.get(at),
        rs.get(at),
    );
}

/// Run both comparators.
///
/// # Panics
///
/// If either diverges from the C.
pub fn check(input: &BcmpInput) {
    check_serialize(input);
    check_accept(input);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bm_wire::util::BmIpAddr;

    fn input(ty: MessageType, body: Vec<u8>) -> BcmpInput {
        let type_index = REGISTERED
            .iter()
            .position(|t| *t == ty)
            .expect("test types are registered") as u8;
        BcmpInput {
            src: BmIpAddr::LINK_LOCAL_MULTICAST.0,
            dst: BmIpAddr::GLOBAL_MULTICAST.0,
            type_index,
            unregistered: false,
            seq_num: 0,
            body,
            ingress_stamp: 0,
            legacy_ports: 0,
            corrupt_checksum: false,
            trailing: 0,
        }
    }

    #[test]
    fn a_heartbeat_round_trips_through_both_implementations() {
        let mut i = input(
            MessageType::HEARTBEAT,
            vec![0x30, 0x7C, 0x71, 0x22, 0, 0, 0, 0, 10, 0, 0, 0],
        );
        i.src = bm_wire::addr::nodeid_to_ip(0xFE80_0000, 0x0000_0000_55AA_0011).0;
        i.dst = BmIpAddr::LINK_LOCAL_MULTICAST.0;
        check(&i);
    }

    #[test]
    fn every_registered_type_serialises_identically() {
        for (index, ty) in REGISTERED.iter().enumerate() {
            let mut i = input(*ty, vec![index as u8; 16]);
            i.seq_num = 0xDEAD_0000 | index as u32;
            check(&i);
        }
    }

    #[test]
    fn bodies_of_every_length_up_to_a_hundred() {
        for len in 0..100usize {
            let body: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(31)).collect();
            check(&input(MessageType::DEVICE_INFO_REPLY, body));
        }
    }

    #[test]
    fn the_ingress_stamp_is_undone_before_the_checksum_is_checked() {
        for port in 0..16u8 {
            let mut i = input(MessageType::HEARTBEAT, vec![1, 2, 3, 4]);
            i.ingress_stamp = port;
            check(&i);
        }
    }

    #[test]
    fn the_legacy_port_bytes_are_undone_too() {
        for legacy in [0x0000u16, 0x0001, 0xFFFF, 0xAB00, 0x00CD] {
            let mut i = input(MessageType::HEARTBEAT, vec![9; 8]);
            i.legacy_ports = legacy;
            check(&i);
        }
    }

    #[test]
    fn a_corrupted_checksum_is_rejected_the_same_way_by_both() {
        let mut i = input(MessageType::HEARTBEAT, vec![7; 12]);
        i.corrupt_checksum = true;
        check(&i);
    }

    #[test]
    fn an_unregistered_type_is_validated_but_not_dispatched() {
        let mut i = input(MessageType::HEARTBEAT, vec![4; 20]);
        i.unregistered = true;
        check_accept(&i);
    }

    #[test]
    fn trailing_bytes_are_outside_the_checksum() {
        for trailing in [0u8, 1, 7, 64, 255] {
            let mut i = input(MessageType::ECHO_REPLY, vec![3; 24]);
            i.trailing = trailing;
            check(&i);
        }
    }

    #[test]
    fn the_largest_body_the_domain_allows() {
        check(&input(MessageType::DFU_START, vec![0xC3; MAX_BODY]));
    }

    #[test]
    fn saturated_addresses_and_sequence_numbers() {
        let mut i = input(MessageType::REBOOT_REQUEST, vec![0xFF; 32]);
        i.src = [0xFF; 16];
        i.dst = [0xFF; 16];
        i.seq_num = u32::MAX;
        i.ingress_stamp = 0x0F;
        i.legacy_ports = 0xFFFF;
        i.trailing = 0xFF;
        check(&i);
    }
}
