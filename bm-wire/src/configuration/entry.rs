//! A stored key with its value, for a log line.
//!
//! `key_buf` up to its NUL is not the key: its first `key_len` bytes are, and
//! what follows is whatever was after the key when it was first stored
//! (divergence #45). The value is in the slot.

use core::fmt;

use super::{ConfigPartition, Head, KEY_BUF_LEN, ValueType};
use crate::cbor::parser::{CborString, Value};

/// One key of a partition and the value in its slot.
///
/// `Display` writes the name, the type as `data_type_enum_to_str` names it,
/// and the value: `sensorsPollIntervalMs uint32 3000`.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Entry<'a> {
    /// `key_buf`'s first `key_len` bytes, up to a NUL: what bm_protocol's
    /// `cfg listkeys` prints.
    pub key: &'a [u8],
    /// `value_type` as stored. A loaded image can hold any value here.
    pub value_type: u32,
    /// The slot, read as `value_type`.
    pub value: EntryValue<'a>,
}

/// A slot's value, read as the key's stored `value_type`.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum EntryValue<'a> {
    /// As `get_config_uint`.
    Uint(u32),
    /// As `get_config_int`.
    Int(i32),
    /// As `get_config_float`.
    Float(f32),
    /// As `get_config_string`.
    Str(CborString<'a>),
    /// As `get_config_buffer`.
    Bytes(CborString<'a>),
    /// The array's encoding, head included.
    Array(&'a [u8]),
    /// `value_type` is not a `ConfigDataTypes`, or the slot does not hold a
    /// well-formed item of that type.
    Unreadable,
}

impl ConfigPartition {
    pub(super) fn entry(&self, i: usize) -> Entry<'_> {
        let stored = self.stored_key(i);
        let off = self.layout.key_offset(i);
        let key_len = usize::try_from(stored.key_len).map_or(KEY_BUF_LEN, |n| n.min(KEY_BUF_LEN));
        let key = &self.image[off..off + key_len];
        let key = key.split(|&b| b == 0).next().unwrap_or(key);
        Entry {
            key,
            value_type: stored.value_type,
            value: self.entry_value(i, stored.value_type),
        }
    }

    fn entry_value(&self, i: usize, value_type: u32) -> EntryValue<'_> {
        let slot = self.slot(i);
        let (Some(head), Ok(value)) = (Head::parse(slot), Value::parse(slot)) else {
            return EntryValue::Unreadable;
        };
        let arg = head.arg as i64;
        match (value_type, head.major, head.info) {
            (0, 0, _) => EntryValue::Uint(head.arg as u32),
            (1, 0, _) => EntryValue::Int(arg as i32),
            (1, 1, _) => EntryValue::Int(arg.wrapping_neg().wrapping_sub(1) as i32),
            (2, 7, 26) => EntryValue::Float(f32::from_bits(head.arg as u32)),
            (3, 3, _) => value
                .string()
                .map_or(EntryValue::Unreadable, EntryValue::Str),
            (4, 2, _) => value
                .string()
                .map_or(EntryValue::Unreadable, EntryValue::Bytes),
            (5, 4, _) => {
                let mut end = value;
                match end.advance() {
                    Ok(()) => slot
                        .get(..end.next_byte())
                        .map_or(EntryValue::Unreadable, EntryValue::Array),
                    Err(_) => EntryValue::Unreadable,
                }
            }
            _ => EntryValue::Unreadable,
        }
    }
}

/// `data_type_enum_to_str`.
fn type_name(value_type: u32) -> Option<&'static str> {
    [
        (ValueType::Uint32, "uint32"),
        (ValueType::Int32, "int32"),
        (ValueType::Float, "float"),
        (ValueType::Str, "str"),
        (ValueType::Bytes, "bytes"),
        (ValueType::Array, "array"),
    ]
    .into_iter()
    .find_map(|(ty, name)| (ty as u32 == value_type).then_some(name))
}

