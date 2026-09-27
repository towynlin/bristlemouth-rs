//! Differential comparator for [`bm_wire::bcmp::dfu`].
//!
//! # What is being compared
//!
//! bm_core has no DFU encoder or decoder to call. Its senders fill a packed
//! `BcmpDfu*` struct field by field and hand `sizeof` of it to `bcmp_tx`; its
//! receivers cast the body pointer to one. `check_endianness` swaps nothing for
//! `0xD0`–`0xD9`. So the C's codec is the packed layout, and the oracle here is
//! `bm_wire_sys`'s `repr(C, packed)` mirrors of those structs, whose sizes and
//! field offsets bindgen asserts against clang's layout of `messages.h` and
//! `dfu_message_structs.h` at compile time.
//!
//! * **Encode.** [`check`] fills the C struct the way the sender in
//!   `dfu_core.c`, `dfu_client.c` or `dfu_host.c` does and compares its bytes
//!   with [`DfuMessage::encode`]'s. A chunk is built as
//!   `bm_dfu_host_send_chunk` builds it: `sizeof(BcmpDfuPayload)` plus the
//!   payload, copied to `payload_buf`.
//! * **Decode.** Arbitrary bytes are read through the C struct that
//!   `bm_dfu_process_message`'s switch on the body's first byte selects, and
//!   every field is compared with [`DfuMessage::decode`]'s. The decoded value
//!   is then re-encoded and must reproduce the bytes it was read from.
//!
//! What the senders do *not* set is not compared: none of them `memset` the
//! struct, and every field is assigned, so there is no padding or stale byte
//! to reproduce.
//!
//! # Input domain
//!
//! The C reads each struct, and a chunk's `payload_length` bytes, without
//! consulting `BcmpProcessData.size` (divergence #55). A body shorter than the
//! struct its type byte selects, or a chunk declaring more than arrived, has no
//! defined C reading to compare against. For those the comparator asserts only
//! that the port refuses them with [`BmWireError::Truncated`]. A type byte
//! outside `0xD0`–`0xD9` reaches the `default:` branch, which drops the body
//! (divergence #54); the port must refuse it with [`BmWireError::Invalid`].
//!
//! Chunk payloads are not clamped to [`DFU_MAX_CHUNK_SIZE`]: the limit is the
//! client's, not the codec's, and a host sends up to it.

use arbitrary::{Arbitrary, Result, Unstructured};
use bm_wire::BmWireError;
use bm_wire::bcmp::dfu::{
    DFU_MAX_CHUNK_SIZE, DfuAddress, DfuChunk, DfuChunkRequest, DfuMessage, DfuResult, DfuStart,
    ImgInfo,
};
use bm_wire_sys as sys;

/// Source and destination node ids.
#[derive(Debug, Clone, Copy, Arbitrary)]
pub struct Addr {
    /// `src_node_id`.
    pub src: u64,
    /// `dst_node_id`.
    pub dst: u64,
}

/// `BmDfuImgInfo`'s fields.
#[derive(Debug, Clone, Copy, Arbitrary)]
#[allow(missing_docs)]
pub struct Img {
    pub image_size: u32,
    pub chunk_size: u16,
    pub crc16: u16,
    pub major_ver: u8,
    pub minor_ver: u8,
    pub filter_key: u32,
    pub git_sha: u32,
}

/// `BmDfuEventResult`'s fields after the address.
#[derive(Debug, Clone, Copy, Arbitrary)]
#[allow(missing_docs)]
pub struct Outcome {
    pub success: u8,
    pub err_code: u8,
}

/// One message to encode on both sides, by type.
#[derive(Debug, Clone, Arbitrary)]
#[allow(missing_docs)]
pub enum Encode {
    Start(Addr, Img),
    PayloadReq(Addr, u16),
    Payload(Addr, Vec<u8>),
    End(Addr, Outcome),
    Ack(Addr, Outcome),
    Abort(Addr, Outcome),
    Heartbeat(Addr),
    RebootReq(Addr),
    Reboot(Addr),
    BootComplete(Addr),
}

/// One message to encode, and one arbitrary body to decode.
#[derive(Debug, Clone)]
pub struct DfuCodecInput {
    /// Fields for the encode direction.
    pub encode: Encode,
    /// Bytes for the decode direction, handed to both sides. The rest of the
    /// input, verbatim, so a seed can carry a body as it is on the wire.
    pub decode: Vec<u8>,
}

