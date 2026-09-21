//! `BcmpSystemTimeRequest`, `BcmpSystemTimeResponse` and `BcmpSystemTimeSet`,
//! ported from `bcmp/messages.h` and `bcmp/time.c`.
//!
//! The three types `0x10`, `0x11` and `0x12` share a 16-byte head —
//! [`SystemTimeHeader`], `BcmpSystemTimeHeader` in the C — and the two that
//! carry a timestamp append one 64-bit field to it. The request carries
//! nothing else, which is why it is the header and nothing more.
//!
//! # A second target field
//!
//! Every other BCMP message that is addressed at all puts its
//! `target_node_id` first and stops there. These carry the **sender's** node id
//! as well, in the body, even though the frame's source address already holds
//! it. `bcmp_time_send_response` answers `msg->header.source_node_id` rather
//! than the address the request arrived from, so the body's copy is the one
//! that decides where a reply goes.
//!
//! # `target_node_id` does not mean the same thing three times
//!
//! `bcmp_time_process_time_message` tests the target twice: once to decide
//! whether to forward, where zero means "for everyone", and again inside the
//! `switch`, where only an exact match will do. A **request** or a **response**
//! addressed to zero therefore passes the first test and fails the second, and
//! is dropped without an answer and without a forward; a **set** skips the
//! inner test entirely and is honoured. So `0x12` broadcasts and `0x10` does
//! not, from the same field. See divergence #27 — the port reproduces it, and
//! [`SystemTimeRequest::is_for`] is where the asymmetry is written down.

use crate::BmWireError;

/// `BcmpSystemTimeHeader`: who a system-time message is for, and who sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SystemTimeHeader {
    /// Node the message is addressed to. Zero is a broadcast — but only
    /// [`SystemTimeSet`] treats it as one; see the module docs.
    pub target_node_id: u64,
    /// Node that sent it. A reply goes here, not to the frame's source address.
    pub source_node_id: u64,
}

impl SystemTimeHeader {
    /// Wire size. The C struct is packed and ends with a `uint8_t payload[0]`,
    /// which contributes nothing.
    pub const LEN: usize = 16;

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
            target_node_id: u64::from_le_bytes(buf[0..8].try_into().expect("8 bytes")),
            source_node_id: u64::from_le_bytes(buf[8..16].try_into().expect("8 bytes")),
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than [`Self::LEN`].
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        let buf = buf.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
        buf[0..8].copy_from_slice(&self.target_node_id.to_le_bytes());
        buf[8..16].copy_from_slice(&self.source_node_id.to_le_bytes());
        Ok(())
    }

    /// Whether a message with this header travels no further — the first of
    /// `bcmp_time_process_time_message`'s two tests, which all three types
    /// share.
    ///
    /// A message failing this is re-flooded by `bcmp_ll_forward` and is not
    /// looked at again.
    #[must_use]
    pub fn is_local(&self, our_node_id: u64) -> bool {
        self.target_node_id == our_node_id || self.target_node_id == 0
    }

    /// Whether this message names `our_node_id` exactly — the second test,
    /// which the request and the response make and the set does not.
    #[must_use]
    pub fn is_addressed_exactly_to(&self, our_node_id: u64) -> bool {
        self.target_node_id == our_node_id
    }
}

/// `BcmpSystemTimeRequest` (`0x10`): ask a node for its clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SystemTimeRequest {
    /// Who it is for, and who is asking.
    pub header: SystemTimeHeader,
}

impl SystemTimeRequest {
    /// Wire size.
    pub const LEN: usize = SystemTimeHeader::LEN;

    /// Decode from the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        Ok(Self {
            header: SystemTimeHeader::decode(buf)?,
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        self.header.encode(buf)
    }

    /// Whether this node should answer the request.
    ///
    /// **Not** `is_local`: a request addressed to zero reaches the `switch` and
    /// is then dropped by the exact-match test, so broadcasting a time request
    /// asks nobody. That is divergence #27, and it is why this is a named
    /// predicate rather than a comparison written at the call site.
    #[must_use]
    pub fn is_for(&self, our_node_id: u64) -> bool {
        self.header.is_addressed_exactly_to(our_node_id)
    }
}

