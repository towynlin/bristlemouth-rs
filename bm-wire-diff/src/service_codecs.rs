//! Differential comparator for [`bm_wire::service`]'s body codecs against
//! `bm_common_messages`' `sys_info`, `config_map` and `power_info` codecs.
//!
//! tinycbor and the codecs keep no state, so this needs nothing brought up
//! and its target lives in [`crate::replay::TARGETS`].
//!
//! # What is compared
//!
//! | Step | Compared |
//! |---|---|
//! | Encode one value into a buffer of a given size | The C's `CborError` against the port's; on success, the length and the bytes |
//! | Decode arbitrary bytes into a struct holding arbitrary values | The `CborError`; every numeric field afterwards, written or not; on success, the string's bytes and its NUL |
//!
//! The decode payload is either raw bytes or an encoding of any of the four
//! bodies with bytes overwritten and the tail cut, so most inputs get past
//! the map's opening.
//!
//! # Input domain
//!
//! The oracle compiles the four codecs with `NDEBUG`, as a release build
//! does (`bm-wire-sys/build.rs`, `T2_RELEASE`): a debug build asserts on any
//! uint field that is not an unsigned integer (divergence #82), which would
//! abort the comparator rather than compare anything.
//!
//! Three things are not compared:
//!
//! * Inputs the port decodes to [`CborError::Unreachable`]: a tag among the
//!   values leaves the map short of its end, and `cbor_value_leave_container`
//!   then fails a `cbor_assert`, undefined in a release build; and a
//!   `config_map` reply whose data is not a byte string reaches
//!   `cbor_value_copy_byte_string`'s assert. The C is not called.
//! * Inputs whose outcome is the heap's. `sys_info_reply_decode` and
//!   `config_cbor_map_reply_decode` allocate a size the sender chooses.
//!   `bm_shim_heap_watch_begin` refuses zero bytes or more than [`HEAP_LIMIT`]
//!   for the duration of the call, as a small embedded heap would, and an
//!   input that had an allocation refused is skipped. The port assumes the
//!   allocation succeeds.
//! * `config_cbor_map_reply_decode`'s data pointer on failure: the C leaves
//!   a partial copy there, the port leaves `None`.
//!
//! An `app_name` to encode is NUL-free: the C reads it to its first NUL.

use std::ffi::CString;

use arbitrary::{Arbitrary, Result, Unstructured};
use bm_wire::cbor::parser::CborError;
use bm_wire::service::config_map::{ConfigMapReply, ConfigMapRequest, DecodedConfigMapReply};
use bm_wire::service::power_info::PowerInfoReply;
use bm_wire::service::sys_info::{DecodedSysInfoReply, SysInfoReply};
use bm_wire_sys as sys;

/// The largest allocation the shim grants a decoder. Larger than any payload
/// this comparator builds, so a refusal means the sender claimed a length it
/// did not send.
pub const HEAP_LIMIT: usize = 64 * 1024;

/// Longest payload, encoding buffer or byte string built: past a service
/// handler's 1008 bytes.
const MAX_LEN: usize = 1100;

/// Which body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arbitrary)]
pub enum Body {
    /// `SysInfoReplyData`.
    SysInfo,
    /// `ConfigCborMapRequestData`.
    ConfigMapRequest,
    /// `ConfigCborMapReplyData`.
    ConfigMapReply,
    /// `PowerInfoReplyData`.
    PowerInfo,
}

/// The values of every body's fields, used both to encode and as a decode
/// destination's starting contents.
#[derive(Debug, Clone)]
pub struct Fields {
    /// `node_id`.
    pub node_id: u64,
    /// `git_sha`, `partition_id`, `total_on_s`.
    pub a: u32,
    /// `sys_config_crc`, `remaining_on_s`.
    pub b: u32,
    /// `app_name_strlen`, `cbor_encoded_map_len`, `upcoming_off_s`.
    pub c: u32,
    /// `success`.
    pub success: bool,
    /// `app_name` (NULs removed) or `cbor_data`.
    pub bytes: Vec<u8>,
}

/// One encode step.
#[derive(Debug, Clone)]
pub struct Encode {
    /// The body.
    pub body: Body,
    /// Its values.
    pub fields: Fields,
    /// The buffer's size.
    pub size: usize,
}

/// One decode step.
#[derive(Debug, Clone)]
pub struct Decode {
    /// The body.
    pub body: Body,
    /// The bytes handed to both decoders.
    pub payload: Vec<u8>,
    /// What the destination struct holds before the call.
    pub init: Fields,
}

