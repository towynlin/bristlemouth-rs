//! Differential comparator for [`bm_wire::udp`] against `bm_udp_tx_perform`
//! and the UDP branch of `bm_l2_submit` in `network/bm_linux.c`.
//!
//! A stack target, for the reason [`crate::stack`] gives: `bm_udp_tx_perform`
//! reads the addresses `bm_ip_init` sets and hands its frame to L2, so the only
//! place to read it is the capture ring. Driven from `tests/udp.rs`.
//!
//! | Direction | C | Rust |
//! |---|---|---|
//! | Send | `bm_udp_tx_perform` through L2 to the capture ring | [`bm_wire::udp::build`], then [`port_transmit`] |
//! | Receive | `bm_l2_submit`, called directly, to a callback bound per port | [`bm_wire::udp::accept`] |
//!
//! `bm_linux.c` is not what deployed nodes run, and where it differs
//! `bm_wire::udp` follows lwIP. What this comparator does about each:
//!
//! | # | Field | Here |
//! |---|---|---|
//! | 70 | Source MAC, hop limit | normalised by [`crate::stack::drain`] |
//! | 70 | Source address | the Rust side is built from `bm_linux.c`'s `fe80::<id>`; [`bm_wire::udp::source_address`] is pinned by unit tests and the capture instead |
//! | 71 | UDP checksum byte order, and a checksum computing to 0 | asserted: the Rust frame is given the checksum `bm_linux.c` writes, byte-swapped or 0, before L2, and must then match |
//! | 72 | UDP length field on receive | asserted: the C's payload is the first `length - 8` bytes of the Rust one, and the C refuses exactly where that length is under 8 or past the IPv6 payload |
//!
//! Receiving never goes through L2: relaying and the ingress nibble are
//! card U2's.

use std::sync::Mutex;

use arbitrary::Arbitrary;

use bm_wire::addr::{self, LINK_LOCAL_PREFIX};
use bm_wire::frame::{
    ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IP_PROTO_UDP, IPV6_NEXT_HEADER_OFFSET,
    IPV6_PAYLOAD_LENGTH_OFFSET, MIN_FRAME_WITH_ADDRESSES, UDP_CHECKSUM_OFFSET, UDP_HEADER_LEN,
    UDP_LENGTH_OFFSET,
};
use bm_wire::udp;
use bm_wire::util::BmIpAddr;

use crate::l2_egress::port_transmit;
use crate::stack::{NODE_ID, drain, oracle, pump_until_quiet};

/// Largest payload either direction carries.
///
/// Keeps a frame inside [`crate::stack::drain`]'s 2048-byte buffer and under
/// an Ethernet MTU. Far below [`udp::MAX_PAYLOAD_LEN`], past which
/// `bm_udp_tx_perform` truncates its 16-bit length fields where
/// `bm_wire::udp::build` refuses.
pub const MAX_PAYLOAD: usize = 1024;

/// `BM_MIDDLEWARE_PORT`, which `bm_middleware_init` binds before this module
/// runs.
pub const MIDDLEWARE_PORT: u16 = 4321;

/// Ports this module binds in the oracle, once per process: the UDP list has
/// no unbind. [`MIDDLEWARE_PORT`] is bound a second time, which gives a pcb to
/// send from; receiving on it reaches the middleware's callback, which is
/// listed first.
pub const BOUND_PORTS: [u16; 5] = [MIDDLEWARE_PORT, 0, 1, 0x1234, 0xFFFF];

/// A datagram to send.
#[derive(Debug, Clone, Arbitrary)]
pub struct Send {
    /// Index into [`BOUND_PORTS`] of the port to send from, reduced modulo
    /// its length.
    pub src_port: u8,
    /// Where to send it.
    pub dst: Dst,
    /// The destination port.
    pub dst_port: u16,
    /// The payload, cut to [`MAX_PAYLOAD`].
    pub payload: Vec<u8>,
}

/// A destination address.
#[derive(Debug, Clone, Copy, Arbitrary)]
pub enum Dst {
    /// `ff03::1`, where pub/sub publishes. One frame to every port.
    Global,
    /// `ff02::1`. One frame per port, stamped, with divergence #12's 8-bit
    /// checksum patch.
    LinkLocal,
    /// Anything. Byte 13 is L2's requested-egress-port channel, so this also
    /// narrows the ports; a unicast address is dropped unsent (divergence
    /// #13).
    Other([u8; 16]),
}

