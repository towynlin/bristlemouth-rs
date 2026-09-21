//! `BcmpEchoRequest` and `BcmpEchoReply`, ported from `bcmp/messages.h` and
//! `bcmp/ping.c`.
//!
//! Ping is the smallest complete exchange BCMP has: a node asks, a node
//! answers with the same bytes back. Both message types are registered
//! `{false, false}` by `ping_init`, so neither carries a header sequence
//! number and neither is matched by `packet.c`'s outstanding-request list —
//! whatever correlation there is, `bcmp/ping.c` does itself, out of the two
//! statics it keeps.
//!
//! # The two structs are one struct
//!
//! `BcmpEchoRequest` and `BcmpEchoReply` are byte-for-byte identical: a 64-bit
//! node id, three 16-bit fields, then the payload. Only the first field's
//! *meaning* differs — the request names who should answer, the reply names
//! who did. `bcmp_process_ping_request` exploits that directly: it overwrites
//! `target_node_id` with this node's id and casts the request buffer to a
//! `BcmpEchoReply` in place. [`EchoRequest::into_reply`] is that cast, with
//! the substitution written down rather than implied by a pointer.
//!
//! # Lengths are checked here and nowhere in the C
//!
//! `bcmp_process_ping_request` echoes `sizeof(BcmpEchoReply) + payload_len`
//! bytes out of the received frame, and `bcmp_process_ping_reply` `memcmp`s
//! `payload_len` of them, in both cases taking the length from the frame and
//! never comparing it against `BcmpProcessData.size`, which is right there.
//! [`EchoRequest::decode`] and [`EchoReply::decode`] validate it against the
//! buffer instead. See divergence #29: this is a domain limit, not a
//! behaviour the port reproduces, because there is no defined C behaviour to
//! reproduce.

use crate::BmWireError;

/// Bytes before the payload in both messages, `sizeof(BcmpEchoRequest)` and
/// `sizeof(BcmpEchoReply)` alike.
pub const ECHO_HEADER_LEN: usize = 14;

/// Longest payload either message can declare, since `payload_len` is a `u16`.
pub const MAX_ECHO_PAYLOAD: usize = u16::MAX as usize;

/// Decode the part both messages share: the leading `u64`, the three `u16`s,
/// and a payload whose declared length is checked against `buf`.
fn decode_parts(buf: &[u8]) -> Result<(u64, u16, u16, &[u8]), BmWireError> {
    let head: &[u8; ECHO_HEADER_LEN] = buf
        .get(..ECHO_HEADER_LEN)
        .and_then(|b| b.try_into().ok())
        .ok_or(BmWireError::Truncated)?;
    let node_id = u64::from_le_bytes(head[0..8].try_into().expect("8 bytes"));
    let id = u16::from_le_bytes([head[8], head[9]]);
    let seq_num = u16::from_le_bytes([head[10], head[11]]);
    let payload_len = usize::from(u16::from_le_bytes([head[12], head[13]]));

    // The check bm_core does not do.
    let payload = buf
        .get(ECHO_HEADER_LEN..ECHO_HEADER_LEN + payload_len)
        .ok_or(BmWireError::Truncated)?;
    Ok((node_id, id, seq_num, payload))
}

/// Encode the part both messages share, returning the bytes written.
fn encode_parts(
    buf: &mut [u8],
    node_id: u64,
    id: u16,
    seq_num: u16,
    payload: &[u8],
) -> Result<usize, BmWireError> {
    if payload.len() > MAX_ECHO_PAYLOAD {
        return Err(BmWireError::Invalid);
    }
    let end = ECHO_HEADER_LEN + payload.len();
    let buf = buf.get_mut(..end).ok_or(BmWireError::Truncated)?;
    buf[0..8].copy_from_slice(&node_id.to_le_bytes());
    buf[8..10].copy_from_slice(&id.to_le_bytes());
    buf[10..12].copy_from_slice(&seq_num.to_le_bytes());
    buf[12..14].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    buf[ECHO_HEADER_LEN..end].copy_from_slice(payload);
    Ok(end)
}