impl<'a> Arbitrary<'a> for DfuCodecInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let encode = Encode::arbitrary(u)?;
        let len = u.arbitrary_len::<u8>()?;
        Ok(Self {
            encode,
            decode: u.bytes(len)?.to_vec(),
        })
    }

    fn arbitrary_take_rest(mut u: Unstructured<'a>) -> Result<Self> {
        Ok(Self {
            encode: Encode::arbitrary(&mut u)?,
            decode: u.take_rest().to_vec(),
        })
    }
}

/// Run both directions.
///
/// # Panics
///
/// If the port and the C layout disagree on any byte or field.
pub fn check(input: &DfuCodecInput) {
    check_encode(&input.encode);
    check_decode(&input.decode);
}

fn address(a: Addr) -> DfuAddress {
    DfuAddress {
        src_node_id: a.src,
        dst_node_id: a.dst,
    }
}

fn c_address(a: Addr) -> sys::BmDfuEventAddress {
    sys::BmDfuEventAddress {
        src_node_id: a.src,
        dst_node_id: a.dst,
    }
}

fn c_result(a: Addr, o: Outcome) -> sys::BmDfuEventResult {
    sys::BmDfuEventResult {
        addresses: c_address(a),
        success: o.success,
        err_code: o.err_code,
    }
}

fn header(message_type: u32) -> sys::BmDfuFrameHeader {
    sys::BmDfuFrameHeader {
        frame_type: message_type as u8,
    }
}

/// The bytes of a packed C struct.
fn bytes_of<T: Copy>(value: &T) -> Vec<u8> {
    // SAFETY: `T` is a bindgen `repr(C, packed)` struct of integers: no
    // padding, every byte initialised.
    unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()).to_vec() }
}

/// A packed C struct read from the front of `body`, if it is long enough.
fn read<T: Copy>(body: &[u8]) -> Option<T> {
    // SAFETY: bounds checked; `T` is packed, so alignment is 1, and every bit
    // pattern is valid for its integer fields.
    (body.len() >= size_of::<T>()).then(|| unsafe { body.as_ptr().cast::<T>().read_unaligned() })
}

/// The C sender's bytes for `encode`, and the port's message for it.
fn c_and_rust(encode: &Encode) -> (Vec<u8>, DfuMessage<'_>) {
    use sys::{
        BcmpMessageType_BcmpDFUAbortMessage as ABORT, BcmpMessageType_BcmpDFUAckMessage as ACK,
        BcmpMessageType_BcmpDFUBootCompleteMessage as BOOT_COMPLETE,
        BcmpMessageType_BcmpDFUEndMessage as END,
        BcmpMessageType_BcmpDFUHeartbeatMessage as HEARTBEAT,
        BcmpMessageType_BcmpDFUPayloadReqMessage as PAYLOAD_REQ,
        BcmpMessageType_BcmpDFURebootMessage as REBOOT,
        BcmpMessageType_BcmpDFURebootReqMessage as REBOOT_REQ,
        BcmpMessageType_BcmpDFUStartMessage as START,
    };
    let result = |a: Addr, o: Outcome| DfuResult {
        addresses: address(a),
        success: o.success,
        err_code: o.err_code,
    };
    match encode {
        Encode::Start(a, i) => (
            bytes_of(&sys::BcmpDfuStart {
                header: header(START),
                info: sys::BmDfuEventImgInfo {
                    addresses: c_address(*a),
                    img_info: sys::BmDfuImgInfo {
                        image_size: i.image_size,
                        chunk_size: i.chunk_size,
                        crc16: i.crc16,
                        major_ver: i.major_ver,
                        minor_ver: i.minor_ver,
                        filter_key: i.filter_key,
                        gitSHA: i.git_sha,
                    },
                },
            }),
            DfuMessage::Start(DfuStart {
                addresses: address(*a),
                img_info: ImgInfo {
                    image_size: i.image_size,
                    chunk_size: i.chunk_size,
                    crc16: i.crc16,
                    major_ver: i.major_ver,
                    minor_ver: i.minor_ver,
                    filter_key: i.filter_key,
                    git_sha: i.git_sha,
                },
            }),
        ),
        Encode::PayloadReq(a, seq_num) => (
            bytes_of(&sys::BcmpDfuPayloadReq {
                header: header(PAYLOAD_REQ),
                chunk_req: sys::BmDfuEventChunkRequest {
                    addresses: c_address(*a),
                    seq_num: *seq_num,
                },
            }),
            DfuMessage::PayloadReq(DfuChunkRequest {
                addresses: address(*a),
                seq_num: *seq_num,
            }),
        ),
        Encode::Payload(a, payload) => (
            c_chunk(*a, payload),
            DfuMessage::Payload(DfuChunk {
                addresses: address(*a),
                payload,
            }),
        ),
        Encode::End(a, o) => (
            bytes_of(&sys::BcmpDfuEnd {
                header: header(END),
                result: c_result(*a, *o),
            }),
            DfuMessage::End(result(*a, *o)),
        ),
        Encode::Ack(a, o) => (
            bytes_of(&sys::BcmpDfuAck {
                header: header(ACK),
                ack: c_result(*a, *o),
            }),
            DfuMessage::Ack(result(*a, *o)),
        ),
        Encode::Abort(a, o) => (
            bytes_of(&sys::BcmpDfuAbort {
                header: header(ABORT),
                err: c_result(*a, *o),
            }),
            DfuMessage::Abort(result(*a, *o)),
        ),
        Encode::Heartbeat(a) => (
            bytes_of(&sys::BcmpDfuHeartbeat {
                header: header(HEARTBEAT),
                addr: c_address(*a),
            }),
            DfuMessage::Heartbeat(address(*a)),
        ),
        Encode::RebootReq(a) => (
            bytes_of(&sys::BcmpDfuRebootReq {
                header: header(REBOOT_REQ),
                addr: c_address(*a),
            }),
            DfuMessage::RebootReq(address(*a)),
        ),
        Encode::Reboot(a) => (
            bytes_of(&sys::BcmpDfuReboot {
                header: header(REBOOT),
                addr: c_address(*a),
            }),
            DfuMessage::Reboot(address(*a)),
        ),
        Encode::BootComplete(a) => (
            bytes_of(&sys::BcmpDfuBootComplete {
                header: header(BOOT_COMPLETE),
                addr: c_address(*a),
            }),
            DfuMessage::BootComplete(address(*a)),
        ),
    }
}

