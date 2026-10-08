//! The ten DFU bodies, `0xD0`–`0xD9`, ported from `bcmp/dfu_message_structs.h`
//! and the `BcmpDfu*` wrappers in `bcmp/messages.h`.
//!
//! Codecs only. The state machine that sends and consumes them is
//! `bcmp/dfu_core.c`, `dfu_client.c` and `dfu_host.c`.
//!
//! # Layout
//!
//! Every body starts with a one-byte `frame_type` (`BmDfuFrameHeader`) and then
//! a [`DfuAddress`]. What follows depends on the type:
//!
//! | Type | C wrapper | After the address | Length |
//! |---|---|---|---|
//! | `0xD0` | `BcmpDfuStart` | [`ImgInfo`] | 35 |
//! | `0xD1` | `BcmpDfuPayloadReq` | `seq_num: u16` | 19 |
//! | `0xD2` | `BcmpDfuPayload` | `payload_length: u16`, then the bytes | 19 + n |
//! | `0xD3`, `0xD4`, `0xD5` | `BcmpDfuEnd`, `BcmpDfuAck`, `BcmpDfuAbort` | `success: u8`, `err_code: u8` | 19 |
//! | `0xD6`–`0xD9` | `BcmpDfuHeartbeat`, `BcmpDfuRebootReq`, `BcmpDfuReboot`, `BcmpDfuBootComplete` | nothing | 17 |
//!
//! `check_endianness` lists all ten types and swaps nothing, so these are
//! little-endian only because every deployed node is.
//!
//! # The body's type byte decides
//!
//! `frame_type` repeats the low byte of the BCMP header's type. `packet.c`
//! dispatches on the header, but all ten types reach the same handler, and
//! `bm_dfu_process_message` then switches on the **body** byte. The two can
//! disagree; the body byte wins. [`DfuMessage::decode`] dispatches on it too.
//! See divergence #54.
//!
//! # Lengths
//!
//! The C reads every field without consulting `BcmpProcessData.size`, and
//! reads `payload_length` bytes of chunk wherever the frame ends (divergence
//! #55). The decoders here refuse a body shorter than its fields with
//! [`BmWireError::Truncated`] and ignore bytes past them.

use crate::BmWireError;
use crate::bcmp::MessageType;
use crate::le;

/// `bm_dfu_max_chunk_size`: the longest chunk a client accepts and a host may
/// be asked to send. `dfu_client.c` ignores a longer [`DfuChunk`];
/// `bm_dfu_initiate_update` refuses a longer [`ImgInfo::chunk_size`]. The
/// codec itself does not enforce it, and neither does the C's.
pub const DFU_MAX_CHUNK_SIZE: usize = 1024;

/// `BM_DFU_IMG_INFO_FORCE_UPDATE`: an [`ImgInfo::filter_key`] that makes a
/// client accept an image whose `gitSHA` matches its own.
pub const IMG_INFO_FORCE_UPDATE: u32 = 0x4CED_C0FE;

/// Offset of the first DFU field after `frame_type`.
const ADDRESS_OFFSET: usize = 1;

/// `BmDfuEventAddress`: who sent a DFU message and who it is for.
///
/// Source first, unlike every other addressed BCMP body. There is no
/// broadcast: `dfu_copy_and_process_message` acts only on an exact match of
/// `dst_node_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DfuAddress {
    /// Node that sent it.
    pub src_node_id: u64,
    /// Node it is for.
    pub dst_node_id: u64,
}

impl DfuAddress {
    /// Wire size.
    pub const LEN: usize = 16;

    /// Decode from the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than [`Self::LEN`].
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let buf: &[u8; Self::LEN] = le::prefix(buf)?;
        Ok(Self {
            src_node_id: le::u64_at(buf, 0),
            dst_node_id: le::u64_at(buf, 8),
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than [`Self::LEN`].
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        let buf = buf.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
        buf[0..8].copy_from_slice(&self.src_node_id.to_le_bytes());
        buf[8..16].copy_from_slice(&self.dst_node_id.to_le_bytes());
        Ok(())
    }

    /// The address of any DFU body, read where `dfu_copy_and_process_message`
    /// reads it: one byte in, whatever the type byte says.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `body` is shorter than
    /// [`DfuMessage::MIN_LEN`].
    pub fn of_body(body: &[u8]) -> Result<Self, BmWireError> {
        Self::decode(body.get(ADDRESS_OFFSET..).ok_or(BmWireError::Truncated)?)
    }
}

/// `BmDfuImgInfo`: the image a `0xD0` offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImgInfo {
    /// Image length in bytes.
    pub image_size: u32,
    /// Bytes per [`DfuChunk`]. Zero is not refused by the codec or by the C;
    /// see divergence #57.
    pub chunk_size: u16,
    /// `crc16_ccitt` of the whole image, seeded with zero.
    pub crc16: u16,
    /// Major version.
    pub major_ver: u8,
    /// Minor version.
    pub minor_ver: u8,
    /// [`IMG_INFO_FORCE_UPDATE`], or anything else.
    pub filter_key: u32,
    /// `gitSHA`: the image's git SHA, compared with the client's own.
    pub git_sha: u32,
}