/// `BcmpEchoRequest`: ask one node, or every node, to echo these bytes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EchoRequest<'a> {
    /// Node that should answer, or zero for all of them.
    pub target_node_id: u64,
    /// Identifier for a stream of pings.
    ///
    /// bm_core sets it to `(uint16_t)node_id()` — the sender's node id
    /// truncated to sixteen bits — and the comment at `ping.c:42` says it
    /// should be a random number instead. It is what the requester matches a
    /// reply on, so two nodes sharing the low sixteen bits of their ids share
    /// this. See divergence #30.
    pub id: u16,
    /// Counter within that stream, from `ping.c`'s own `BCMP_SEQ`.
    ///
    /// Nothing ever reads it back: the reply echoes it, and
    /// `bcmp_process_ping_reply` does not compare it. It is also **not** the
    /// BCMP header's sequence number — ping is registered unsequenced, so the
    /// header carries zero — and it counts in a sequence space of its own,
    /// separate from `packet.c`'s `message_count`.
    pub seq_num: u16,
    /// Bytes the answering node must send back unchanged.
    pub payload: &'a [u8],
}

impl<'a> EchoRequest<'a> {
    /// Size of the fixed part, `sizeof(BcmpEchoRequest)`.
    pub const HEADER_LEN: usize = ECHO_HEADER_LEN;

    /// Longest payload the `u16` length field can describe.
    pub const MAX_PAYLOAD_LEN: usize = MAX_ECHO_PAYLOAD;

    /// Decode a request, borrowing its payload from `buf`.
    ///
    /// Trailing bytes past the declared payload are ignored, as they are by
    /// the C.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than the fixed part, or
    /// shorter than the payload length it declares.
    pub fn decode(buf: &'a [u8]) -> Result<Self, BmWireError> {
        let (target_node_id, id, seq_num, payload) = decode_parts(buf)?;
        Ok(Self {
            target_node_id,
            id,
            seq_num,
            payload,
        })
    }

    /// Bytes [`Self::encode`] will write.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        Self::HEADER_LEN + self.payload.len()
    }

    /// Encode into `buf`, returning how many bytes were written.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    /// [`BmWireError::Invalid`] if the payload is longer than
    /// [`Self::MAX_PAYLOAD_LEN`], since the length field could not describe it.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        encode_parts(
            buf,
            self.target_node_id,
            self.id,
            self.seq_num,
            self.payload,
        )
    }

    /// The reply bm_core sends to this request, from a node with `node_id`.
    ///
    /// `bcmp_process_ping_request` does this by assignment and a cast:
    ///
    /// ```c
    /// echo_req->target_node_id = node_id();
    /// err = bcmp_send_ping_reply((BcmpEchoReply *)echo_req, data.dst, echo_req->seq_num);
    /// ```
    ///
    /// so every other field, the payload included, rides back out exactly as
    /// it arrived. The caller decides whether the request was addressed to it;
    /// this only performs the substitution.
    #[must_use]
    pub fn into_reply(self, node_id: u64) -> EchoReply<'a> {
        EchoReply {
            node_id,
            id: self.id,
            seq_num: self.seq_num,
            payload: self.payload,
        }
    }
}

/// `BcmpEchoReply`: the same bytes coming back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EchoReply<'a> {
    /// Node that answered.
    ///
    /// **Not checked by the requester.** `bcmp_process_ping_reply` never looks
    /// at it, so a reply from any node answers a ping aimed at one particular
    /// node — divergence #30.
    pub node_id: u64,
    /// The request's [`EchoRequest::id`], echoed.
    pub id: u16,
    /// The request's [`EchoRequest::seq_num`], echoed. Also not checked.
    pub seq_num: u16,
    /// The request's payload, echoed.
    pub payload: &'a [u8],
}

impl<'a> EchoReply<'a> {
    /// Size of the fixed part, `sizeof(BcmpEchoReply)` — the same as
    /// [`EchoRequest::HEADER_LEN`], which is what lets the C cast one to the
    /// other in place.
    pub const HEADER_LEN: usize = ECHO_HEADER_LEN;

    /// Longest payload the `u16` length field can describe.
    pub const MAX_PAYLOAD_LEN: usize = MAX_ECHO_PAYLOAD;

    /// Decode a reply, borrowing its payload from `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than the fixed part, or
    /// shorter than the payload length it declares.
    pub fn decode(buf: &'a [u8]) -> Result<Self, BmWireError> {
        let (node_id, id, seq_num, payload) = decode_parts(buf)?;
        Ok(Self {
            node_id,
            id,
            seq_num,
            payload,
        })
    }