impl Dst {
    fn addr(self) -> BmIpAddr {
        match self {
            Self::Global => BmIpAddr::GLOBAL_MULTICAST,
            Self::LinkLocal => BmIpAddr::LINK_LOCAL_MULTICAST,
            Self::Other(bytes) => BmIpAddr(bytes),
        }
    }
}

/// A frame to receive: a well-formed datagram, then the header fields
/// `bm_l2_submit` and [`udp::accept`] validate, each optionally overwritten.
#[derive(Debug, Clone, Arbitrary)]
pub struct Receive {
    /// The sender's address; its low half is the node id reported.
    pub src: [u8; 16],
    /// The sender's port.
    pub src_port: u16,
    /// The destination port: an index into [`BOUND_PORTS`] when `Ok`, a raw
    /// port when `Err`. [`MIDDLEWARE_PORT`] is replaced by the next port up
    /// either way, because it reaches `bm_middleware_rx`, which parses the
    /// payload as a publication and reads past it (`bm_handle_msg`, card P1).
    pub dst_port: Result<u8, u16>,
    /// The payload, cut to [`MAX_PAYLOAD`].
    pub payload: Vec<u8>,
    /// A replacement EtherType.
    pub ethertype: Option<u16>,
    /// A replacement next header. [`IP_PROTO_BCMP`] is replaced by UDP's,
    /// because `bm_l2_submit` queues BCMP to a task this comparator does not
    /// run for it.
    pub next_header: Option<u8>,
    /// A replacement IPv6 payload length.
    pub ipv6_len: Option<u16>,
    /// A replacement UDP length.
    pub udp_len: Option<u16>,
    /// Bytes appended past the datagram.
    pub trailing: Vec<u8>,
    /// Bytes cut from the end of the frame, after `trailing` is added.
    pub cut: u16,
}

/// One step of a run.
#[derive(Debug, Clone, Arbitrary)]
pub enum Step {
    /// Send a datagram from both sides and compare the frames.
    Send(Send),
    /// Receive a frame on both sides and compare what reaches the port.
    Receive(Receive),
}

/// Steps run in order against the one oracle.
#[derive(Debug, Clone, Arbitrary)]
pub struct UdpInput {
    /// The steps.
    pub steps: Vec<Step>,
}

/// Run every step.
///
/// # Panics
///
/// On any divergence; see [`check_send`] and [`check_receive`].
pub fn check(input: &UdpInput) {
    for step in &input.steps {
        match step {
            Step::Send(send) => check_send(send),
            Step::Receive(receive) => check_receive(receive),
        }
    }
}

/// What reached a port the oracle bound: `(index into BOUND_PORTS, source
/// port, source node id, payload)`.
type Delivery = (usize, u16, u64, Vec<u8>);

static DELIVERED: Mutex<Vec<Delivery>> = Mutex::new(Vec::new());

/// The callback `bm_udp_bind_port` takes, one instance per bound port so a
/// delivery says which port it reached. It owns `buf`, as
/// `bm_middleware_rx` does.
unsafe extern "C" fn on_datagram<const I: usize>(
    src_port: u16,
    buf: *mut core::ffi::c_void,
    node_id: u64,
    len: u32,
) -> bm_wire_sys::BmErr {
    let payload = unsafe {
        let at = bm_wire_sys::bm_udp_get_payload(buf).cast::<u8>();
        let payload = std::slice::from_raw_parts(at, len as usize).to_vec();
        bm_wire_sys::bm_udp_cleanup(buf);
        payload
    };
    DELIVERED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((I, src_port, node_id, payload));
    bm_wire_sys::BmErr_BmOK
}

type UdpCallback =
    unsafe extern "C" fn(u16, *mut core::ffi::c_void, u64, u32) -> bm_wire_sys::BmErr;

const CALLBACKS: [UdpCallback; BOUND_PORTS.len()] = [
    on_datagram::<0>,
    on_datagram::<1>,
    on_datagram::<2>,
    on_datagram::<3>,
    on_datagram::<4>,
];

/// The oracle's pcbs for [`BOUND_PORTS`], bound on first use.
struct Pcbs([usize; BOUND_PORTS.len()]);

static PCBS: Mutex<Option<Pcbs>> = Mutex::new(None);