impl ImgInfo {
    /// Wire size. Also `DFU_IMG_START_OFFSET_BYTES`, where a host's stored
    /// image begins after its header.
    pub const LEN: usize = 18;

    /// Decode from the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than [`Self::LEN`].
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let b: &[u8; Self::LEN] = le::prefix(buf)?;
        Ok(Self {
            image_size: le::u32_at(b, 0),
            chunk_size: le::u16_at(b, 4),
            crc16: le::u16_at(b, 6),
            major_ver: b[8],
            minor_ver: b[9],
            filter_key: le::u32_at(b, 10),
            git_sha: le::u32_at(b, 14),
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than [`Self::LEN`].
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        let b = buf.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
        b[0..4].copy_from_slice(&self.image_size.to_le_bytes());
        b[4..6].copy_from_slice(&self.chunk_size.to_le_bytes());
        b[6..8].copy_from_slice(&self.crc16.to_le_bytes());
        b[8] = self.major_ver;
        b[9] = self.minor_ver;
        b[10..14].copy_from_slice(&self.filter_key.to_le_bytes());
        b[14..18].copy_from_slice(&self.git_sha.to_le_bytes());
        Ok(())
    }
}

/// `BmDfuEventImgInfo`, the body of `0xD0`: a host offering an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DfuStart {
    /// Host, then client.
    pub addresses: DfuAddress,
    /// The image on offer.
    pub img_info: ImgInfo,
}

/// `BmDfuEventChunkRequest`, the body of `0xD1`: a client asking for a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DfuChunkRequest {
    /// Client, then host.
    pub addresses: DfuAddress,
    /// Zero-based chunk index.
    pub seq_num: u16,
}

/// `BmDfuEventImageChunk`, the body of `0xD2`: a host sending a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DfuChunk<'a> {
    /// Host, then client.
    pub addresses: DfuAddress,
    /// The chunk. Its length is `payload_length` on the wire.
    pub payload: &'a [u8],
}

impl DfuChunk<'_> {
    /// Longest payload the `u16` length field can describe. The C's limit is
    /// [`DFU_MAX_CHUNK_SIZE`], applied by the client, not the codec.
    pub const MAX_PAYLOAD_LEN: usize = u16::MAX as usize;
}

/// `BmDfuEventResult`, the body of `0xD3`, `0xD4` and `0xD5`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DfuResult {
    /// Sender, then recipient.
    pub addresses: DfuAddress,
    /// 1 for success, 0 for failure, as bm_core sends it. Any byte decodes.
    pub success: u8,
    /// A `BmDfuErr`, carried as its byte.
    pub err_code: u8,
}

/// One DFU body, keyed on its `frame_type` byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DfuMessage<'a> {
    /// `0xD0`, `BcmpDfuStart`.
    Start(DfuStart),
    /// `0xD1`, `BcmpDfuPayloadReq`.
    PayloadReq(DfuChunkRequest),
    /// `0xD2`, `BcmpDfuPayload`.
    Payload(DfuChunk<'a>),
    /// `0xD3`, `BcmpDfuEnd`.
    End(DfuResult),
    /// `0xD4`, `BcmpDfuAck`.
    Ack(DfuResult),
    /// `0xD5`, `BcmpDfuAbort`.
    Abort(DfuResult),
    /// `0xD6`, `BcmpDfuHeartbeat`.
    Heartbeat(DfuAddress),
    /// `0xD7`, `BcmpDfuRebootReq`.
    RebootReq(DfuAddress),
    /// `0xD8`, `BcmpDfuReboot`.
    Reboot(DfuAddress),
    /// `0xD9`, `BcmpDfuBootComplete`.
    BootComplete(DfuAddress),
}