/// `BcmpSystemTimeResponse` (`0x11`): a node's answer, in UTC microseconds.
///
/// `bcmp_time_process_time_message` only logs this, so a C node does nothing
/// observable with one. A port that wants the time has to read it itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SystemTimeResponse {
    /// Who it is for, and who is answering.
    pub header: SystemTimeHeader,
    /// Microseconds since the Unix epoch, as the answering node's RTC reads.
    pub utc_time_us: u64,
}

/// `BcmpSystemTimeSet` (`0x12`): tell a node what time it is.
///
/// Identical on the wire to [`SystemTimeResponse`], and a separate type
/// because the C has two typedefs and because only this one is honoured when
/// broadcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SystemTimeSet {
    /// Who it is for, and who is setting it.
    pub header: SystemTimeHeader,
    /// Microseconds since the Unix epoch to adopt.
    pub utc_time_us: u64,
}

/// The two 24-byte bodies, which differ only in what they mean.
macro_rules! timestamped {
    ($name:ident) => {
        impl $name {
            /// Wire size.
            pub const LEN: usize = SystemTimeHeader::LEN + 8;

            /// Decode from the first [`Self::LEN`] bytes of `buf`.
            ///
            /// # Errors
            ///
            /// [`BmWireError::Truncated`] if `buf` is too short.
            pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
                let buf: &[u8; Self::LEN] = buf
                    .get(..Self::LEN)
                    .and_then(|b| b.try_into().ok())
                    .ok_or(BmWireError::Truncated)?;
                Ok(Self {
                    header: SystemTimeHeader::decode(buf)?,
                    utc_time_us: u64::from_le_bytes(
                        buf[SystemTimeHeader::LEN..Self::LEN]
                            .try_into()
                            .expect("8 bytes"),
                    ),
                })
            }

            /// Encode into the first [`Self::LEN`] bytes of `buf`.
            ///
            /// # Errors
            ///
            /// [`BmWireError::Truncated`] if `buf` is too short.
            pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
                let buf = buf.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
                self.header.encode(buf)?;
                buf[SystemTimeHeader::LEN..Self::LEN]
                    .copy_from_slice(&self.utc_time_us.to_le_bytes());
                Ok(())
            }
        }
    };
}

timestamped!(SystemTimeResponse);
timestamped!(SystemTimeSet);

impl SystemTimeResponse {
    /// Whether this node is the one being answered.
    ///
    /// Zero fails, exactly as it does for a request: `bcmp_time_process_time_message`
    /// makes the same exact-match test for `0x11`.
    #[must_use]
    pub fn is_for(&self, our_node_id: u64) -> bool {
        self.header.is_addressed_exactly_to(our_node_id)
    }
}