fn hex(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    bytes.iter().try_for_each(|b| write!(f, "{b:02x}"))
}

fn chunks(string: &CborString<'_>, mut each: impl FnMut(&[u8]) -> fmt::Result) -> fmt::Result {
    let mut result = Ok(());
    string.for_each_chunk(|chunk| {
        if result.is_ok() {
            result = each(chunk);
        }
    });
    result
}

impl fmt::Display for Entry<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ", self.key.escape_ascii())?;
        match type_name(self.value_type) {
            Some(name) => write!(f, "{name} ")?,
            None => write!(f, "type {} ", self.value_type)?,
        }
        match &self.value {
            EntryValue::Uint(v) => write!(f, "{v}"),
            EntryValue::Int(v) => write!(f, "{v}"),
            EntryValue::Float(v) => write!(f, "{v}"),
            EntryValue::Str(s) => {
                f.write_str("\"")?;
                chunks(s, |chunk| write!(f, "{}", chunk.escape_ascii()))?;
                f.write_str("\"")
            }
            EntryValue::Bytes(s) => chunks(s, |chunk| hex(f, chunk)),
            EntryValue::Array(encoded) => hex(f, encoded),
            EntryValue::Unreadable => f.write_str("unreadable"),
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use std::string::{String, ToString};
    use std::vec::Vec;

    use super::super::{Key, Layout};
    use super::*;

    fn lines(part: &ConfigPartition) -> Vec<String> {
        part.entries().map(|e| e.to_string()).collect()
    }

    /// Issue #65's partition: two keys set from a C node's CLI, whose key
    /// pointer runs on into the command line, one set with a NUL-terminated
    /// key, and two set over the bus, whose key pointer runs on into the CBOR
    /// value.
    fn issue_65() -> ConfigPartition {
        let mut p = ConfigPartition::new(Layout::ARM_EABI_GCC);
        assert!(p.set_uint(Key::with_len(b"disableUnusedPortsTimeMs uint 5", 24), 5));
        assert!(p.set_uint(Key::with_len(b"sensorsPollIntervalMs uint 3000", 21), 3000));
        assert!(p.set_uint(Key::new(b"rbrCodaType"), 3));
        let pi = [0xfa, 0x40, 0x49, 0x0f, 0xdb];
        assert!(p.set_cbor(Key::with_len(b"ztest1\xfa\x40\x49\x0f\xdb", 6), &pi));
        assert!(p.set_cbor(Key::with_len(b"ztest2jhanashimas", 6), b"jhanashimas"));
        p
    }

    /// What `bringup` logged before: `key_buf` up to its NUL.
    fn key_bufs(part: &ConfigPartition) -> Vec<Vec<u8>> {
        part.stored_keys()
            .map(|k| k.key_buf.split(|&b| b == 0).next().unwrap().to_vec())
            .collect()
    }

    /// The issue's sequence: set `ztest2` to a longer string, commit, reload.
    /// The new value is in the image; `key_buf` keeps the first set's tail,
    /// because `set_config_cbor` writes `key_buf` only for a new key.
    #[test]
    fn a_longer_string_survives_a_commit_and_key_buf_keeps_the_old_tail() {
        let mut p = issue_65();
        assert!(p.set_cbor(Key::with_len(b"ztest2lhanashimaska", 6), b"lhanashimaska"));
        let image = p.seal().to_vec();

        let mut loaded = ConfigPartition::new(Layout::ARM_EABI_GCC);
        assert!(loaded.load_with(|buf| {
            buf[..image.len()].copy_from_slice(&image);
            true
        }));

        let mut out = [0u8; 50];
        assert_eq!(loaded.get_string(Key::new(b"ztest2"), &mut out), Ok(12));
        assert_eq!(&out[..12], b"hanashimaska");

        // The issue's log, byte for byte.
        assert_eq!(
            key_bufs(&loaded),
            [
                b"disableUnusedPortsTimeMs uint 5".to_vec(),
                b"sensorsPollIntervalMs uint 3000".to_vec(),
                b"rbrCodaType".to_vec(),
                b"ztest1\xfa@I\x0f\xdb".to_vec(),
                b"ztest2jhanashimas".to_vec(),
            ]
        );
        assert_eq!(
            lines(&loaded),
            [
                "disableUnusedPortsTimeMs uint32 5",
                "sensorsPollIntervalMs uint32 3000",
                "rbrCodaType uint32 3",
                "ztest1 float 3.1415927",
                "ztest2 str \"hanashimaska\"",
            ]
        );
    }

    #[test]
    fn every_type_has_a_line() {
        let mut p = ConfigPartition::new(Layout::LP64);
        assert!(p.set_uint(Key::new(b"u"), u32::MAX));
        assert!(p.set_int(Key::new(b"i"), -1000));
        assert!(p.set_int(Key::new(b"j"), 7));
        assert!(p.set_float(Key::new(b"f"), -0.5));
        assert!(p.set_string(Key::new(b"s"), b"a \"b\"\n"));
        assert!(p.set_buffer(Key::new(b"b"), &[0xde, 0xad, 0x00]));
        // [1, [2, "x"]], then a chunked string.
        assert!(p.set_cbor(Key::new(b"a"), &[0x82, 0x01, 0x82, 0x02, 0x61, 0x78]));
        assert!(p.set_cbor(Key::new(b"c"), &[0x7f, 0x61, 0x68, 0x61, 0x69, 0xff]));
        assert!(p.set_string(Key::new(b"e"), b""));
        assert_eq!(
            lines(&p),
            [
                "u uint32 4294967295",
                "i int32 -1000",
                "j int32 7",
                "f float -0.5",
                "s str \"a \\\"b\\\"\\n\"",
                "b bytes dead00",
                "a array 820182026178",
                "c str \"hi\"",
                "e str \"\"",
            ]
        );
    }

    /// A shorter value leaves the longer one's tail in the slot; the line
    /// shows only the item.
    #[test]
    fn a_stale_slot_tail_is_not_shown() {
        let mut p = ConfigPartition::new(Layout::LP64);
        assert!(p.set_string(Key::new(b"s"), b"hanashimaska"));
        assert!(p.set_string(Key::new(b"s"), b"ha"));
        assert!(p.set_cbor(Key::new(b"a"), &[0x83, 0x01, 0x02, 0x03]));
        assert!(p.set_cbor(Key::new(b"a"), &[0x81, 0x09]));
        assert_eq!(lines(&p), ["s str \"ha\"", "a array 8109"]);
    }

    /// A loaded image can hold a type and a slot that disagree, a type that
    /// is no `ConfigDataTypes`, and a `key_len` past `key_buf`.
    #[test]
    fn a_mismatched_slot_is_unreadable() {
        let layout = Layout::LP64;
        let mut p = ConfigPartition::new(layout);
        assert!(p.set_uint(Key::new(b"a"), 1));
        assert!(p.set_uint(Key::new(b"b"), 2));
        assert!(p.set_uint(Key::new(b"c"), 3));
        // `a`: a string type over an integer slot.
        let meta = layout.key_offset(0) + KEY_BUF_LEN;
        p.image[meta + layout.key_len_width] = ValueType::Str as u8;
        // `b`: type 9, and a `key_len` of 255.
        let meta = layout.key_offset(1) + KEY_BUF_LEN;
        p.image[meta] = 0xff;
        p.image[meta + layout.key_len_width] = 9;
        // `c`: a string whose body runs past the slot.
        let meta = layout.key_offset(2) + KEY_BUF_LEN;
        p.image[meta + layout.key_len_width] = ValueType::Str as u8;
        p.image[layout.value_offset(2)] = 0x78;
        p.image[layout.value_offset(2) + 1] = 200;
        assert_eq!(
            lines(&p),
            [
                "a str unreadable",
                "b type 9 unreadable",
                "c str unreadable"
            ]
        );
    }
}
