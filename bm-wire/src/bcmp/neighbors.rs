//! `BcmpNeighborTableRequest` and `BcmpNeighborTableReply`, ported from
//! `bcmp/messages.h`, `bcmp/neighbors.c` and `integrations/topology.c`.
//!
//! The reply is an 11-byte head followed by two counted arrays: `port_len`
//! [`PortInfo`] entries, then `neighbor_len` [`NeighborInfo`] entries. Both
//! counts come off the wire, and neither is checked against the size of the
//! message that carried them — see divergence #14. [`NeighborTableReply`]
//! borrows the two arrays out of the frame, so the bounds are checked once, at
//! decode, and the iterators cannot walk past them.

use crate::BmWireError;

/// `BcmpNeighborTableRequest`: ask one node, or every node, for its neighbours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NeighborTableRequest {
    /// Node to answer, or zero for all of them.
    pub target_node_id: u64,
}

impl NeighborTableRequest {
    /// Wire size.
    pub const LEN: usize = 8;

    /// Decode from the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let bytes: [u8; 8] = buf
            .get(..Self::LEN)
            .and_then(|b| b.try_into().ok())
            .ok_or(BmWireError::Truncated)?;
        Ok(Self {
            target_node_id: u64::from_le_bytes(bytes),
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        let buf = buf.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
        buf.copy_from_slice(&self.target_node_id.to_le_bytes());
        Ok(())
    }
}

/// `BcmpPortInfo`: one local port's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PortInfo {
    /// Link state, as the raw byte.
    ///
    /// The C field is a `bool`, so any value other than 0 or 1 is an
    /// out-of-range read there. Keeping the byte means a decoded reply
    /// re-encodes to the same bytes; use [`Self::is_up`] for the meaning.
    pub state: u8,
    /// Port type, mapping to `bm_port_type_e`. `bcmp_send_neighbor_table`
    /// never sets it, so it is always zero on the wire today.
    pub port_type: u8,
}

impl PortInfo {
    /// Wire size.
    pub const LEN: usize = 2;

    /// Whether the link is up.
    #[must_use]
    pub const fn is_up(&self) -> bool {
        self.state != 0
    }
}

/// `BcmpNeighborInfo`: one neighbour, as the replying node sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NeighborInfo {
    /// The neighbour's node id.
    pub node_id: u64,
    /// Which of the replying node's ports the neighbour is on.
    pub port: u8,
    /// Whether the neighbour is currently online, as the raw byte.
    pub online: u8,
}

impl NeighborInfo {
    /// Wire size.
    pub const LEN: usize = 10;

    /// Whether the neighbour is online.
    #[must_use]
    pub const fn is_online(&self) -> bool {
        self.online != 0
    }

    fn decode(buf: &[u8; Self::LEN]) -> Self {
        Self {
            node_id: u64::from_le_bytes(buf[0..8].try_into().expect("8 bytes")),
            port: buf[8],
            online: buf[9],
        }
    }

    fn encode_into(&self, buf: &mut [u8; Self::LEN]) {
        buf[0..8].copy_from_slice(&self.node_id.to_le_bytes());
        buf[8] = self.port;
        buf[9] = self.online;
    }
}

/// `BcmpNeighborTableReply`, borrowed from the frame it arrived in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeighborTableReply<'a> {
    /// Node id of the replying node.
    pub node_id: u64,
    ports: &'a [u8],
    neighbors: &'a [u8],
}

impl<'a> NeighborTableReply<'a> {
    /// Size of the fixed part. `sizeof(BcmpNeighborTableReply)`.
    pub const HEADER_LEN: usize = 11;

    /// Decode a reply, borrowing its two arrays from `buf`.
    ///
    /// Trailing bytes past the declared arrays are ignored.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than the fixed part, or
    /// shorter than the entry counts it declares.
    pub fn decode(buf: &'a [u8]) -> Result<Self, BmWireError> {
        let head = buf.get(..Self::HEADER_LEN).ok_or(BmWireError::Truncated)?;
        let node_id = u64::from_le_bytes(head[0..8].try_into().expect("8 bytes"));
        let port_len = usize::from(head[8]);
        let neighbor_len = usize::from(u16::from_le_bytes([head[9], head[10]]));

        // The check neither bm_core's neighbour code nor topology.c does.
        let ports_bytes = port_len * PortInfo::LEN;
        let neighbors_bytes = neighbor_len * NeighborInfo::LEN;
        let body = buf
            .get(Self::HEADER_LEN..Self::HEADER_LEN + ports_bytes + neighbors_bytes)
            .ok_or(BmWireError::Truncated)?;

        Ok(Self {
            node_id,
            ports: &body[..ports_bytes],
            neighbors: &body[ports_bytes..],
        })
    }