impl<'a> DfuMessage<'a> {
    /// The shortest body, `frame_type` plus a [`DfuAddress`]: every
    /// address-only type, and what the C reads before looking at the type.
    pub const MIN_LEN: usize = ADDRESS_OFFSET + DfuAddress::LEN;

    /// `sizeof(BcmpDfuStart)`.
    pub const START_LEN: usize = Self::MIN_LEN + ImgInfo::LEN;

    /// `sizeof` of `BcmpDfuPayloadReq`, `BcmpDfuEnd`, `BcmpDfuAck`,
    /// `BcmpDfuAbort` and `BcmpDfuPayload` (whose `payload_buf[0]` adds
    /// nothing).
    pub const WITH_TWO_BYTES_LEN: usize = Self::MIN_LEN + 2;

    /// The type this body's `frame_type` byte names.
    #[must_use]
    pub fn message_type(&self) -> MessageType {
        match self {
            Self::Start(_) => MessageType::DFU_START,
            Self::PayloadReq(_) => MessageType::DFU_PAYLOAD_REQ,
            Self::Payload(_) => MessageType::DFU_PAYLOAD,
            Self::End(_) => MessageType::DFU_END,
            Self::Ack(_) => MessageType::DFU_ACK,
            Self::Abort(_) => MessageType::DFU_ABORT,
            Self::Heartbeat(_) => MessageType::DFU_HEARTBEAT,
            Self::RebootReq(_) => MessageType::DFU_REBOOT_REQ,
            Self::Reboot(_) => MessageType::DFU_REBOOT,
            Self::BootComplete(_) => MessageType::DFU_BOOT_COMPLETE,
        }
    }

    /// Sender and recipient.
    #[must_use]
    pub fn addresses(&self) -> DfuAddress {
        match self {
            Self::Start(m) => m.addresses,
            Self::PayloadReq(m) => m.addresses,
            Self::Payload(m) => m.addresses,
            Self::End(m) | Self::Ack(m) | Self::Abort(m) => m.addresses,
            Self::Heartbeat(a) | Self::RebootReq(a) | Self::Reboot(a) | Self::BootComplete(a) => *a,
        }
    }

    /// Decode a body, choosing the type from its first byte as
    /// `bm_dfu_process_message` does.
    ///
    /// Bytes past the fields, or past a chunk's declared `payload_length`,
    /// are ignored.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `body` is empty, shorter than the type's
    /// fields, or shorter than a chunk's declared length.
    /// [`BmWireError::Invalid`] if the first byte is not `0xD0`–`0xD9`; the C
    /// drops such a body (divergence #54).
    pub fn decode(body: &'a [u8]) -> Result<Self, BmWireError> {
        let frame_type = *body.first().ok_or(BmWireError::Truncated)?;
        let addresses = DfuAddress::of_body(body)?;
        let tail = &body[Self::MIN_LEN..];
        let two = |tail: &[u8]| -> Result<[u8; 2], BmWireError> { le::prefix(tail).copied() };
        let result = |tail: &[u8]| -> Result<DfuResult, BmWireError> {
            let [success, err_code] = two(tail)?;
            Ok(DfuResult {
                addresses,
                success,
                err_code,
            })
        };
        Ok(match MessageType(u16::from(frame_type)) {
            MessageType::DFU_START => Self::Start(DfuStart {
                addresses,
                img_info: ImgInfo::decode(tail)?,
            }),
            MessageType::DFU_PAYLOAD_REQ => Self::PayloadReq(DfuChunkRequest {
                addresses,
                seq_num: u16::from_le_bytes(two(tail)?),
            }),
            MessageType::DFU_PAYLOAD => {
                let len = usize::from(u16::from_le_bytes(two(tail)?));
                Self::Payload(DfuChunk {
                    addresses,
                    payload: tail.get(2..2 + len).ok_or(BmWireError::Truncated)?,
                })
            }
            MessageType::DFU_END => Self::End(result(tail)?),
            MessageType::DFU_ACK => Self::Ack(result(tail)?),
            MessageType::DFU_ABORT => Self::Abort(result(tail)?),
            MessageType::DFU_HEARTBEAT => Self::Heartbeat(addresses),
            MessageType::DFU_REBOOT_REQ => Self::RebootReq(addresses),
            MessageType::DFU_REBOOT => Self::Reboot(addresses),
            MessageType::DFU_BOOT_COMPLETE => Self::BootComplete(addresses),
            _ => return Err(BmWireError::Invalid),
        })
    }