impl SystemTimeSet {
    /// Whether this node should adopt the time.
    ///
    /// The odd one out: the `switch` arm for `0x12` makes no exact-match test,
    /// so zero — which got this far by being a broadcast — is honoured. A
    /// single `0x12` to `FF02::1` with `target_node_id == 0` therefore sets the
    /// clock of every node on the link, and then every one of them answers.
    #[must_use]
    pub fn is_for(&self, our_node_id: u64) -> bool {
        self.header.is_local(our_node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_the_packed_c_structs() {
        assert_eq!(SystemTimeHeader::LEN, 16);
        assert_eq!(SystemTimeRequest::LEN, 16);
        assert_eq!(SystemTimeResponse::LEN, 24);
        assert_eq!(SystemTimeSet::LEN, 24);
    }

    #[test]
    fn a_request_round_trips_little_endian() {
        let body = [
            0x78, 0x56, 0x34, 0x12, 0x00, 0xEE, 0xFF, 0xC0, // target
            0x11, 0x00, 0xAA, 0x55, 0x00, 0x00, 0x00, 0x00, // source
        ];
        let request = SystemTimeRequest::decode(&body).unwrap();
        assert_eq!(request.header.target_node_id, 0xC0FF_EE00_1234_5678);
        assert_eq!(request.header.source_node_id, 0x0000_0000_55AA_0011);

        let mut out = [0u8; SystemTimeRequest::LEN];
        request.encode(&mut out).unwrap();
        assert_eq!(out, body);
    }

    #[test]
    fn a_response_carries_its_timestamp_after_the_header() {
        // 2026-09-21T00:00:00Z, to the microsecond.
        let utc_time_us: u64 = 1_789_948_800_000_000;
        let response = SystemTimeResponse {
            header: SystemTimeHeader {
                target_node_id: 0x0000_0000_55AA_0011,
                source_node_id: 0xC0FF_EE00_1234_5678,
            },
            utc_time_us,
        };
        let mut body = [0u8; SystemTimeResponse::LEN];
        response.encode(&mut body).unwrap();
        assert_eq!(
            &body[SystemTimeHeader::LEN..],
            &utc_time_us.to_le_bytes(),
            "the timestamp is little-endian, 16 bytes in"
        );
        assert_eq!(SystemTimeResponse::decode(&body).unwrap(), response);

        // And the set message is the same bytes, which is why they are
        // distinguished only by the header's type field.
        let set = SystemTimeSet {
            header: response.header,
            utc_time_us,
        };
        let mut same = [0u8; SystemTimeSet::LEN];
        set.encode(&mut same).unwrap();
        assert_eq!(same, body);
    }

    /// The asymmetry divergence #27 records, as a table.
    #[test]
    fn zero_is_a_broadcast_for_a_set_and_a_dead_letter_for_the_other_two() {
        const US: u64 = 0xC0FF_EE00_1234_5678;
        const THEM: u64 = 0x0000_0000_55AA_0011;

        for target in [0, US, THEM] {
            let header = SystemTimeHeader {
                target_node_id: target,
                source_node_id: THEM,
            };
            // The forwarding test is the same for all three.
            assert_eq!(header.is_local(US), target != THEM, "is_local({target:#x})");

            let request = SystemTimeRequest { header };
            let response = SystemTimeResponse {
                header,
                utc_time_us: 0,
            };
            let set = SystemTimeSet {
                header,
                utc_time_us: 0,
            };

            assert_eq!(request.is_for(US), target == US, "request({target:#x})");
            assert_eq!(response.is_for(US), target == US, "response({target:#x})");
            assert_eq!(set.is_for(US), target != THEM, "set({target:#x})");
        }

        // Spelled out: the one row that is not the same in all three columns.
        let broadcast = SystemTimeHeader {
            target_node_id: 0,
            source_node_id: THEM,
        };
        assert!(
            !SystemTimeRequest { header: broadcast }.is_for(US),
            "a broadcast time request is answered by nobody"
        );
        assert!(
            SystemTimeSet {
                header: broadcast,
                utc_time_us: 0
            }
            .is_for(US),
            "a broadcast time set is honoured by everybody"
        );
    }

    #[test]
    fn short_buffers_are_rejected_at_every_length() {
        for len in 0..SystemTimeRequest::LEN {
            assert_eq!(
                SystemTimeRequest::decode(&[0u8; SystemTimeRequest::LEN][..len]),
                Err(BmWireError::Truncated)
            );
            let mut buf = [0u8; SystemTimeRequest::LEN];
            assert_eq!(
                SystemTimeRequest::default().encode(&mut buf[..len]),
                Err(BmWireError::Truncated)
            );
        }
        for len in 0..SystemTimeResponse::LEN {
            assert_eq!(
                SystemTimeResponse::decode(&[0u8; SystemTimeResponse::LEN][..len]),
                Err(BmWireError::Truncated)
            );
            assert_eq!(
                SystemTimeSet::decode(&[0u8; SystemTimeSet::LEN][..len]),
                Err(BmWireError::Truncated)
            );
            let mut buf = [0u8; SystemTimeSet::LEN];
            assert_eq!(
                SystemTimeSet::default().encode(&mut buf[..len]),
                Err(BmWireError::Truncated)
            );
        }
    }

    /// Trailing bytes are the C's business too: `data.size` is whatever
    /// arrived, and the cast reads the first 24 bytes and stops.
    #[test]
    fn trailing_bytes_are_ignored() {
        let body = [0xAAu8; SystemTimeSet::LEN + 9];
        let set = SystemTimeSet::decode(&body).unwrap();
        assert_eq!(set.header.target_node_id, 0xAAAA_AAAA_AAAA_AAAA);
        assert_eq!(set.utc_time_us, 0xAAAA_AAAA_AAAA_AAAA);
    }
}