    /// Number of local ports described.
    #[must_use]
    pub fn port_count(&self) -> u8 {
        (self.ports.len() / PortInfo::LEN) as u8
    }

    /// Number of neighbours described.
    #[must_use]
    pub fn neighbor_count(&self) -> u16 {
        (self.neighbors.len() / NeighborInfo::LEN) as u16
    }

    /// The local ports, in order.
    pub fn ports(&self) -> impl Iterator<Item = PortInfo> + '_ {
        self.ports
            .as_chunks::<{ PortInfo::LEN }>()
            .0
            .iter()
            .map(|c| PortInfo {
                state: c[0],
                port_type: c[1],
            })
    }

    /// The neighbours, in order.
    pub fn neighbors(&self) -> impl Iterator<Item = NeighborInfo> + '_ {
        self.neighbors
            .as_chunks::<{ NeighborInfo::LEN }>()
            .0
            .iter()
            .map(NeighborInfo::decode)
    }

    /// Bytes [`Self::encode`] will write.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        Self::HEADER_LEN + self.ports.len() + self.neighbors.len()
    }

    /// Re-encode into `buf`, returning how many bytes were written.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        let end = self.encoded_len();
        let buf = buf.get_mut(..end).ok_or(BmWireError::Truncated)?;
        write_head(buf, self.node_id, self.port_count(), self.neighbor_count());
        let split = Self::HEADER_LEN + self.ports.len();
        buf[Self::HEADER_LEN..split].copy_from_slice(self.ports);
        buf[split..end].copy_from_slice(self.neighbors);
        Ok(end)
    }
}

fn write_head(buf: &mut [u8], node_id: u64, port_len: u8, neighbor_len: u16) {
    buf[0..8].copy_from_slice(&node_id.to_le_bytes());
    buf[8] = port_len;
    buf[9..11].copy_from_slice(&neighbor_len.to_le_bytes());
}

/// Bytes [`encode_neighbor_table_reply`] will write for these lists.
#[must_use]
pub const fn neighbor_table_reply_len(port_count: usize, neighbor_count: usize) -> usize {
    NeighborTableReply::HEADER_LEN + port_count * PortInfo::LEN + neighbor_count * NeighborInfo::LEN
}