    /// Bytes [`Self::encode`] will write: the C's `sizeof` for the type, plus
    /// a chunk's payload.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        match self {
            Self::Start(_) => Self::START_LEN,
            Self::Payload(m) => Self::WITH_TWO_BYTES_LEN + m.payload.len(),
            Self::PayloadReq(_) | Self::End(_) | Self::Ack(_) | Self::Abort(_) => {
                Self::WITH_TWO_BYTES_LEN
            }
            Self::Heartbeat(_) | Self::RebootReq(_) | Self::Reboot(_) | Self::BootComplete(_) => {
                Self::MIN_LEN
            }
        }
    }

    /// Encode into `buf`, `frame_type` first, returning the bytes written.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than
    /// [`Self::encoded_len`].
    /// [`BmWireError::Invalid`] if a chunk is longer than
    /// [`DfuChunk::MAX_PAYLOAD_LEN`].
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        if let Self::Payload(m) = self
            && m.payload.len() > DfuChunk::MAX_PAYLOAD_LEN
        {
            return Err(BmWireError::Invalid);
        }
        let end = self.encoded_len();
        let buf = buf.get_mut(..end).ok_or(BmWireError::Truncated)?;
        // Every DFU type fits a byte; `frame_type` is its low byte.
        buf[0] = self.message_type().0 as u8;
        self.addresses().encode(&mut buf[ADDRESS_OFFSET..])?;
        let tail = &mut buf[Self::MIN_LEN..];
        match self {
            Self::Start(m) => m.img_info.encode(tail)?,
            Self::PayloadReq(m) => tail[..2].copy_from_slice(&m.seq_num.to_le_bytes()),
            Self::Payload(m) => {
                tail[..2].copy_from_slice(&(m.payload.len() as u16).to_le_bytes());
                tail[2..].copy_from_slice(m.payload);
            }
            Self::End(m) | Self::Ack(m) | Self::Abort(m) => {
                tail[0] = m.success;
                tail[1] = m.err_code;
            }
            Self::Heartbeat(_) | Self::RebootReq(_) | Self::Reboot(_) | Self::BootComplete(_) => {}
        }
        Ok(end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: DfuAddress = DfuAddress {
        src_node_id: 0x0102_0304_0506_0708,
        dst_node_id: 0x1112_1314_1516_1718,
    };

    const ADDR_BYTES: [u8; 16] = [
        0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12,
        0x11,
    ];

    /// A body: `frame_type`, [`ADDR_BYTES`], then `tail`.
    struct Body {
        bytes: [u8; 64],
        len: usize,
    }

    impl Body {
        fn new(frame_type: u8, tail: &[u8]) -> Self {
            let mut bytes = [0u8; 64];
            bytes[0] = frame_type;
            bytes[1..17].copy_from_slice(&ADDR_BYTES);
            bytes[17..17 + tail.len()].copy_from_slice(tail);
            Self {
                bytes,
                len: 17 + tail.len(),
            }
        }

        fn get(&self) -> &[u8] {
            &self.bytes[..self.len]
        }
    }

    fn round_trip(message: DfuMessage<'_>, expected: &[u8]) {
        let mut buf = [0u8; 64];
        let n = message.encode(&mut buf).expect("fits");
        assert_eq!(&buf[..n], expected);
        assert_eq!(n, message.encoded_len());
        assert_eq!(DfuMessage::decode(expected), Ok(message));
    }

    #[test]
    fn sizes_are_the_c_sizeofs() {
        assert_eq!(DfuMessage::MIN_LEN, 17);
        assert_eq!(DfuMessage::START_LEN, 35);
        assert_eq!(DfuMessage::WITH_TWO_BYTES_LEN, 19);
    }

    #[test]
    fn start_puts_the_image_info_after_the_address() {
        let img_info = ImgInfo {
            image_size: 0x0004_0000,
            chunk_size: 512,
            crc16: 0xBEEF,
            major_ver: 2,
            minor_ver: 7,
            filter_key: IMG_INFO_FORCE_UPDATE,
            git_sha: 0xCAFE_F00D,
        };
        let body = Body::new(
            0xD0,
            &[
                0x00, 0x00, 0x04, 0x00, 0x00, 0x02, 0xEF, 0xBE, 2, 7, 0xFE, 0xC0, 0xED, 0x4C, 0x0D,
                0xF0, 0xFE, 0xCA,
            ],
        );
        round_trip(
            DfuMessage::Start(DfuStart {
                addresses: ADDR,
                img_info,
            }),
            body.get(),
        );
    }

    #[test]
    fn results_are_two_bytes_after_the_address() {
        let result = DfuResult {
            addresses: ADDR,
            success: 1,
            err_code: 14,
        };
        for (message, frame_type) in [
            (DfuMessage::End(result), 0xD3),
            (DfuMessage::Ack(result), 0xD4),
            (DfuMessage::Abort(result), 0xD5),
        ] {
            round_trip(message, Body::new(frame_type, &[1, 14]).get());
        }
    }

    #[test]
    fn address_only_types_are_seventeen_bytes() {
        for (message, frame_type) in [
            (DfuMessage::Heartbeat(ADDR), 0xD6),
            (DfuMessage::RebootReq(ADDR), 0xD7),
            (DfuMessage::Reboot(ADDR), 0xD8),
            (DfuMessage::BootComplete(ADDR), 0xD9),
        ] {
            round_trip(message, Body::new(frame_type, &[]).get());
        }
    }

    #[test]
    fn a_chunk_carries_its_length_and_ignores_what_follows() {
        let chunk = DfuMessage::Payload(DfuChunk {
            addresses: ADDR,
            payload: &[0xAA, 0xBB, 0xCC],
        });
        round_trip(chunk, Body::new(0xD2, &[3, 0, 0xAA, 0xBB, 0xCC]).get());

        let longer = Body::new(0xD2, &[3, 0, 0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(DfuMessage::decode(longer.get()), Ok(chunk));
    }

    #[test]
    fn a_chunk_declaring_more_than_arrived_is_truncated() {
        let body = Body::new(0xD2, &[4, 0, 0xAA, 0xBB, 0xCC]);
        assert_eq!(DfuMessage::decode(body.get()), Err(BmWireError::Truncated));
    }

    #[test]
    fn a_chunk_request_carries_its_index() {
        round_trip(
            DfuMessage::PayloadReq(DfuChunkRequest {
                addresses: ADDR,
                seq_num: 0x1234,
            }),
            Body::new(0xD1, &[0x34, 0x12]).get(),
        );
    }

    #[test]
    fn a_type_byte_outside_the_dfu_range_is_invalid() {
        let mut body = [0u8; DfuMessage::START_LEN];
        for byte in (0..=0xFF).filter(|b| !(0xD0..=0xD9).contains(b)) {
            body[0] = byte;
            assert_eq!(DfuMessage::decode(&body), Err(BmWireError::Invalid));
        }
    }

    #[test]
    fn short_bodies_are_truncated() {
        assert_eq!(DfuMessage::decode(&[]), Err(BmWireError::Truncated));
        let mut body = [0u8; DfuMessage::START_LEN];
        for frame_type in 0xD0..=0xD9u8 {
            body[0] = frame_type;
            let need = match frame_type {
                0xD0 => DfuMessage::START_LEN,
                0xD1..=0xD5 => DfuMessage::WITH_TWO_BYTES_LEN,
                _ => DfuMessage::MIN_LEN,
            };
            assert!(DfuMessage::decode(&body[..need]).is_ok());
            assert_eq!(
                DfuMessage::decode(&body[..need - 1]),
                Err(BmWireError::Truncated)
            );
        }
    }

    #[test]
    fn the_address_is_read_whatever_the_type_byte() {
        let body = Body::new(0x00, &[]);
        assert_eq!(DfuAddress::of_body(body.get()), Ok(ADDR));
        assert_eq!(
            DfuAddress::of_body(&body.get()[..16]),
            Err(BmWireError::Truncated)
        );
    }

    #[test]
    fn a_chunk_too_long_to_describe_is_refused() {
        let payload = [0u8; DfuChunk::MAX_PAYLOAD_LEN + 1];
        let mut buf = [0u8; DfuChunk::MAX_PAYLOAD_LEN + 32];
        let chunk = DfuMessage::Payload(DfuChunk {
            addresses: ADDR,
            payload: &payload,
        });
        assert_eq!(chunk.encode(&mut buf), Err(BmWireError::Invalid));
    }
}