/// An encode step and a decode step.
#[derive(Debug, Clone)]
pub struct ServiceCodecsInput {
    /// Encoded both sides.
    pub encode: Encode,
    /// Decoded both sides.
    pub decode: Decode,
}

fn bounded_bytes(u: &mut Unstructured<'_>, max: usize) -> Result<Vec<u8>> {
    let len = u.int_in_range(0..=max)?;
    let mut bytes = vec![0u8; len];
    u.fill_buffer(&mut bytes)?;
    Ok(bytes)
}

fn fields(u: &mut Unstructured<'_>) -> Result<Fields> {
    let mut f = Fields {
        node_id: u.arbitrary()?,
        a: u.arbitrary()?,
        b: u.arbitrary()?,
        c: u.arbitrary()?,
        success: u.arbitrary()?,
        bytes: Vec::new(),
    };
    // Mostly short, so most encodes fit.
    let max = if u.ratio(1, 8)? { MAX_LEN } else { 40 };
    f.bytes = bounded_bytes(u, max)?;
    Ok(f)
}

impl<'a> Arbitrary<'a> for ServiceCodecsInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let encode = Encode {
            body: u.arbitrary()?,
            fields: fields(u)?,
            size: u.int_in_range(0..=MAX_LEN)?,
        };
        let body = u.arbitrary()?;
        let init = fields(u)?;
        let payload = if u.ratio(1, 4)? {
            bounded_bytes(u, MAX_LEN)?
        } else {
            let source = Encode {
                body: u.arbitrary()?,
                fields: fields(u)?,
                size: MAX_LEN + 64,
            };
            let mut payload = encode_rust(&source).unwrap_or_default();
            let edits = u.int_in_range(0..=4)?;
            for _ in 0..edits {
                if payload.is_empty() {
                    break;
                }
                let at = u.choose_index(payload.len())?;
                payload[at] = u.arbitrary()?;
            }
            if u.ratio(1, 4)? && !payload.is_empty() {
                let keep = u.int_in_range(0..=payload.len())?;
                payload.truncate(keep);
            }
            payload
        };
        Ok(Self {
            encode,
            decode: Decode {
                body,
                payload,
                init,
            },
        })
    }
}

/// Assert the port agrees with the C for this input.
///
/// # Panics
///
/// On any divergence.
pub fn check(input: &ServiceCodecsInput) {
    check_encode(&input.encode);
    check_decode(&input.decode);
}

fn app_name(f: &Fields) -> Vec<u8> {
    f.bytes.iter().copied().filter(|&b| b != 0).collect()
}

fn rust_encode(e: &Encode, out: &mut [u8]) -> std::result::Result<usize, CborError> {
    let f = &e.fields;
    match e.body {
        Body::SysInfo => SysInfoReply {
            node_id: f.node_id,
            git_sha: f.a,
            sys_config_crc: f.b,
            app_name_strlen: f.c,
            app_name: &app_name(f),
        }
        .encode(out),
        Body::ConfigMapRequest => ConfigMapRequest { partition_id: f.a }.encode(out),
        Body::ConfigMapReply => ConfigMapReply {
            node_id: f.node_id,
            partition_id: f.a,
            success: f.success,
            cbor_data: &f.bytes,
        }
        .encode(out),
        Body::PowerInfo => PowerInfoReply {
            total_on_s: f.a,
            remaining_on_s: f.b,
            upcoming_off_s: f.c,
        }
        .encode(out),
    }
}

fn encode_rust(e: &Encode) -> Option<Vec<u8>> {
    let mut out = vec![0u8; e.size];
    let len = rust_encode(e, &mut out).ok()?;
    out.truncate(len);
    Some(out)
}

