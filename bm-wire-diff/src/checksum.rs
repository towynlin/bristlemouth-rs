//! Differential comparators for `bm_wire::checksum` and `bm_wire::addr`.
//!
//! The oracle functions here are `static` in a normal bm_core build and only
//! become linkable because `bm-wire-sys/build.rs` defines `ENABLE_TESTING`,
//! the same thing bm_core's own unit tests do. They are declared for bindgen
//! in `bm-wire-sys/csrc/bm_shim.h`.

use std::ffi::CStr;

use arbitrary::{Arbitrary, Result, Unstructured};
use bm_wire::util::BmIpAddr;

/// A captured packet with the checksum a live node agreed on.
pub struct GoldVector {
    /// Name of the originating gtest case.
    pub name: &'static str,
    /// Source address.
    pub src: [u8; 16],
    /// Destination address.
    pub dst: [u8; 16],
    /// Upper-layer bytes, with the checksum field zeroed.
    pub data: &'static [u8],
    /// Checksum both implementations must produce.
    pub expected: u16,
}

include!("gold_vectors.rs");

/// Source, destination, next-header and payload for a checksum.
#[derive(Debug, Clone)]
pub struct ChecksumInput {
    /// Source address.
    pub src: [u8; 16],
    /// Destination address.
    pub dst: [u8; 16],
    /// Next-header / IP protocol byte.
    pub next_header: u8,
    /// Upper-layer data. Its length is what the C is told the length is, so
    /// the two can never disagree and the C never reads past the buffer.
    pub data: Vec<u8>,
    /// A 32-bit prefix, for the address-derivation checks.
    pub prefix: u32,
    /// A node id, likewise.
    pub node_id: u64,
}

impl<'a> Arbitrary<'a> for ChecksumInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        Ok(Self {
            src: u.arbitrary()?,
            dst: u.arbitrary()?,
            next_header: u.arbitrary()?,
            data: u.arbitrary()?,
            prefix: u.arbitrary()?,
            node_id: u.arbitrary()?,
        })
    }
}

/// Assert the pseudo-header checksum agrees with bm_core.
///
/// # Panics
///
/// If the checksum diverges from the C.
pub fn check_checksum(input: &ChecksumInput) {
    let c_src = bm_wire_sys::BmIpAddr { addr: input.src };
    let c_dst = bm_wire_sys::BmIpAddr { addr: input.dst };

    let c = unsafe {
        bm_wire_sys::ipv6_pseudo_checksum(
            &c_src,
            &c_dst,
            input.next_header,
            input.data.len() as u32,
            input.data.as_ptr().cast(),
        )
    };
    let rs = bm_wire::checksum::ipv6_pseudo_checksum(
        &BmIpAddr(input.src),
        &BmIpAddr(input.dst),
        input.next_header,
        &input.data,
    );
    assert_eq!(
        c,
        rs,
        "ipv6_pseudo_checksum diverged (next_header {:#04x}, {} bytes)",
        input.next_header,
        input.data.len()
    );
}

/// Assert the address and MAC derivations agree with bm_core.
///
/// # Panics
///
/// If any derived address, MAC, multicast test, or formatted string diverges.
pub fn check_addr_derivation(input: &ChecksumInput) {
    // nodeid_to_ip
    let mut c_ip = bm_wire_sys::BmIpAddr::default();
    unsafe { bm_wire_sys::nodeid_to_ip(&mut c_ip, input.prefix, input.node_id) };
    let rs_ip = bm_wire::addr::nodeid_to_ip(input.prefix, input.node_id);
    assert_eq!(c_ip.addr, rs_ip.0, "nodeid_to_ip diverged");

    // mac_from_nodeid
    let mut c_mac = [0u8; 6];
    unsafe { bm_wire_sys::mac_from_nodeid(c_mac.as_mut_ptr(), input.node_id) };
    assert_eq!(
        c_mac,
        bm_wire::addr::mac_from_nodeid(input.node_id),
        "mac_from_nodeid diverged for {:#018x}",
        input.node_id
    );

    let c_dst = bm_wire_sys::BmIpAddr { addr: input.dst };
    let rs_dst = BmIpAddr(input.dst);

    // multicast_mac_from_ipv6
    let mut c_mmac = [0u8; 6];
    unsafe { bm_wire_sys::multicast_mac_from_ipv6(c_mmac.as_mut_ptr(), &c_dst) };
    assert_eq!(
        c_mmac,
        bm_wire::addr::multicast_mac_from_ipv6(&rs_dst),
        "multicast_mac_from_ipv6 diverged"
    );

    // is_multicast
    let c_multi = unsafe { bm_wire_sys::is_multicast(&c_dst) };
    assert_eq!(
        c_multi,
        bm_wire::addr::is_multicast(&rs_dst),
        "is_multicast diverged"
    );

    // format_ipv6. The C writes into a caller-provided buffer it documents as
    // needing 40 bytes; give it extra and check it never used the slack.
    const SLACK: usize = 24;
    let mut buf = [0i8; bm_wire::addr::IPV6_STR_LEN + SLACK];
    let guard = 0x7Fi8;
    buf[bm_wire::addr::IPV6_STR_LEN..].fill(guard);
    unsafe { bm_wire_sys::format_ipv6(buf.as_mut_ptr(), &c_dst) };
    assert!(
        buf[bm_wire::addr::IPV6_STR_LEN..]
            .iter()
            .all(|&b| b == guard),
        "format_ipv6 wrote past the 40 bytes it documents"
    );
    let c_str = unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_str()
        .expect("format_ipv6 emits ASCII");
    let rs_str = bm_wire::addr::format_ipv6(&rs_dst);
    assert_eq!(
        c_str,
        rs_str.as_str(),
        "format_ipv6 diverged for {:02x?}",
        input.dst
    );
}