    /// Bytes [`Self::encode`] will write.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        Self::HEADER_LEN + self.payload.len()
    }

    /// Encode into `buf`, returning how many bytes were written.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    /// [`BmWireError::Invalid`] if the payload is longer than
    /// [`Self::MAX_PAYLOAD_LEN`].
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        encode_parts(buf, self.node_id, self.id, self.seq_num, self.payload)
    }

    /// Whether `bcmp_process_ping_reply` would accept this reply, given the
    /// requester's truncated node id and the payload it is still expecting.
    ///
    /// `expected_payload` is `EXPECTED_PAYLOAD` and its length together:
    /// `None` is the C's `NULL`, which it only ever is while
    /// `EXPECTED_PAYLOAD_LEN` is zero, because `bcmp_send_ping_request` clears
    /// and sets the two as a pair.
    ///
    /// The three things the C compares are the payload length, the id, and the
    /// payload bytes. The three it does not are the reply's `seq_num`, its
    /// `node_id`, and the address it arrived from; see divergence #30 for what
    /// that admits.
    #[must_use]
    pub fn answers(&self, our_id: u16, expected_payload: Option<&[u8]>) -> bool {
        let expected_len = expected_payload.map_or(0, <[u8]>::len);
        expected_len == self.payload.len()
            && our_id == self.id
            // `if (EXPECTED_PAYLOAD != NULL)`: a null pointer skips the
            // comparison rather than failing it.
            && expected_payload.is_none_or(|expected| expected == self.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_the_packed_c_structs() {
        assert_eq!(EchoRequest::HEADER_LEN, 14);
        assert_eq!(EchoReply::HEADER_LEN, 14);
        assert_eq!(
            EchoRequest::HEADER_LEN,
            EchoReply::HEADER_LEN,
            "bcmp_process_ping_request casts one to the other in place, which \
             only works because the layouts are identical"
        );
    }

    #[test]
    fn a_request_round_trips_through_its_own_bytes() {
        let request = EchoRequest {
            target_node_id: 0xC0FF_EE00_1234_5678,
            id: 0x5678,
            seq_num: 7,
            payload: b"hello ping",
        };
        let mut buf = [0u8; 64];
        let len = request.encode(&mut buf).unwrap();
        assert_eq!(len, EchoRequest::HEADER_LEN + 10);
        assert_eq!(
            &buf[..14],
            &[
                0x78, 0x56, 0x34, 0x12, 0x00, 0xEE, 0xFF, 0xC0, // target_node_id
                0x78, 0x56, // id
                0x07, 0x00, // seq_num
                0x0A, 0x00, // payload_len
            ],
            "little-endian, packed, no padding"
        );
        assert_eq!(EchoRequest::decode(&buf[..len]).unwrap(), request);
    }

    #[test]
    fn a_reply_round_trips_through_its_own_bytes() {
        let reply = EchoReply {
            node_id: 0x0000_0000_55AA_0011,
            id: 0x5678,
            seq_num: u16::MAX,
            payload: &[],
        };
        let mut buf = [0u8; EchoReply::HEADER_LEN];
        assert_eq!(reply.encode(&mut buf).unwrap(), EchoReply::HEADER_LEN);
        assert_eq!(EchoReply::decode(&buf).unwrap(), reply);
    }

    /// The request and the reply are the same fourteen bytes with one field
    /// renamed, which is the whole basis of the C's in-place cast.
    #[test]
    fn a_request_and_a_reply_with_the_same_fields_encode_the_same_bytes() {
        let payload = b"abcd";
        let request = EchoRequest {
            target_node_id: 42,
            id: 9,
            seq_num: 3,
            payload,
        };
        let reply = EchoReply {
            node_id: 42,
            id: 9,
            seq_num: 3,
            payload,
        };
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        assert_eq!(
            request.encode(&mut a).unwrap(),
            reply.encode(&mut b).unwrap()
        );
        assert_eq!(a, b);
    }

    #[test]
    fn into_reply_substitutes_only_the_first_field() {
        let request = EchoRequest {
            target_node_id: 0,
            id: 0xBEEF,
            seq_num: 12,
            payload: b"payload bytes",
        };
        let reply = request.into_reply(0xC0FF_EE00_1234_5678);
        assert_eq!(reply.node_id, 0xC0FF_EE00_1234_5678);
        assert_eq!(reply.id, request.id);
        assert_eq!(reply.seq_num, request.seq_num);
        assert_eq!(reply.payload, request.payload);
    }

    #[test]
    fn declared_payload_lengths_are_checked_against_the_buffer() {
        let mut body = [0u8; ECHO_HEADER_LEN + 4];
        body[12..14].copy_from_slice(&4u16.to_le_bytes());
        assert_eq!(EchoRequest::decode(&body).unwrap().payload.len(), 4);

        // One byte more than arrived. bm_core would echo past the frame here.
        body[12..14].copy_from_slice(&5u16.to_le_bytes());
        assert_eq!(EchoRequest::decode(&body), Err(BmWireError::Truncated));
        assert_eq!(EchoReply::decode(&body), Err(BmWireError::Truncated));

        // The worst case: a saturated length on a header-only body, which
        // would have the C read 64 KiB past the frame.
        let mut minimal = [0u8; ECHO_HEADER_LEN];
        minimal[12..14].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(EchoRequest::decode(&minimal), Err(BmWireError::Truncated));
        assert_eq!(EchoReply::decode(&minimal), Err(BmWireError::Truncated));
    }

    #[test]
    fn trailing_bytes_past_the_payload_are_ignored() {
        let mut body = [0xA5u8; ECHO_HEADER_LEN + 16];
        body[..ECHO_HEADER_LEN].fill(0);
        body[12..14].copy_from_slice(&2u16.to_le_bytes());
        let request = EchoRequest::decode(&body).unwrap();
        assert_eq!(request.payload, &[0xA5, 0xA5]);
        assert_eq!(request.encoded_len(), ECHO_HEADER_LEN + 2);
    }

    #[test]
    fn short_buffers_are_rejected_at_every_length() {
        for len in 0..ECHO_HEADER_LEN {
            assert_eq!(
                EchoRequest::decode(&[0u8; ECHO_HEADER_LEN][..len]),
                Err(BmWireError::Truncated)
            );
            assert_eq!(
                EchoReply::decode(&[0u8; ECHO_HEADER_LEN][..len]),
                Err(BmWireError::Truncated)
            );
            let mut buf = [0u8; ECHO_HEADER_LEN];
            assert_eq!(
                EchoRequest {
                    target_node_id: 0,
                    id: 0,
                    seq_num: 0,
                    payload: &[],
                }
                .encode(&mut buf[..len]),
                Err(BmWireError::Truncated)
            );
        }
    }

    // -----------------------------------------------------------------------
    // The acceptance test, which is the whole of ping.c's requester half.
    // -----------------------------------------------------------------------

    fn reply(id: u16, payload: &[u8]) -> EchoReply<'_> {
        EchoReply {
            node_id: 0x0000_0000_55AA_0011,
            id,
            seq_num: 0,
            payload,
        }
    }

    #[test]
    fn a_reply_matching_length_id_and_payload_is_accepted() {
        assert!(reply(0x5678, b"ping").answers(0x5678, Some(b"ping")));
    }

    #[test]
    fn a_wrong_id_a_wrong_length_or_wrong_bytes_are_all_refused() {
        assert!(!reply(0x0001, b"ping").answers(0x5678, Some(b"ping")), "id");
        assert!(
            !reply(0x5678, b"pin").answers(0x5678, Some(b"ping")),
            "length"
        );
        assert!(
            !reply(0x5678, b"pong").answers(0x5678, Some(b"ping")),
            "bytes"
        );
    }

    /// With no payload outstanding the C compares nothing but the length and
    /// the id — so an empty reply is accepted even though no ping with an
    /// empty payload was necessarily ever sent.
    #[test]
    fn an_empty_reply_is_accepted_whenever_nothing_is_expected() {
        assert!(reply(0x5678, b"").answers(0x5678, None));
        assert!(!reply(0x5678, b"x").answers(0x5678, None));
        // And the same through the other spelling of "nothing expected",
        // which is what a ping with an empty payload leaves behind.
        assert!(reply(0x5678, b"").answers(0x5678, Some(b"")));
    }

    /// Divergence #30: the reply's own `seq_num` and `node_id` are never
    /// compared, so a reply from the wrong node, carrying the wrong counter,
    /// still answers.
    #[test]
    fn neither_the_sequence_number_nor_the_answering_node_is_checked() {
        let stale = EchoReply {
            node_id: 0xDEAD_BEEF_DEAD_BEEF,
            id: 0x5678,
            seq_num: 999,
            payload: b"ping",
        };
        assert!(stale.answers(0x5678, Some(b"ping")));
    }
}
