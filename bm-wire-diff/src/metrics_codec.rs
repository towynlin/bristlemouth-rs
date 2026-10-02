//! Differential comparator for [`bm_wire::service::metrics`] against
//! `metrics_reply_encode` and `metrics_reply_decode`.
//!
//! Both are pure: they touch only their arguments, so this runs in-process.
//!
//! * **Encode.** One reply and component list, into a buffer of the same
//!   size on both sides: the same `CborError`, and on success the same
//!   bytes.
//! * **Decode.** One body, into the same tables with the same initial
//!   values on both sides: the same `CborError`, and the same value in the
//!   reply and in every destination, whether or not it failed. The body is
//!   the encoded one with byte edits and a cut, or arbitrary bytes.
//!
//! Keys are mostly drawn from a small pool so that decode tables find what
//! the encoder wrote, and include an interior NUL, a 63-byte and a 64-byte
//! key. A key is passed to the C with a NUL appended, so the C sees it up to
//! its first NUL, as the port does.
//!
//! # Input domain
//!
//! A tag on a field value makes `bm_decode_fields_from_table` advance past
//! the end of the component's map, which is a tinycbor precondition
//! violation: `assert` with asserts on, `unreachable()` under `NDEBUG`
//! (divergence #87). The port returns `CborError::Unreachable` there, as
//! `bm_wire::cbor::parser` does for every such assertion. For a body where
//! it does, the C is not called; the comparator asserts only that the body
//! holds a tag head (major type 6), the one way to reach it.

use arbitrary::Arbitrary;
use bm_wire::cbor::parser::CborError;
use bm_wire::service::metrics::{self, Component, ComponentMut, Entry, Field, Reply};
use bm_wire_sys as sys;

const LONG_63: &str = "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijk";
const LONG_64: &str = "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijkl";

const POOL: [&str; 10] = [
    "",
    "a",
    "b",
    "ab",
    "a\0b",
    "num_ports",
    "network_port_stats",
    "version",
    LONG_63,
    LONG_64,
];

/// A key: one of the pool, or arbitrary text.
#[derive(Debug, Clone, Arbitrary)]
pub enum Key {
    /// `POOL[i % POOL.len()]`.
    Pool(u8),
    /// Any text, NULs included.
    Raw(String),
}

impl Key {
    fn as_str(&self) -> &str {
        match self {
            Self::Pool(i) => POOL[usize::from(*i) % POOL.len()],
            Self::Raw(s) => s,
        }
    }
}

/// A field type and value. Floats are carried as bits so that NaNs compare.
#[derive(Debug, Clone, Copy, Arbitrary)]
#[allow(missing_docs)]
pub enum FieldIn {
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    Float(u32),
    Double(u64),
    String,
}

impl FieldIn {
    fn field(self) -> Field {
        match self {
            Self::U8(v) => Field::U8(v),
            Self::U16(v) => Field::U16(v),
            Self::U32(v) => Field::U32(v),
            Self::U64(v) => Field::U64(v),
            Self::Float(v) => Field::Float(f32::from_bits(v)),
            Self::Double(v) => Field::Double(f64::from_bits(v)),
            Self::String => Field::String,
        }
    }
}

/// A component: a key and its table.
#[derive(Debug, Clone, Arbitrary)]
pub struct ComponentIn {
    /// Its key.
    pub key: Key,
    /// Its entries.
    pub fields: Vec<(Key, FieldIn)>,
}

/// What [`check`] decodes.
#[derive(Debug, Clone, Arbitrary)]
pub enum Body {
    /// The C's encoding, if it succeeded, with these bytes overwritten (at
    /// positions taken modulo the length) and then cut to at most `cut`
    /// bytes; else nothing.
    Encoded {
        /// `(position, byte)` edits.
        edits: Vec<(u16, u8)>,
        /// Upper bound on the length.
        cut: u16,
    },
    /// These bytes.
    Raw(Vec<u8>),
}

/// One encode and one decode.
#[derive(Debug, Clone, Arbitrary)]
pub struct MetricsCodecInput {
    /// `version`, `node_id`, `uptime_ms` to encode.
    pub reply: (u8, u64, u32),
    /// Components to encode.
    pub components: Vec<ComponentIn>,
    /// The encode buffer's size, modulo 1200.
    pub buf_len: u16,
    /// What to decode.
    pub body: Body,
    /// The decode's initial reply.
    pub initial: (u8, u64, u32),
    /// The decode tables, with their initial values.
    pub tables: Vec<ComponentIn>,
}

fn c_key(key: &str) -> Vec<u8> {
    let mut v = key.as_bytes().to_vec();
    v.push(0);
    v
}