/// Encode through the C into a buffer of exactly `e.size` bytes.
fn c_encode(e: &Encode, out: &mut [u8]) -> (sys::CborError, usize) {
    let f = &e.fields;
    let mut len = 0usize;
    // SAFETY: each struct is fully initialised; every pointer is live for the
    // call; `out` is `out.len()` bytes; the C writes `len` only.
    let err = unsafe {
        match e.body {
            Body::SysInfo => {
                let name = CString::new(app_name(f)).expect("NULs were removed");
                let mut d = sys::SysInfoReplyData {
                    node_id: f.node_id,
                    git_sha: f.a,
                    sys_config_crc: f.b,
                    app_name_strlen: f.c,
                    app_name: name.as_ptr().cast_mut(),
                };
                sys::sys_info_reply_encode(&raw mut d, out.as_mut_ptr(), out.len(), &raw mut len)
            }
            Body::ConfigMapRequest => {
                let mut d = sys::ConfigCborMapRequestData { partition_id: f.a };
                sys::config_cbor_map_request_encode(
                    &raw mut d,
                    out.as_mut_ptr(),
                    out.len(),
                    &raw mut len,
                )
            }
            Body::ConfigMapReply => {
                let mut d = sys::ConfigCborMapReplyData {
                    node_id: f.node_id,
                    partition_id: f.a,
                    success: f.success,
                    cbor_encoded_map_len: f.bytes.len() as u32,
                    cbor_data: f.bytes.as_ptr().cast_mut(),
                };
                sys::config_cbor_map_reply_encode(
                    &raw mut d,
                    out.as_mut_ptr(),
                    out.len(),
                    &raw mut len,
                )
            }
            Body::PowerInfo => {
                let mut d = sys::PowerInfoReplyData {
                    total_on_s: f.a,
                    remaining_on_s: f.b,
                    upcoming_off_s: f.c,
                };
                sys::power_info_reply_encode(&raw mut d, out.as_mut_ptr(), out.len(), &raw mut len)
            }
        }
    };
    (err, len)
}

fn code(r: std::result::Result<(), CborError>) -> sys::CborError {
    match r {
        Ok(()) => sys::CborError_CborNoError,
        Err(e) => e.code().expect("Unreachable is never compared"),
    }
}

fn check_encode(e: &Encode) {
    let mut rust = vec![0u8; e.size];
    let r = rust_encode(e, &mut rust);
    let mut c = vec![0u8; e.size];
    let (err, c_len) = c_encode(e, &mut c);
    assert_eq!(
        err,
        code(r.map(|_| ())),
        "encode {e:?}: C returned {err}, the port {r:?}"
    );
    if let Ok(len) = r {
        assert_eq!(c_len, len, "encode {e:?}: lengths differ");
        assert_eq!(&c[..len], &rust[..len], "encode {e:?}: bytes differ");
    }
}

fn watch_begin() {
    // SAFETY: the watch is thread-local shim state with no preconditions.
    unsafe { sys::bm_shim_heap_watch_begin(HEAP_LIMIT) };
}

fn watch_end() -> sys::BmShimHeapWatch {
    // SAFETY: as above.
    unsafe { sys::bm_shim_heap_watch_end() }
}