/// Run both comparators.
///
/// # Panics
///
/// If either diverges from the C.
pub fn check(input: &ChecksumInput) {
    check_checksum(input);
    check_addr_derivation(input);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(src: [u8; 16], dst: [u8; 16], next_header: u8, data: Vec<u8>) -> ChecksumInput {
        ChecksumInput {
            src,
            dst,
            next_header,
            data,
            prefix: 0xFE80_0000,
            node_id: 0,
        }
    }

    /// The four captured packets, asserted against their literal expected
    /// values -- an independent check that C and Rust agreeing is not merely
    /// two implementations being wrong the same way.
    #[test]
    fn gold_vectors_match_their_recorded_checksums() {
        for v in GOLD_VECTORS {
            let rs = bm_wire::checksum::ipv6_pseudo_checksum(
                &BmIpAddr(v.src),
                &BmIpAddr(v.dst),
                bm_wire::frame::IP_PROTO_BCMP,
                v.data,
            );
            assert_eq!(
                rs, v.expected,
                "{} : Rust disagreed with the capture",
                v.name
            );

            let c_src = bm_wire_sys::BmIpAddr { addr: v.src };
            let c_dst = bm_wire_sys::BmIpAddr { addr: v.dst };
            let c = unsafe {
                bm_wire_sys::ipv6_pseudo_checksum(
                    &c_src,
                    &c_dst,
                    bm_wire::frame::IP_PROTO_BCMP,
                    v.data.len() as u32,
                    v.data.as_ptr().cast(),
                )
            };
            assert_eq!(c, v.expected, "{} : C disagreed with the capture", v.name);
        }
    }

    #[test]
    fn gold_vectors_pass_the_differential_check() {
        for v in GOLD_VECTORS {
            check(&input(
                v.src,
                v.dst,
                bm_wire::frame::IP_PROTO_BCMP,
                v.data.to_vec(),
            ));
        }
    }

    #[test]
    fn empty_and_odd_payloads() {
        let zero = [0u8; 16];
        for len in 0..40usize {
            let data: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(37)).collect();
            check(&input(zero, zero, 0xBC, data));
        }
    }

    #[test]
    fn saturated_addresses_and_payloads() {
        check(&input([0xFF; 16], [0xFF; 16], 0xFF, vec![0xFF; 64]));
        check(&input([0xFF; 16], [0x00; 16], 0x00, vec![0xFF; 65]));
    }

    #[test]
    fn address_formatting_edge_cases() {
        let cases: [[u8; 16]; 7] = [
            [0; 16],
            [0xFF; 16],
            BmIpAddr::LINK_LOCAL_MULTICAST.0,
            BmIpAddr::GLOBAL_MULTICAST.0,
            // Two equal-length zero runs: the first must win.
            [0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 5],
            // A single zero word, too short to compress.
            [0, 1, 0, 0, 0, 2, 0, 3, 0, 4, 0, 5, 0, 6, 0, 7],
            // Trailing zero run.
            [0xfe, 0x80, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        ];
        for dst in cases {
            check(&input([0; 16], dst, 0, vec![]));
        }
    }

    #[test]
    fn node_ids_across_the_bit_range() {
        for bit in 0..64 {
            let mut i = input([0; 16], [0; 16], 0, vec![]);
            i.node_id = 1u64 << bit;
            i.prefix = 0xFE80_0000;
            check(&i);
        }
        for id in [0u64, u64::MAX, 0xDEAD_BEEF_1234_5678] {
            let mut i = input([0; 16], [0; 16], 0, vec![]);
            i.node_id = id;
            check(&i);
        }
    }
}