fn c_type(f: FieldIn) -> sys::BmField {
    match f {
        FieldIn::U8(_) => sys::BmField_BM_FIELD_UINT8,
        FieldIn::U16(_) => sys::BmField_BM_FIELD_UINT16,
        FieldIn::U32(_) => sys::BmField_BM_FIELD_UINT32,
        FieldIn::U64(_) => sys::BmField_BM_FIELD_UINT64,
        FieldIn::Float(_) => sys::BmField_BM_FIELD_FLOAT,
        FieldIn::Double(_) => sys::BmField_BM_FIELD_DOUBLE,
        FieldIn::String => sys::BmField_BM_FIELD_STRING,
    }
}

/// A value's bytes, little-endian, in an 8-byte slot the C reads or writes
/// at its own width.
fn slot(f: FieldIn) -> u64 {
    match f {
        FieldIn::U8(v) => v.into(),
        FieldIn::U16(v) => v.into(),
        FieldIn::U32(v) | FieldIn::Float(v) => v.into(),
        FieldIn::U64(v) | FieldIn::Double(v) => v,
        FieldIn::String => 0,
    }
}

/// What a decode left in a destination, by bits.
fn bits(f: Field) -> (u8, u64) {
    match f {
        Field::U8(v) => (0, v.into()),
        Field::U16(v) => (1, v.into()),
        Field::U32(v) => (2, v.into()),
        Field::U64(v) => (3, v),
        Field::Float(v) => (4, v.to_bits().into()),
        Field::Double(v) => (5, v.to_bits()),
        Field::String => (6, 0),
    }
}

/// The C slot read back at the field's width.
fn slot_bits(f: FieldIn, slot: u64) -> (u8, u64) {
    match f {
        FieldIn::U8(_) => (0, slot & 0xff),
        FieldIn::U16(_) => (1, slot & 0xffff),
        FieldIn::U32(_) => (2, slot & 0xffff_ffff),
        FieldIn::U64(_) => (3, slot),
        FieldIn::Float(_) => (4, slot & 0xffff_ffff),
        FieldIn::Double(_) => (5, slot),
        FieldIn::String => (6, 0),
    }
}

/// The C value of an outcome that is not `Unreachable`.
fn code(r: Result<(), CborError>) -> i32 {
    r.err()
        .map_or(Some(0), CborError::code)
        .expect("Unreachable is filtered out before comparing")
}

/// Encode `input`'s components both sides. Returns the C's bytes on success.
fn check_encode(input: &MetricsCodecInput) -> Option<Vec<u8>> {
    let (version, node_id, uptime_ms) = input.reply;
    let buf_len = usize::from(input.buf_len % 1200);

    // Rust.
    let entries: Vec<Vec<Entry<'_>>> = input
        .components
        .iter()
        .map(|c| {
            c.fields
                .iter()
                .map(|(k, f)| Entry {
                    key: k.as_str(),
                    field: f.field(),
                })
                .collect()
        })
        .collect();
    let comps: Vec<Component<'_>> = input
        .components
        .iter()
        .zip(&entries)
        .map(|(c, fields)| Component {
            key: c.key.as_str(),
            fields,
        })
        .collect();
    let reply = Reply {
        version,
        node_id,
        uptime_ms,
    };
    let mut rs_buf = vec![0u8; buf_len];
    let rs = metrics::encode(&reply, &comps, &mut rs_buf);

    // C.
    let keys: Vec<(Vec<u8>, Vec<Vec<u8>>)> = input
        .components
        .iter()
        .map(|c| {
            (
                c_key(c.key.as_str()),
                c.fields.iter().map(|(k, _)| c_key(k.as_str())).collect(),
            )
        })
        .collect();
    let values: Vec<Vec<u64>> = input
        .components
        .iter()
        .map(|c| c.fields.iter().map(|(_, f)| slot(*f)).collect())
        .collect();
    let tables: Vec<Vec<sys::BmEncoderTableEntry>> = input
        .components
        .iter()
        .enumerate()
        .map(|(i, c)| {
            c.fields
                .iter()
                .enumerate()
                .map(|(j, (_, f))| sys::BmEncoderTableEntry {
                    key: keys[i].1[j].as_ptr().cast(),
                    type_: c_type(*f),
                    value_source: (&raw const values[i][j]).cast(),
                })
                .collect()
        })
        .collect();
    let c_comps: Vec<sys::MetricsComponent> = tables
        .iter()
        .enumerate()
        .map(|(i, t)| sys::MetricsComponent {
            key: keys[i].0.as_ptr().cast(),
            fields: t.as_ptr(),
            num_fields: t.len(),
        })
        .collect();
    let d = sys::MetricsReplyData {
        version,
        node_id,
        uptime_ms,
        components: c_comps.as_ptr(),
        num_components: c_comps.len(),
    };
    let mut c_buf = vec![0u8; buf_len];
    let mut c_len = 0usize;
    let err = unsafe {
        sys::metrics_reply_encode(&raw const d, c_buf.as_mut_ptr(), buf_len, &raw mut c_len)
    };

    match rs {
        Ok(len) => {
            assert_eq!(err, 0, "encode: Rust succeeded, C returned {err}");
            assert_eq!(len, c_len, "encode: lengths differ");
            assert_eq!(rs_buf[..len], c_buf[..len], "encode: bytes differ");
            Some(c_buf[..len].to_vec())
        }
        Err(e) => {
            assert_eq!(e.code(), Some(err), "encode: Rust returned {e:?}, C {err}");
            None
        }
    }
}