/// `bm_dfu_host_send_chunk`'s buffer: `sizeof(BcmpDfuPayload) + payload_len`,
/// the fixed fields written through the struct, the payload through
/// `payload_buf`. `payload_length` is assigned from a `uint32_t`, so it keeps
/// the low 16 bits; the caller keeps the payload within them.
fn c_chunk(a: Addr, payload: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; size_of::<sys::BcmpDfuPayload>() + payload.len()];
    let p = buf.as_mut_ptr().cast::<sys::BcmpDfuPayload>();
    // SAFETY: `buf` holds the struct and the payload after it; the struct is
    // packed, so every place written here is unaligned-safe by construction,
    // and no reference to a packed field is taken.
    unsafe {
        (*p).header = header(sys::BcmpMessageType_BcmpDFUPayloadMessage);
        (*p).chunk.addresses = c_address(a);
        (*p).chunk.payload_length = payload.len() as u16;
        let dst = std::ptr::addr_of_mut!((*p).chunk.payload_buf).cast::<u8>();
        std::ptr::copy_nonoverlapping(payload.as_ptr(), dst, payload.len());
    }
    buf
}

fn check_encode(encode: &Encode) {
    let (c, message) = c_and_rust(encode);
    // `payload_length` cannot describe it; the C would keep the low 16 bits.
    if let Encode::Payload(_, payload) = encode
        && payload.len() > DfuChunk::MAX_PAYLOAD_LEN
    {
        let mut buf = vec![0u8; message.encoded_len()];
        assert_eq!(message.encode(&mut buf), Err(BmWireError::Invalid));
        return;
    }
    let mut buf = vec![0u8; message.encoded_len()];
    let n = message.encode(&mut buf).expect("buffer is encoded_len");
    assert_eq!(n, c.len(), "{encode:?}: length diverged");
    assert_eq!(buf, c, "{encode:?}: bytes diverged");
    assert_eq!(
        DfuMessage::decode(&c),
        Ok(message),
        "{encode:?}: the port could not read the C's bytes back"
    );
}