/// The pcb bound to `BOUND_PORTS[index]`. Takes the oracle lock's guard as
/// proof it is held.
fn pcb(_oracle: &std::sync::MutexGuard<'static, ()>, index: usize) -> *mut core::ffi::c_void {
    let mut pcbs = PCBS.lock().unwrap_or_else(|p| p.into_inner());
    let pcbs = pcbs.get_or_insert_with(|| {
        let group = bm_wire_sys::BmIpAddr {
            addr: BmIpAddr::GLOBAL_MULTICAST.0,
        };
        Pcbs(std::array::from_fn(|i| {
            let pcb = unsafe {
                bm_wire_sys::bm_udp_bind_port(&group, BOUND_PORTS[i], Some(CALLBACKS[i]))
            };
            assert!(!pcb.is_null(), "bm_udp_bind_port({})", BOUND_PORTS[i]);
            pcb as usize
        }))
    });
    pcbs.0[index] as *mut core::ffi::c_void
}

/// Assert a datagram sent by `bm_udp_tx_perform` and by [`udp::build`] leaves
/// on the same ports as the same bytes, modulo divergences #70 and #71 as the
/// module docs say.
///
/// # Panics
///
/// If the C refuses the send, or the frames differ.
pub fn check_send(send: &Send) {
    let index = usize::from(send.src_port) % BOUND_PORTS.len();
    let src_port = BOUND_PORTS[index];
    let dst = send.dst.addr();
    let payload = &send.payload[..send.payload.len().min(MAX_PAYLOAD)];

    let guard = oracle();
    let c = unsafe {
        assert!(
            drain().is_empty(),
            "the ring was not drained before this run"
        );
        let buf = bm_wire_sys::bm_udp_new(payload.len() as u32);
        assert!(!buf.is_null(), "bm_udp_new");
        let at = bm_wire_sys::bm_udp_get_payload(buf).cast::<u8>();
        std::ptr::copy_nonoverlapping(payload.as_ptr(), at, payload.len());
        let c_dst = bm_wire_sys::BmIpAddr { addr: dst.0 };
        let err = bm_wire_sys::bm_udp_tx_perform(
            pcb(&guard, index),
            buf,
            payload.len() as u32,
            &c_dst,
            send.dst_port,
        );
        bm_wire_sys::bm_udp_cleanup(buf);
        assert_eq!(err, bm_wire_sys::BmErr_BmOK, "bm_udp_tx_perform");
        pump_until_quiet();
        assert_eq!(
            bm_wire_sys::bm_shim_tx_dropped(),
            0,
            "capture ring overflowed"
        );
        drain()
    };
    drop(guard);

    let src = addr::nodeid_to_ip(LINK_LOCAL_PREFIX, NODE_ID);
    let mut frame = vec![0u8; udp::PAYLOAD_OFFSET + payload.len()];
    let len = udp::build(&mut frame, &src, &dst, src_port, send.dst_port, payload)
        .expect("frame is sized for the payload");
    assert_eq!(len, frame.len());

    as_bm_linux_writes_it(&mut frame);
    let rs = port_transmit(&frame);

    assert_eq!(
        c.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        rs.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        "egress ports differ ({send:?})"
    );
    for ((port, c_frame), (_, rs_frame)) in c.iter().zip(&rs) {
        assert_eq!(
            c_frame, rs_frame,
            "frame to port {port} differs ({send:?})\n  C:    {c_frame:02x?}\n  Rust: {rs_frame:02x?}"
        );
    }
}

/// Rewrite the checksum [`udp::build`] wrote as `bm_udp_tx_perform` writes
/// it (divergence #71): byte-swapped, and zero where `udp::build` writes
/// `0xFFFF`. A computed checksum is never `0xFFFF`, so that value is always
/// lwIP's replacement for zero.
///
/// # Panics
///
/// If `frame` is too short to hold a UDP header.
pub fn as_bm_linux_writes_it(frame: &mut [u8]) {
    let field = &mut frame[UDP_CHECKSUM_OFFSET..UDP_CHECKSUM_OFFSET + 2];
    if field == [0xFF, 0xFF] {
        field.fill(0);
    } else {
        field.swap(0, 1);
    }
}

impl Receive {
    fn dst_port(&self) -> u16 {
        let port = match self.dst_port {
            Ok(index) => BOUND_PORTS[usize::from(index) % BOUND_PORTS.len()],
            Err(port) => port,
        };
        if port == MIDDLEWARE_PORT {
            port + 1
        } else {
            port
        }
    }