fn check_decode(d: &Decode) {
    let p = &d.payload;
    let i = &d.init;
    let ctx = || format!("decode {:?} from {:02x?}", d.body, p);
    match d.body {
        Body::SysInfo => {
            let mut rust = DecodedSysInfoReply {
                node_id: i.node_id,
                git_sha: i.a,
                sys_config_crc: i.b,
                app_name_strlen: i.c,
                app_name: None,
            };
            let r = rust.decode_into(p);
            if r == Err(CborError::Unreachable) {
                return;
            }
            let mut c = sys::SysInfoReplyData {
                node_id: i.node_id,
                git_sha: i.a,
                sys_config_crc: i.b,
                app_name_strlen: i.c,
                app_name: std::ptr::null_mut(),
            };
            watch_begin();
            // SAFETY: `c` is initialised and `p` is live for the call.
            let err = unsafe { sys::sys_info_reply_decode(&raw mut c, p.as_ptr(), p.len()) };
            let watch = watch_end();
            let name = c.app_name;
            if watch.allocations > 0 && name.is_null() {
                // SAFETY: the C allocated this with bm_malloc and dropped it
                // when the copy failed (divergence #83).
                unsafe { sys::bm_free(watch.last) };
            }
            if watch.refused > 0 {
                // SAFETY: allocated by bm_malloc, or NULL.
                unsafe { sys::bm_free(name.cast()) };
                return;
            }
            assert_eq!(err, code(r), "{}", ctx());
            assert_eq!(
                (c.node_id, c.git_sha, c.sys_config_crc, c.app_name_strlen),
                (
                    rust.node_id,
                    rust.git_sha,
                    rust.sys_config_crc,
                    rust.app_name_strlen
                ),
                "{}",
                ctx()
            );
            assert_eq!(name.is_null(), rust.app_name.is_none(), "{}", ctx());
            if let Some(s) = rust.app_name {
                let buflen = rust.app_name_strlen.wrapping_add(1) as usize;
                let readable = buflen.min(s.len() + 1);
                // SAFETY: the C allocated `buflen` bytes and copied `s.len()`
                // of them, plus a NUL when `buflen` had room for one.
                let got = unsafe { std::slice::from_raw_parts(name.cast::<u8>(), readable) };
                assert!(s.eq_bytes(&got[..s.len()]), "{}: app_name differs", ctx());
                if s.len() < buflen {
                    assert_eq!(got[s.len()], 0, "{}: app_name unterminated", ctx());
                }
                // SAFETY: allocated by bm_malloc.
                unsafe { sys::bm_free(name.cast()) };
            }
        }
        Body::ConfigMapRequest => {
            let mut rust = ConfigMapRequest { partition_id: i.a };
            let r = rust.decode_into(p);
            if r == Err(CborError::Unreachable) {
                return;
            }
            let mut c = sys::ConfigCborMapRequestData { partition_id: i.a };
            // SAFETY: `c` is initialised and `p` is live for the call.
            let err =
                unsafe { sys::config_cbor_map_request_decode(&raw mut c, p.as_ptr(), p.len()) };
            assert_eq!(err, code(r), "{}", ctx());
            assert_eq!(c.partition_id, rust.partition_id, "{}", ctx());
        }
        Body::ConfigMapReply => {
            let mut rust = DecodedConfigMapReply {
                node_id: i.node_id,
                partition_id: i.a,
                success: i.success,
                cbor_encoded_map_len: i.c,
                cbor_data: None,
            };
            let r = rust.decode_into(p);
            if r == Err(CborError::Unreachable) {
                return;
            }
            let mut c = sys::ConfigCborMapReplyData {
                node_id: i.node_id,
                partition_id: i.a,
                success: i.success,
                cbor_encoded_map_len: i.c,
                cbor_data: std::ptr::null_mut(),
            };
            watch_begin();
            // SAFETY: `c` is initialised and `p` is live for the call.
            let err = unsafe { sys::config_cbor_map_reply_decode(&raw mut c, p.as_ptr(), p.len()) };
            let watch = watch_end();
            let data = c.cbor_data;
            let free = || {
                // SAFETY: allocated by bm_malloc, or NULL.
                unsafe { sys::bm_free(data.cast()) };
            };
            if watch.refused > 0 {
                free();
                return;
            }
            assert_eq!(err, code(r), "{}", ctx());
            assert_eq!(
                (c.node_id, c.partition_id, c.success, c.cbor_encoded_map_len),
                (
                    rust.node_id,
                    rust.partition_id,
                    rust.success,
                    rust.cbor_encoded_map_len
                ),
                "{}",
                ctx()
            );
            if r.is_ok() {
                assert_eq!(data.is_null(), rust.cbor_data.is_none(), "{}", ctx());
                if let Some(s) = rust.cbor_data {
                    // SAFETY: the C allocated and filled `cbor_encoded_map_len`
                    // bytes, which the port says is the string's length.
                    let got = unsafe { std::slice::from_raw_parts(data, s.len()) };
                    assert!(s.eq_bytes(got), "{}: cbor_data differs", ctx());
                }
            }
            free();
        }
        Body::PowerInfo => {
            let mut rust = PowerInfoReply {
                total_on_s: i.a,
                remaining_on_s: i.b,
                upcoming_off_s: i.c,
            };
            let r = rust.decode_into(p);
            assert_ne!(r, Err(CborError::Unreachable), "{}", ctx());
            let mut c = sys::PowerInfoReplyData {
                total_on_s: i.a,
                remaining_on_s: i.b,
                upcoming_off_s: i.c,
            };
            // SAFETY: `c` is initialised and `p` is live for the call.
            let err = unsafe { sys::power_info_reply_decode(&raw mut c, p.as_ptr(), p.len()) };
            assert_eq!(err, code(r), "{}", ctx());
            assert_eq!(
                (c.total_on_s, c.remaining_on_s, c.upcoming_off_s),
                (rust.total_on_s, rust.remaining_on_s, rust.upcoming_off_s),
                "{}",
                ctx()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(body: Body, payload: &[u8]) {
        check_decode(&Decode {
            body,
            payload: payload.to_vec(),
            init: Fields {
                node_id: 0x1111,
                a: 0x2222,
                b: 0x3333,
                c: 0x4444,
                success: true,
                bytes: Vec::new(),
            },
        });
    }

    fn encoded(body: Body, f: Fields) -> Vec<u8> {
        encode_rust(&Encode {
            body,
            fields: f,
            size: MAX_LEN,
        })
        .unwrap()
    }

    fn some_fields(bytes: &[u8]) -> Fields {
        Fields {
            node_id: 0xdead_beef_cafe,
            a: 1,
            b: 0x8000_0000,
            c: bytes.len() as u32,
            success: true,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn every_body_round_trips_through_both() {
        for body in [
            Body::SysInfo,
            Body::ConfigMapRequest,
            Body::ConfigMapReply,
            Body::PowerInfo,
        ] {
            let fields = some_fields(b"bm_wire_sys");
            for size in [0, 1, 20, 60, 61, 62, MAX_LEN] {
                check_encode(&Encode {
                    body,
                    fields: fields.clone(),
                    size,
                });
            }
            let bytes = encoded(body, fields);
            for target in [
                Body::SysInfo,
                Body::ConfigMapRequest,
                Body::ConfigMapReply,
                Body::PowerInfo,
            ] {
                decode(target, &bytes);
                for cut in 0..bytes.len() {
                    decode(target, &bytes[..cut]);
                }
            }
        }
    }

    /// `power_info_ut.cpp`'s values, encoded by both.
    #[test]
    fn power_info_ut() {
        check_encode(&Encode {
            body: Body::PowerInfo,
            fields: Fields {
                node_id: 0,
                a: u32::MAX,
                b: 100_000,
                c: 3_333_333,
                success: false,
                bytes: Vec::new(),
            },
            size: 1024,
        });
    }

    /// Divergence #82: a uint field of another type is read for its head's
    /// argument.
    #[test]
    fn a_uint_field_of_any_type() {
        decode(Body::ConfigMapRequest, b"\xa1\x61p\x62ab");
        decode(Body::ConfigMapRequest, b"\xa1\x61p\xf4");
        decode(Body::ConfigMapRequest, b"\xa1\x61p\xfa\x3f\x80\x00\x00");
        decode(Body::ConfigMapRequest, b"\xa1\x61p\x9f\xff");
        decode(Body::PowerInfo, b"\xa3\x61a\x62ab\x61b\x00\x61c\x00");
    }

    /// Divergence #83: a name one byte longer than its claimed length is
    /// accepted without a terminator; two bytes longer leaks the buffer.
    #[test]
    fn app_name_lengths() {
        for claimed in [3, 2, 1, 0, u32::MAX] {
            let mut f = some_fields(b"abc");
            f.c = claimed;
            decode(Body::SysInfo, &encoded(Body::SysInfo, f));
        }
    }

    /// Divergence #84: without `success` and a length the data is not read.
    #[test]
    fn config_map_reply_data() {
        for (success, len, data) in [
            (true, 3, &b"abc"[..]),
            (true, 2, b"abc"),
            (true, 4, b"abc"),
            (false, 3, b"abc"),
            (true, 0, b"abc"),
            (true, 70_000, b"abc"),
        ] {
            let mut f = some_fields(data);
            f.success = success;
            let mut bytes = encoded(Body::ConfigMapReply, f);
            // Rewrite `cbor_encoded_map_len`'s value, the byte after its key.
            let key = b"cbor_encoded_map_len";
            let at = bytes.windows(key.len()).position(|w| w == key).unwrap() + key.len();
            let mut head = [0u8; 5];
            head[0] = 0x1a;
            head[1..].copy_from_slice(&(len as u32).to_be_bytes());
            bytes.splice(at..=at, head);
            decode(Body::ConfigMapReply, &bytes);
        }
        // Not a byte string: a text string, and an array, where the data is
        // never read.
        let mut f = some_fields(b"");
        f.success = false;
        let mut bytes = encoded(Body::ConfigMapReply, f);
        *bytes.last_mut().unwrap() = 0x80;
        decode(Body::ConfigMapReply, &bytes);
    }

    #[test]
    fn chunked_strings_and_nesting() {
        // sys_info with a chunked app_name, and a git_sha nested ten deep.
        let mut p = b"\xa5\x61a\x01\x61b".to_vec();
        p.extend([0x81; 10]);
        p.push(0x00);
        p.extend(b"\x61c\x03\x61d\x04\x61e\x7f\x62ab\x61c\xff");
        decode(Body::SysInfo, &p);
        let mut deeper = b"\xa5\x61a\x01\x61b".to_vec();
        deeper.extend([0x81; 11]);
        deeper.push(0x00);
        deeper.extend(b"\x61c\x03\x61d\x04\x61e\x60");
        decode(Body::SysInfo, &deeper);
    }
}