/// Decode `body` both sides into `input.tables`.
fn check_decode(input: &MetricsCodecInput, body: &[u8]) {
    let (version, node_id, uptime_ms) = input.initial;

    // Rust.
    let mut entries: Vec<Vec<Entry<'_>>> = input
        .tables
        .iter()
        .map(|c| {
            c.fields
                .iter()
                .map(|(k, f)| Entry {
                    key: k.as_str(),
                    field: f.field(),
                })
                .collect()
        })
        .collect();
    let mut comps: Vec<ComponentMut<'_, '_>> = input
        .tables
        .iter()
        .zip(entries.iter_mut())
        .map(|(c, fields)| ComponentMut {
            key: c.key.as_str(),
            fields,
        })
        .collect();
    let mut reply = Reply {
        version,
        node_id,
        uptime_ms,
    };
    let rs = metrics::decode(body, &mut reply, &mut comps);
    drop(comps);
    if rs == Err(CborError::Unreachable) {
        assert!(
            body.iter().any(|b| b >> 5 == 6),
            "decode: Unreachable without a tag in {body:02x?}"
        );
        return;
    }
    let rs = code(rs);

    // C.
    let keys: Vec<(Vec<u8>, Vec<Vec<u8>>)> = input
        .tables
        .iter()
        .map(|c| {
            (
                c_key(c.key.as_str()),
                c.fields.iter().map(|(k, _)| c_key(k.as_str())).collect(),
            )
        })
        .collect();
    let mut values: Vec<Vec<u64>> = input
        .tables
        .iter()
        .map(|c| c.fields.iter().map(|(_, f)| slot(*f)).collect())
        .collect();
    let tables: Vec<Vec<sys::BmDecodeTableEntry>> = input
        .tables
        .iter()
        .zip(values.iter_mut())
        .enumerate()
        .map(|(i, (c, vals))| {
            c.fields
                .iter()
                .zip(vals.iter_mut())
                .enumerate()
                .map(|(j, ((_, f), v))| sys::BmDecodeTableEntry {
                    key: keys[i].1[j].as_ptr().cast(),
                    type_: c_type(*f),
                    value_desitination: (&raw mut *v).cast(),
                })
                .collect()
        })
        .collect();
    let c_comps: Vec<sys::MetricsComponentDecode> = tables
        .iter()
        .enumerate()
        .map(|(i, t)| sys::MetricsComponentDecode {
            key: keys[i].0.as_ptr().cast(),
            fields: t.as_ptr(),
            num_fields: t.len(),
        })
        .collect();
    let mut out = sys::MetricsReplyDecode {
        version,
        node_id,
        uptime_ms,
        components: c_comps.as_ptr(),
        num_components: c_comps.len(),
    };
    // An exact-length copy, so ASan sees any read past the body.
    let c_body = body.to_vec();
    let err = unsafe { sys::metrics_reply_decode(c_body.as_ptr(), c_body.len(), &raw mut out) };
    drop(c_comps);
    drop(tables);

    assert_eq!(
        rs, err,
        "decode: Rust returned {rs}, C {err} for {body:02x?}"
    );
    assert_eq!(
        (reply.version, reply.node_id, reply.uptime_ms),
        (out.version, out.node_id, out.uptime_ms),
        "decode: reply differs for {body:02x?}"
    );
    for (i, c) in input.tables.iter().enumerate() {
        for (j, (k, f)) in c.fields.iter().enumerate() {
            assert_eq!(
                bits(entries[i][j].field),
                slot_bits(*f, values[i][j]),
                "decode: component {i} field {j} ({:?}) differs for {body:02x?}",
                k.as_str()
            );
        }
    }
}