    fn frame(&self) -> Vec<u8> {
        let payload = &self.payload[..self.payload.len().min(MAX_PAYLOAD)];
        let mut frame = vec![0u8; udp::PAYLOAD_OFFSET + payload.len()];
        udp::build(
            &mut frame,
            &BmIpAddr(self.src),
            &BmIpAddr::GLOBAL_MULTICAST,
            self.src_port,
            self.dst_port(),
            payload,
        )
        .expect("frame is sized for the payload");
        let mut put = |at: usize, value: Option<u16>| {
            if let Some(value) = value {
                frame[at..at + 2].copy_from_slice(&value.to_be_bytes());
            }
        };
        put(ETHERNET_TYPE_OFFSET, self.ethertype);
        put(IPV6_PAYLOAD_LENGTH_OFFSET, self.ipv6_len);
        put(UDP_LENGTH_OFFSET, self.udp_len);
        if let Some(next_header) = self.next_header {
            frame[IPV6_NEXT_HEADER_OFFSET] = if next_header == IP_PROTO_BCMP {
                IP_PROTO_UDP
            } else {
                next_header
            };
        }
        frame.extend_from_slice(&self.trailing);
        frame.truncate(frame.len().saturating_sub(usize::from(self.cut)));
        frame
    }
}

/// Assert `bm_l2_submit` and [`udp::accept`] agree on a received frame: on
/// whether it is a datagram, and on the source port, source node id and
/// payload a bound port is given.
///
/// # Panics
///
/// On any disagreement the module docs do not account for.
pub fn check_receive(receive: &Receive) {
    let frame = receive.frame();
    let dst_port = receive.dst_port();

    let guard = oracle();
    // Bind before submitting, so a first step that receives finds its port.
    let _ = pcb(&guard, 0);
    DELIVERED.lock().unwrap_or_else(|p| p.into_inner()).clear();
    let err = unsafe {
        let buf = bm_wire_sys::bm_l2_new(frame.len() as u32);
        assert!(!buf.is_null(), "bm_l2_new");
        let at = bm_wire_sys::bm_l2_get_payload(buf).cast::<u8>();
        std::ptr::copy_nonoverlapping(frame.as_ptr(), at, frame.len());
        let err = bm_wire_sys::bm_l2_submit(buf, frame.len() as u32);
        // BmOK means bm_l2_submit freed it.
        if err != bm_wire_sys::BmErr_BmOK {
            bm_wire_sys::bm_l2_free(buf);
        }
        err
    };
    let delivered = std::mem::take(&mut *DELIVERED.lock().unwrap_or_else(|p| p.into_inner()));
    drop(guard);

    let rs = udp::accept(&frame);
    let bound = BOUND_PORTS.iter().position(|p| *p == dst_port);

    let Ok(datagram) = rs else {
        assert_ne!(
            err,
            bm_wire_sys::BmErr_BmOK,
            "the C took a frame the Rust refuses ({rs:?}): {receive:?}"
        );
        assert!(delivered.is_empty());
        return;
    };
    assert_eq!(datagram.dst_port, dst_port);

    // Divergence #72: bm_linux.c trusts the UDP length field, lwIP does not.
    let ipv6_len = usize::from(u16::from_be_bytes([
        frame[IPV6_PAYLOAD_LENGTH_OFFSET],
        frame[IPV6_PAYLOAD_LENGTH_OFFSET + 1],
    ]));
    let udp_len = usize::from(u16::from_be_bytes([
        frame[UDP_LENGTH_OFFSET],
        frame[UDP_LENGTH_OFFSET + 1],
    ]));
    assert_eq!(datagram.payload.len(), ipv6_len - UDP_HEADER_LEN);
    assert!(frame.len() >= MIN_FRAME_WITH_ADDRESSES + ipv6_len);
    if !(UDP_HEADER_LEN..=ipv6_len).contains(&udp_len) {
        assert_ne!(
            err,
            bm_wire_sys::BmErr_BmOK,
            "the C took a UDP length of {udp_len} in {ipv6_len}: {receive:?}"
        );
        assert!(delivered.is_empty());
        return;
    }
    assert_eq!(err, bm_wire_sys::BmErr_BmOK, "the C refused {receive:?}");

    let expected: Vec<Delivery> = bound
        .map(|index| {
            (
                index,
                datagram.src_port,
                datagram.source,
                datagram.payload[..udp_len - UDP_HEADER_LEN].to_vec(),
            )
        })
        .into_iter()
        .collect();
    assert_eq!(delivered, expected, "{receive:?}");
}
