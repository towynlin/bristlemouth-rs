//! Differential comparators for `bm_wire::l2_policy`.
//!
//! The C takes a bare function pointer with no user-data argument, so the fake
//! routing callback has to read its scripted behaviour from a thread-local.
//! The Rust closure reads the same thread-local, which is what makes the two
//! sides comparable: the callback is not just "some callback", it is the same
//! decision function driven by the same fuzz input.

use std::cell::Cell;

use arbitrary::{Arbitrary, Result, Unstructured};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_SOURCE_ADDRESS_OFFSET, MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::util::BmIpAddr;

thread_local! {
    /// What the fake routing callback should do, and what it saw.
    static SCRIPT: Cell<Script> = const { Cell::new(Script::INERT) };
}

#[derive(Debug, Clone, Copy)]
struct Script {
    /// Value the callback writes into `*egress_mask`.
    egress: u16,
    /// Value the callback returns.
    submit: bool,
    /// If set, the callback writes this byte into `src.addr[2]` -- the ports
    /// byte -- which exercises the C's in-place mutation through the pointer
    /// it hands out.
    src_write: Option<u8>,
    /// Number of times the callback was entered.
    calls: u32,
    /// Ingress port the callback last saw.
    last_port: u8,
}

impl Script {
    const INERT: Self = Self {
        egress: 0,
        submit: true,
        src_write: None,
        calls: 0,
        last_port: 0,
    };
}

unsafe extern "C" fn routing_cb(
    ingress_port: u8,
    egress_mask: *mut u16,
    src: *mut bm_wire_sys::BmIpAddr,
    _dest: *const bm_wire_sys::BmIpAddr,
) -> bool {
    SCRIPT.with(|s| {
        let mut script = s.get();
        script.calls += 1;
        script.last_port = ingress_port;
        s.set(script);

        unsafe {
            if !egress_mask.is_null() {
                *egress_mask = script.egress;
            }
            if let (Some(byte), false) = (script.src_write, src.is_null()) {
                (*src).addr[2] = byte;
            }
        }
        script.submit
    })
}

/// A frame plus the port masks and scripted callback behaviour to apply to it.
#[derive(Debug, Clone)]
pub struct L2PolicyInput {
    /// The frame bytes. Mutated in place by both implementations.
    pub frame: Vec<u8>,
    /// Mask of the port the frame arrived on.
    pub ingress_port_mask: u16,
    /// Mask of every port on the device.
    pub all_ports_mask: u16,
    /// Whether a routing callback is supplied at all.
    pub with_callback: bool,
    /// Egress mask the callback writes.
    pub cb_egress: u16,
    /// Value the callback returns.
    pub cb_submit: bool,
    /// Optional byte the callback writes into the source address ports byte.
    pub cb_src_write: Option<u8>,
}

impl<'a> Arbitrary<'a> for L2PolicyInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        // Bias hard toward well-formed frames. Purely random bytes almost never
        // carry EtherType 0x86DD, so without this the fuzzer would spend its
        // whole budget on the early-return path.
        let mut frame: Vec<u8> = u.arbitrary()?;
        let shape: u8 = u.arbitrary()?;
        if !shape.is_multiple_of(4) {
            // Make it at least frame-shaped, then stamp the EtherType.
            if frame.len() < MIN_FRAME_WITH_ADDRESSES {
                frame.resize(MIN_FRAME_WITH_ADDRESSES, 0);
            }
            frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
                .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());

            // Half the time, aim the frame at an address the policy cares about.
            let dst = match shape % 8 {
                1 => Some(BmIpAddr::GLOBAL_MULTICAST),
                2 => Some(BmIpAddr::LINK_LOCAL_MULTICAST),
                3 => {
                    let mut a = BmIpAddr::LINK_LOCAL_MULTICAST;
                    a.0[15] = u.arbitrary()?;
                    Some(a)
                }
                _ => None,
            };
            if let Some(dst) = dst {
                frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
                    .copy_from_slice(&dst.0);
            }
        }

        Ok(Self {
            frame,
            ingress_port_mask: u.arbitrary()?,
            all_ports_mask: u.arbitrary()?,
            with_callback: u.arbitrary()?,
            cb_egress: u.arbitrary()?,
            cb_submit: u.arbitrary()?,
            cb_src_write: u.arbitrary()?,
        })
    }
}

