//! `BcmpHeartbeat`, ported from `bcmp/messages.h`.
//!
//! The message a node emits every ten seconds to keep its neighbours' liveliness
//! leases alive. `bcmp/heartbeat.c` registers it as neither a sequenced request
//! nor a sequenced reply, so its header sequence number is always zero.

use crate::BmWireError;

/// A BCMP heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Heartbeat {
    /// Microseconds since the sender last reset.
    ///
    /// A neighbour treats a value lower than the one it last saw as evidence
    /// the sender rebooted.
    pub time_since_boot_us: u64,
    /// How long to consider the sender alive. Zero can mean indefinitely.
    pub liveliness_lease_dur_s: u32,
}

impl Heartbeat {
    /// Wire size. The C struct is packed, so there is no padding after the
    /// 32-bit field.
    pub const LEN: usize = 12;

    /// Decode from the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than [`Self::LEN`].
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let buf: &[u8; Self::LEN] = buf
            .get(..Self::LEN)
            .and_then(|b| b.try_into().ok())
            .ok_or(BmWireError::Truncated)?;
        Ok(Self {
            time_since_boot_us: u64::from_le_bytes([
                buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
            ]),
            liveliness_lease_dur_s: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than [`Self::LEN`].
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        let buf = buf.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
        buf[0..8].copy_from_slice(&self.time_since_boot_us.to_le_bytes());
        buf[8..12].copy_from_slice(&self.liveliness_lease_dur_s.to_le_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The body of `ipv6_pseudo_checksum_real_packet2` in
    /// `bm_core/test/src/bm_linux_test.cpp`, taken off a real link.
    #[test]
    fn a_captured_heartbeat_decodes() {
        // The capture is 25 bytes: a 13-byte header, then these twelve.
        let body = [
            0x30, 0x7C, 0x71, 0x22, 0x00, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x00, 0x00,
        ];
        let hb = Heartbeat::decode(&body).unwrap();
        assert_eq!(hb.time_since_boot_us, 0x0000_0000_2271_7C30);
        assert_eq!(hb.liveliness_lease_dur_s, 10, "the ten-second lease");

        let mut out = [0u8; Heartbeat::LEN];
        hb.encode(&mut out).unwrap();
        assert_eq!(out, body);
    }

    #[test]
    fn short_buffers_are_rejected() {
        for len in 0..Heartbeat::LEN {
            assert_eq!(
                Heartbeat::decode(&[0u8; Heartbeat::LEN][..len]),
                Err(BmWireError::Truncated)
            );
            let mut buf = [0u8; Heartbeat::LEN];
            assert_eq!(
                Heartbeat::default().encode(&mut buf[..len]),
                Err(BmWireError::Truncated)
            );
        }
    }
}