/// Assert both encode and decode agree with the C for `input`.
///
/// # Panics
///
/// On any divergence.
pub fn check(input: &MetricsCodecInput) {
    let encoded = check_encode(input);
    match &input.body {
        Body::Encoded { edits, cut } => {
            let Some(mut body) = encoded else { return };
            if !body.is_empty() {
                let len = body.len();
                for &(at, b) in edits {
                    body[usize::from(at) % len] = b;
                }
            }
            body.truncate(usize::from(*cut));
            check_decode(input, &body);
        }
        Body::Raw(bytes) => check_decode(input, bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comp(key: &str, fields: &[(&str, FieldIn)]) -> ComponentIn {
        ComponentIn {
            key: Key::Raw(key.into()),
            fields: fields
                .iter()
                .map(|(k, f)| (Key::Raw((*k).into()), *f))
                .collect(),
        }
    }

    fn run(components: Vec<ComponentIn>, buf_len: u16, body: Body, tables: Vec<ComponentIn>) {
        check(&MetricsCodecInput {
            reply: (1, 0x0123_4567_89ab_cdef, 42000),
            components,
            buf_len,
            body,
            initial: (0xaa, 0xbb, 0xcc),
            tables,
        });
    }

    fn untouched() -> Body {
        Body::Encoded {
            edits: vec![],
            cut: u16::MAX,
        }
    }

    /// `MetricsReplyMsg.EncodesAndDecodesEnvelope`'s component.
    fn port_stats(sqi: u8, mse: u16) -> ComponentIn {
        comp(
            "network_port_stats",
            &[
                ("num_ports", FieldIn::U8(1)),
                ("sqi_1", FieldIn::U8(sqi)),
                ("mse_1", FieldIn::U16(mse)),
            ],
        )
    }

    #[test]
    fn the_gtest_envelope() {
        run(
            vec![port_stats(5, 1234)],
            256,
            untouched(),
            vec![port_stats(0, 0)],
        );
    }

    #[test]
    fn every_buffer_size_up_to_the_body() {
        for n in 0..120 {
            run(
                vec![port_stats(5, 1234)],
                n,
                untouched(),
                vec![port_stats(0, 0)],
            );
        }
    }

    #[test]
    fn every_cut_and_every_byte_edit() {
        for cut in 0..100 {
            run(
                vec![port_stats(5, 1234)],
                256,
                Body::Encoded { edits: vec![], cut },
                vec![port_stats(0, 0)],
            );
        }
        for at in 0..100 {
            for b in [
                0x00, 0x18, 0x1f, 0x3f, 0x5f, 0x7f, 0x9f, 0xbf, 0xc1, 0xf9, 0xfa, 0xfb, 0xff,
            ] {
                run(
                    vec![port_stats(5, 1234)],
                    256,
                    Body::Encoded {
                        edits: vec![(at, b)],
                        cut: u16::MAX,
                    },
                    vec![port_stats(0, 0)],
                );
            }
        }
    }

    #[test]
    fn string_fields() {
        let s = ("s", FieldIn::String);
        let u = ("u", FieldIn::U8(1));
        for fields in [[u, s], [s, u], [s, s]] {
            run(vec![comp("c", &fields)], 256, untouched(), vec![]);
        }
        // A STRING decode entry matches and is skipped.
        run(
            vec![comp("c", &[u])],
            256,
            untouched(),
            vec![comp("c", &[("u", FieldIn::String)])],
        );
    }

    #[test]
    fn key_quirks() {
        let fields = [
            ("a\0x", FieldIn::U32(7)),
            (LONG_63, FieldIn::U8(1)),
            (LONG_64, FieldIn::U8(2)),
            ("f", FieldIn::Float(0x3f80_0000)),
            ("d", FieldIn::Double(0x7ff0_0000_0000_0001)),
        ];
        let tables = vec![
            comp(
                "c",
                &[
                    ("a", FieldIn::U32(0)),
                    (LONG_63, FieldIn::U8(0)),
                    (LONG_64, FieldIn::U8(0)),
                    ("f", FieldIn::Double(0)),
                    ("d", FieldIn::Double(0)),
                ],
            ),
            comp("c\0", &[("f", FieldIn::Float(0))]),
            comp("", &[]),
        ];
        run(
            vec![comp("c", &fields), comp("", &[])],
            512,
            untouched(),
            tables,
        );
    }

    #[test]
    fn deep_nesting() {
        // The component's innermost container is the tenth or eleventh nested.
        for depth in [7usize, 8] {
            let mut b = vec![0xa4];
            for k in ["version", "node_id", "uptime_ms"] {
                b.push(0x60 + k.len() as u8);
                b.extend_from_slice(k.as_bytes());
                b.push(0x01);
            }
            b.extend_from_slice(&[0x64, b'd', b'a', b't', b'a', 0xa1, 0x61, b'c']);
            b.extend(std::iter::repeat_n(0x81, depth));
            b.push(0x80);
            run(vec![], 0, Body::Raw(b), vec![comp("c", &[])]);
        }
    }
}