/// What the C reads from `body`: `Err` where it reads nothing defined.
fn c_decode(body: &[u8]) -> Result<DfuMessage<'_>, BmWireError> {
    let addr = |a: sys::BmDfuEventAddress| DfuAddress {
        src_node_id: a.src_node_id,
        dst_node_id: a.dst_node_id,
    };
    let result = |r: sys::BmDfuEventResult| DfuResult {
        addresses: addr(r.addresses),
        success: r.success,
        err_code: r.err_code,
    };
    let frame_type = *body.first().ok_or(BmWireError::Truncated)?;
    // `dfu_copy_and_process_message` reads the address before the type.
    read::<sys::BmDfuEventAddress>(&body[1..]).ok_or(BmWireError::Truncated)?;
    let t = u32::from(frame_type);
    Ok(match t {
        sys::BcmpMessageType_BcmpDFUStartMessage => {
            let m = read::<sys::BcmpDfuStart>(body).ok_or(BmWireError::Truncated)?;
            let i = m.info.img_info;
            DfuMessage::Start(DfuStart {
                addresses: addr(m.info.addresses),
                img_info: ImgInfo {
                    image_size: i.image_size,
                    chunk_size: i.chunk_size,
                    crc16: i.crc16,
                    major_ver: i.major_ver,
                    minor_ver: i.minor_ver,
                    filter_key: i.filter_key,
                    git_sha: i.gitSHA,
                },
            })
        }
        sys::BcmpMessageType_BcmpDFUPayloadReqMessage => {
            let m = read::<sys::BcmpDfuPayloadReq>(body).ok_or(BmWireError::Truncated)?;
            DfuMessage::PayloadReq(DfuChunkRequest {
                addresses: addr(m.chunk_req.addresses),
                seq_num: m.chunk_req.seq_num,
            })
        }
        sys::BcmpMessageType_BcmpDFUPayloadMessage => {
            let fixed = size_of::<sys::BcmpDfuPayload>();
            if body.len() < fixed {
                return Err(BmWireError::Truncated);
            }
            let p = body.as_ptr().cast::<sys::BcmpDfuPayload>();
            // SAFETY: `fixed` bytes are present; packed fields are read by
            // value, and `payload_buf`'s address is taken without a reference.
            let (addresses, len, start) = unsafe {
                let addresses = std::ptr::addr_of!((*p).chunk.addresses).read_unaligned();
                let len = std::ptr::addr_of!((*p).chunk.payload_length).read_unaligned();
                let start = std::ptr::addr_of!((*p).chunk.payload_buf)
                    .cast::<u8>()
                    .offset_from(body.as_ptr()) as usize;
                (addresses, usize::from(len), start)
            };
            DfuMessage::Payload(DfuChunk {
                addresses: addr(addresses),
                payload: body.get(start..start + len).ok_or(BmWireError::Truncated)?,
            })
        }
        sys::BcmpMessageType_BcmpDFUEndMessage => DfuMessage::End(result(
            read::<sys::BcmpDfuEnd>(body)
                .ok_or(BmWireError::Truncated)?
                .result,
        )),
        sys::BcmpMessageType_BcmpDFUAckMessage => DfuMessage::Ack(result(
            read::<sys::BcmpDfuAck>(body)
                .ok_or(BmWireError::Truncated)?
                .ack,
        )),
        sys::BcmpMessageType_BcmpDFUAbortMessage => DfuMessage::Abort(result(
            read::<sys::BcmpDfuAbort>(body)
                .ok_or(BmWireError::Truncated)?
                .err,
        )),
        sys::BcmpMessageType_BcmpDFUHeartbeatMessage => DfuMessage::Heartbeat(addr(
            read::<sys::BcmpDfuHeartbeat>(body)
                .ok_or(BmWireError::Truncated)?
                .addr,
        )),
        sys::BcmpMessageType_BcmpDFURebootReqMessage => DfuMessage::RebootReq(addr(
            read::<sys::BcmpDfuRebootReq>(body)
                .ok_or(BmWireError::Truncated)?
                .addr,
        )),
        sys::BcmpMessageType_BcmpDFURebootMessage => DfuMessage::Reboot(addr(
            read::<sys::BcmpDfuReboot>(body)
                .ok_or(BmWireError::Truncated)?
                .addr,
        )),
        sys::BcmpMessageType_BcmpDFUBootCompleteMessage => DfuMessage::BootComplete(addr(
            read::<sys::BcmpDfuBootComplete>(body)
                .ok_or(BmWireError::Truncated)?
                .addr,
        )),
        // `bm_dfu_process_message`'s `default:`.
        _ => return Err(BmWireError::Invalid),
    })
}