/// Assert `bm_l2_policy_rx_apply` and `bm_l2_policy_prepare_forwarded_copy`
/// agree with bm_core, both in what they return and in how they mutate the
/// frame.
///
/// # Panics
///
/// If the returned result, the mutated frame, or the callback interaction
/// diverges from the C.
pub fn check(input: &L2PolicyInput) {
    let script = Script {
        egress: input.cb_egress,
        submit: input.cb_submit,
        src_write: input.cb_src_write,
        calls: 0,
        last_port: 0,
    };

    // --- C side ---
    let mut c_frame = input.frame.clone();
    SCRIPT.with(|s| s.set(script));
    let c_result = unsafe {
        bm_wire_sys::bm_l2_policy_rx_apply(
            c_frame.as_mut_ptr(),
            c_frame.len(),
            input.ingress_port_mask,
            input.all_ports_mask,
            if input.with_callback {
                Some(routing_cb)
            } else {
                None
            },
        )
    };
    let c_script = SCRIPT.with(|s| s.get());

    // --- Rust side ---
    let mut rs_frame = input.frame.clone();
    SCRIPT.with(|s| s.set(script));
    let mut closure = |port: u8, egress: &mut u16, src: &mut BmIpAddr, _dst: &BmIpAddr| {
        SCRIPT.with(|s| {
            let mut sc = s.get();
            sc.calls += 1;
            sc.last_port = port;
            s.set(sc);
            *egress = sc.egress;
            if let Some(byte) = sc.src_write {
                src.0[2] = byte;
            }
            sc.submit
        })
    };
    let rs_result = bm_wire::l2_policy::rx_apply(
        &mut rs_frame,
        input.ingress_port_mask,
        input.all_ports_mask,
        if input.with_callback {
            Some(&mut closure)
        } else {
            None
        },
    );
    let rs_script = SCRIPT.with(|s| s.get());

    // --- Compare ---
    assert_eq!(
        c_script.calls, rs_script.calls,
        "routing callback invoked a different number of times"
    );
    assert_eq!(
        c_script.last_port, rs_script.last_port,
        "routing callback saw a different ingress port"
    );
    assert_eq!(
        c_result.should_submit, rs_result.should_submit,
        "should_submit diverged"
    );
    assert_eq!(
        c_result.egress_mask, rs_result.egress_mask,
        "egress_mask diverged"
    );
    assert_eq!(
        c_result.ingress_port_num, rs_result.ingress_port_num,
        "ingress_port_num diverged"
    );
    assert_frames_eq(&c_frame, &rs_frame, "rx_apply");

    // The forwarded-copy pass runs on the already-mutated frame, as l2.c does.
    unsafe {
        bm_wire_sys::bm_l2_policy_prepare_forwarded_copy(c_frame.as_mut_ptr(), c_frame.len())
    };
    bm_wire::l2_policy::prepare_forwarded_copy(&mut rs_frame);
    assert_frames_eq(&c_frame, &rs_frame, "prepare_forwarded_copy");
}

fn assert_frames_eq(c: &[u8], rs: &[u8], what: &str) {
    if c == rs {
        return;
    }
    let at = c.iter().zip(rs).position(|(a, b)| a != b);
    panic!(
        "{what}: frame diverged at byte {at:?} (src offset {IPV6_SOURCE_ADDRESS_OFFSET}, \
         dst offset {IPV6_DESTINATION_ADDRESS_OFFSET})\n  C: {c:02x?}\n rs: {rs:02x?}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> L2PolicyInput {
        let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES];
        frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
            .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
        L2PolicyInput {
            frame,
            ingress_port_mask: 0b01,
            all_ports_mask: 0b11,
            with_callback: false,
            cb_egress: 0,
            cb_submit: true,
            cb_src_write: None,
        }
    }

    fn with_dst(addr: &BmIpAddr) -> L2PolicyInput {
        let mut input = base();
        input.frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&addr.0);
        input
    }

    #[test]
    fn frames_too_short_to_carry_addresses() {
        for len in 0..MIN_FRAME_WITH_ADDRESSES {
            let mut input = base();
            input.frame.truncate(len);
            check(&input);
        }
    }

    #[test]
    fn non_ipv6_ethertypes() {
        for ethertype in [0x0800u16, 0x0806, 0x0000, 0xFFFF, 0x86DC, 0x86DE] {
            let mut input = base();
            input.frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
                .copy_from_slice(&ethertype.to_be_bytes());
            check(&input);
        }
    }

    #[test]
    fn every_single_bit_ingress_mask() {
        // Includes bit 15, where ffs yields 16 and the C exceeds its own
        // documented 1-15 range (divergence #4).
        for bit in 0..16 {
            let mut input = with_dst(&BmIpAddr::GLOBAL_MULTICAST);
            input.ingress_port_mask = 1 << bit;
            input.all_ports_mask = 0xFFFF;
            check(&input);
        }
    }

    #[test]
    fn multi_bit_and_zero_ingress_masks() {
        for mask in [0u16, 0b11, 0b1010, 0xFFFF, 0x8000, 0x0100] {
            let mut input = with_dst(&BmIpAddr::GLOBAL_MULTICAST);
            input.ingress_port_mask = mask;
            check(&input);
        }
    }

    #[test]
    fn ports_byte_starts_from_every_possible_value() {
        for preset in 0u8..=255 {
            let mut input = with_dst(&BmIpAddr::GLOBAL_MULTICAST);
            input.frame[bm_wire::frame::IPV6_INGRESS_EGRESS_PORTS_OFFSET] = preset;
            check(&input);
        }
    }

    #[test]
    fn neighbor_multicast_bypasses_the_callback() {
        let mut input = with_dst(&BmIpAddr::LINK_LOCAL_MULTICAST);
        input.with_callback = true;
        input.cb_egress = 0xBEEF;
        input.cb_submit = false;
        check(&input);
    }

    #[test]
    fn callback_drives_the_result() {
        let mut dst = BmIpAddr::LINK_LOCAL_MULTICAST;
        dst.0[15] = 0x42;
        for submit in [true, false] {
            for egress in [0u16, 1, 0xFFFF] {
                let mut input = with_dst(&dst);
                input.with_callback = true;
                input.cb_submit = submit;
                input.cb_egress = egress;
                check(&input);
            }
        }
    }

    #[test]
    fn callback_writing_the_ports_byte_lands_in_the_frame() {
        let mut dst = BmIpAddr::LINK_LOCAL_MULTICAST;
        dst.0[15] = 0x07;
        for byte in [0x00u8, 0x0F, 0xF0, 0xFF, 0x5A] {
            let mut input = with_dst(&dst);
            input.with_callback = true;
            input.cb_src_write = Some(byte);
            check(&input);
        }
    }

    #[test]
    fn global_multicast_with_arbitrary_middle_bytes() {
        // Only bytes 0, 1 and 15 are examined, so these must still flood.
        let mut dst = BmIpAddr::GLOBAL_MULTICAST;
        dst.0[4] = 0xAB;
        dst.0[9] = 0xCD;
        let mut input = with_dst(&dst);
        input.with_callback = true;
        check(&input);
    }
}