/// Build a reply from owned lists, returning how many bytes were written.
///
/// # Errors
///
/// [`BmWireError::Invalid`] if there are more ports or neighbours than the
/// count fields can describe. [`BmWireError::Truncated`] if `buf` is too short.
pub fn encode_neighbor_table_reply(
    buf: &mut [u8],
    node_id: u64,
    ports: &[PortInfo],
    neighbors: &[NeighborInfo],
) -> Result<usize, BmWireError> {
    if ports.len() > usize::from(u8::MAX) || neighbors.len() > usize::from(u16::MAX) {
        return Err(BmWireError::Invalid);
    }
    let end = neighbor_table_reply_len(ports.len(), neighbors.len());
    let buf = buf.get_mut(..end).ok_or(BmWireError::Truncated)?;

    write_head(buf, node_id, ports.len() as u8, neighbors.len() as u16);
    let mut at = NeighborTableReply::HEADER_LEN;
    for port in ports {
        buf[at] = port.state;
        buf[at + 1] = port.port_type;
        at += PortInfo::LEN;
    }
    for neighbor in neighbors {
        let slot: &mut [u8; NeighborInfo::LEN] = (&mut buf[at..at + NeighborInfo::LEN])
            .try_into()
            .expect("slice is NeighborInfo::LEN");
        neighbor.encode_into(slot);
        at += NeighborInfo::LEN;
    }
    Ok(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_the_packed_c_structs() {
        assert_eq!(NeighborTableReply::HEADER_LEN, 11);
        assert_eq!(PortInfo::LEN, 2);
        assert_eq!(NeighborInfo::LEN, 10);
    }

    #[test]
    fn a_two_port_reply_round_trips() {
        let ports = [
            PortInfo {
                state: 1,
                port_type: 0,
            },
            PortInfo {
                state: 0,
                port_type: 0,
            },
        ];
        let neighbors = [
            NeighborInfo {
                node_id: 0xDEAD_BEEF_1234_5678,
                port: 1,
                online: 1,
            },
            NeighborInfo {
                node_id: 0x0000_0000_55AA_0011,
                port: 2,
                online: 0,
            },
        ];

        let mut buf = [0u8; 64];
        let len = encode_neighbor_table_reply(&mut buf, 0xC0FF_EE00_1234_5678, &ports, &neighbors)
            .unwrap();
        assert_eq!(len, neighbor_table_reply_len(2, 2));
        assert_eq!(len, 11 + 4 + 20);

        let reply = NeighborTableReply::decode(&buf[..len]).unwrap();
        assert_eq!(reply.node_id, 0xC0FF_EE00_1234_5678);
        assert_eq!(reply.port_count(), 2);
        assert_eq!(reply.neighbor_count(), 2);
        assert!(reply.ports().eq(ports.iter().copied()));
        assert!(reply.neighbors().eq(neighbors.iter().copied()));
        assert!(reply.ports().next().unwrap().is_up());
        assert!(!reply.ports().nth(1).unwrap().is_up());

        let mut again = [0u8; 64];
        assert_eq!(reply.encode(&mut again).unwrap(), len);
        assert_eq!(&again[..len], &buf[..len]);
    }

    #[test]
    fn an_empty_table_is_just_the_header() {
        let mut buf = [0u8; 16];
        let len = encode_neighbor_table_reply(&mut buf, 7, &[], &[]).unwrap();
        assert_eq!(len, NeighborTableReply::HEADER_LEN);
        let reply = NeighborTableReply::decode(&buf[..len]).unwrap();
        assert_eq!(reply.port_count(), 0);
        assert_eq!(reply.neighbor_count(), 0);
        assert_eq!(reply.ports().count(), 0);
        assert_eq!(reply.neighbors().count(), 0);
    }

    /// The counts are attacker-controlled, and the C multiplies them out and
    /// copies without ever looking at how many bytes arrived.
    #[test]
    fn declared_counts_are_checked_against_the_buffer() {
        let mut body = [0u8; NeighborTableReply::HEADER_LEN + 4];
        body[8] = 2; // two ports, exactly the four trailing bytes
        assert!(NeighborTableReply::decode(&body).is_ok());

        body[8] = 3; // six bytes' worth, only four arrived
        assert_eq!(
            NeighborTableReply::decode(&body),
            Err(BmWireError::Truncated)
        );

        // The worst case: both counts saturated on a minimum-size body. The C
        // would compute 255*2 + 65535*10 and memcpy 655 850 bytes out of a
        // frame that carried eleven.
        let mut minimal = [0u8; NeighborTableReply::HEADER_LEN];
        minimal[8] = u8::MAX;
        minimal[9..11].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            NeighborTableReply::decode(&minimal),
            Err(BmWireError::Truncated)
        );
    }

    #[test]
    fn trailing_bytes_past_the_arrays_are_ignored() {
        let mut buf = [0u8; 64];
        let len = encode_neighbor_table_reply(
            &mut buf,
            1,
            &[PortInfo {
                state: 1,
                port_type: 0,
            }],
            &[],
        )
        .unwrap();
        buf[len..len + 8].fill(0xA5);
        let reply = NeighborTableReply::decode(&buf[..len + 8]).unwrap();
        assert_eq!(reply.encoded_len(), len);
        assert_eq!(reply.port_count(), 1);
    }

    #[test]
    fn a_port_state_byte_that_is_not_zero_or_one_survives_a_round_trip() {
        // The C reads this field as a `bool`, for which any other value is out
        // of range. Keeping the raw byte means re-encoding is byte-exact.
        let ports = [PortInfo {
            state: 0x42,
            port_type: 0x99,
        }];
        let mut buf = [0u8; 32];
        let len = encode_neighbor_table_reply(&mut buf, 0, &ports, &[]).unwrap();
        let reply = NeighborTableReply::decode(&buf[..len]).unwrap();
        assert_eq!(reply.ports().next().unwrap(), ports[0]);
        assert!(reply.ports().next().unwrap().is_up());
    }

    #[test]
    fn short_buffers_are_rejected_at_every_length() {
        for len in 0..NeighborTableReply::HEADER_LEN {
            assert_eq!(
                NeighborTableReply::decode(&[0u8; NeighborTableReply::HEADER_LEN][..len]),
                Err(BmWireError::Truncated)
            );
        }
        let mut tiny = [0u8; 4];
        assert_eq!(
            encode_neighbor_table_reply(&mut tiny, 0, &[], &[]),
            Err(BmWireError::Truncated)
        );
    }
}