fn check_decode(body: &[u8]) {
    let c = c_decode(body);
    let rust = DfuMessage::decode(body);
    assert_eq!(rust, c, "decode diverged on {body:02x?}");

    // Whatever the port reads, it must be able to write back.
    if let Ok(message) = rust {
        let mut buf = vec![0u8; message.encoded_len()];
        let n = message.encode(&mut buf).expect("buffer is encoded_len");
        assert_eq!(
            &buf[..n],
            &body[..n],
            "re-encoding {message:?} did not reproduce its bytes"
        );
    }
}

/// A chunk of `bm_dfu_max_chunk_size` bytes, the largest a client accepts.
#[must_use]
pub fn max_chunk_input() -> DfuCodecInput {
    let payload: Vec<u8> = (0..DFU_MAX_CHUNK_SIZE).map(|i| i as u8).collect();
    let addr = Addr {
        src: 0x0102_0304_0506_0708,
        dst: 0x1112_1314_1516_1718,
    };
    let (decode, _) = c_and_rust(&Encode::Payload(addr, payload.clone()));
    DfuCodecInput {
        encode: Encode::Payload(addr, payload),
        decode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Addr = Addr {
        src: 0xFEDC_BA98_7654_3210,
        dst: 0x0011_2233_4455_6677,
    };

    #[test]
    fn the_crate_constants_are_bm_cores() {
        assert_eq!(DFU_MAX_CHUNK_SIZE, sys::bm_dfu_max_chunk_size as usize);
        assert_eq!(
            bm_wire::bcmp::dfu::IMG_INFO_FORCE_UPDATE,
            sys::BM_DFU_IMG_INFO_FORCE_UPDATE
        );
        assert_eq!(ImgInfo::LEN, size_of::<sys::BmDfuImgInfo>());
        assert_eq!(DfuAddress::LEN, size_of::<sys::BmDfuEventAddress>());
        assert_eq!(DfuMessage::START_LEN, size_of::<sys::BcmpDfuStart>());
        assert_eq!(DfuMessage::MIN_LEN, size_of::<sys::BcmpDfuHeartbeat>());
        assert_eq!(
            DfuMessage::WITH_TWO_BYTES_LEN,
            size_of::<sys::BcmpDfuPayload>()
        );
    }

    #[test]
    fn every_type_encodes_as_the_c_lays_it_out() {
        let img = Img {
            image_size: 0xDEAD_BEEF,
            chunk_size: 1024,
            crc16: 0x1D0F,
            major_ver: 1,
            minor_ver: 2,
            filter_key: sys::BM_DFU_IMG_INFO_FORCE_UPDATE,
            git_sha: 0x0BAD_CAFE,
        };
        let o = Outcome {
            success: 0,
            err_code: 14,
        };
        for encode in [
            Encode::Start(A, img),
            Encode::PayloadReq(A, 0xFFFF),
            Encode::Payload(A, vec![]),
            Encode::Payload(A, vec![0x5A; 7]),
            Encode::End(A, o),
            Encode::Ack(A, o),
            Encode::Abort(A, o),
            Encode::Heartbeat(A),
            Encode::RebootReq(A),
            Encode::Reboot(A),
            Encode::BootComplete(A),
        ] {
            let (decode, _) = c_and_rust(&encode);
            check(&DfuCodecInput { encode, decode });
        }
    }

    #[test]
    fn a_full_size_chunk_round_trips() {
        check(&max_chunk_input());
    }

    #[test]
    fn short_and_foreign_bodies_are_refused_as_the_c_would_misread_them() {
        let (full, _) = c_and_rust(&Encode::Start(
            A,
            Img {
                image_size: 1,
                chunk_size: 1,
                crc16: 1,
                major_ver: 1,
                minor_ver: 1,
                filter_key: 1,
                git_sha: 1,
            },
        ));
        for len in 0..=full.len() {
            check_decode(&full[..len]);
        }
        let mut foreign = full.clone();
        for byte in 0..=0xFF {
            foreign[0] = byte;
            check_decode(&foreign);
        }
    }

    #[test]
    fn a_chunk_declaring_more_than_arrived_is_refused() {
        let mut body = c_chunk(A, &[1, 2, 3]);
        body[17] = 4;
        assert_eq!(c_decode(&body), Err(BmWireError::Truncated));
        check_decode(&body);
    }
}
