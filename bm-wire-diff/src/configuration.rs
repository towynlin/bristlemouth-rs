//! Differential comparator for [`bm_wire::configuration`] against
//! `bcmp/configuration.c`.
//!
//! A script of store operations runs against both. After every step the
//! comparator asserts the same return value and outputs, then the same three
//! RAM partition images byte for byte, the same `needs_commit` flags, and the
//! same three flash images. The RAM image is read through `get_stored_keys`,
//! whose pointer is `HEADER_LEN` bytes into the C's `ConfigPartition`.
//!
//! # State
//!
//! `configuration.c`'s `CONFIGS` is process-global with no deinit, and its
//! flash is the shim's RAM, so every call is serialised behind one `Mutex`. It is
//! reset through its own front door at the **start** of each script: any
//! uncommitted partition is saved (the only way to clear `needs_commit`),
//! zeros are written to flash, and `config_init` loads them. The Rust side is
//! built fresh. Nothing else in `bm-wire-diff` touches either, so this target
//! is in [`crate::replay::TARGETS`].
//!
//! # Seam
//!
//! `bm_config_read`, `bm_config_write` and `bm_config_reset` have no oracle:
//! the C's are `bm-wire-sys/csrc/bm_generic_shim.c`, the Rust's are
//! [`bm_stack::RamConfigStorage`]. Both are byte copies, and every script
//! gives both the same bytes. `save_config` is only ever asked not to
//! restart: the shim's `bm_config_reset` clears flash, the Rust one does
//! nothing, and on hardware it resets the processor.
//!
//! # Domain
//!
//! * An image whose CRC checks and whose `numKeys` exceeds 50 is never
//!   presented: the C reads past its key array (divergence #48).
//!   [`Op::FixCrc`] clamps `numKeys` before it computes the CRC.
//! * `get_config_int` is not called on `3b 8000000000000000`, where the C
//!   negates `INT64_MIN` (divergence #41).

use std::sync::{Mutex, PoisonError};

use arbitrary::{Arbitrary, Result, Unstructured};
use bm_stack::RamConfigStorage;
use bm_stack::config::{config_init, save_config};
use bm_wire::configuration::{
    ConfigStore, CopyError, HEADER_LEN, Head, Key, Layout, MAX_NUM_KV, Partition,
};
use bm_wire::crc::crc32_ieee;

/// The oracle is built for the host.
const LAYOUT: Layout = Layout::LP64;
const IMAGE_LEN: usize = LAYOUT.image_len();

/// Serialises every call into `configuration.c` and the shim's flash.
static LOCK: Mutex<()> = Mutex::new(());

/// Keys chosen to collide: ordinary ones, the 31/32/33-byte boundary, an
/// invalid character, and two that differ only after a NUL.
const KEYS: &[&[u8]] = &[
    b"foo",
    b"bar",
    b"baz",
    b"a",
    b"_",
    b"Z9",
    b"abcdefghijklmnopqrstuvwxyz01234",
    b"abcdefghijklmnopqrstuvwxyz012345",
    b"abcdefghijklmnopqrstuvwxyz0123456",
    b"bad-key",
    b"ab\0c",
    b"ab\0d",
    b"",
];

/// A key as the C receives it: [`Key`]'s two halves, owned.
#[derive(Debug, Clone)]
pub struct KeyInput {
    /// The bytes at the key pointer.
    pub text: Vec<u8>,
    /// `key_len`.
    pub len: usize,
}

impl KeyInput {
    fn key(&self) -> Key<'_> {
        Key::with_len(&self.text, self.len)
    }

    /// `text`, then NULs to cover `key_len` and a terminator, so the C reads
    /// nothing the Rust does not model as NUL.
    fn c_bytes(&self) -> Vec<u8> {
        let mut bytes = self.text.clone();
        bytes.resize(self.text.len().max(self.len) + 1, 0);
        bytes
    }
}

impl<'a> Arbitrary<'a> for KeyInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let base = if u.ratio(3u8, 4)? {
            KEYS[u.choose_index(KEYS.len())?].to_vec()
        } else {
            bounded_bytes(u, 34)?
        };
        let len = match u.int_in_range(0u8..=5)? {
            0..=3 => base.len(),
            4 => base
                .len()
                .saturating_add_signed(u.int_in_range(-2isize..=2)?),
            _ => u.int_in_range(0usize..=40)?,
        };
        let mut text = base;
        // What `bcmp/config.c` passes: a key with the CBOR value straight
        // after it and no NUL between.
        if u.ratio(1u8, 5)? {
            text.extend(bounded_bytes(u, 8)?);
        }
        Ok(Self { text, len })
    }
}

/// A partition and a key.
#[derive(Debug, Clone)]
pub struct Target {
    /// Which partition.
    pub partition: Partition,
    /// Which key.
    pub key: KeyInput,
}

impl<'a> Arbitrary<'a> for Target {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        Ok(Self {
            partition: partition(u)?,
            key: u.arbitrary()?,
        })
    }
}

/// One store operation.
#[derive(Debug, Clone)]
pub enum Op {
    /// `set_config_uint`.
    SetUint(Target, u32),
    /// `set_config_int`.
    SetInt(Target, i32),
    /// `set_config_float`, as bits.
    SetFloat(Target, u32),
    /// `set_config_string`.
    SetString(Target, Vec<u8>),
    /// `set_config_buffer`.
    SetBuffer(Target, Vec<u8>),
    /// `set_config_cbor`.
    SetCbor(Target, Vec<u8>),
    /// `get_config_uint`.
    GetUint(Target),
    /// `get_config_int`.
    GetInt(Target),
    /// `get_config_float`.
    GetFloat(Target),
    /// `get_config_string` into a buffer of this size.
    GetString(Target, u8),
    /// `get_config_buffer` into a buffer of this size.
    GetBuffer(Target, u8),
    /// `get_config_cbor` into a buffer of this size.
    GetCbor(Target, u8),
    /// `get_value_size`.
    ValueSize(Target),
    /// `remove_key`.
    Remove(Target),
    /// `clear_partition`.
    Clear(Partition),
    /// `save_config(partition, false)`.
    Save(Partition),
    /// `config_init`.
    Reload,
    /// Overwrite one byte of a partition's flash image, offset taken modulo
    /// its length.
    Corrupt(Partition, u16, u8),
    /// Clamp a flash image's `numKeys` to 50 and give it a valid CRC, so a
    /// crafted image loads.
    FixCrc(Partition),
}

impl<'a> Arbitrary<'a> for Op {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        Ok(match u.int_in_range(0u8..=18)? {
            0 => Self::SetUint(u.arbitrary()?, u.arbitrary()?),
            1 => Self::SetInt(u.arbitrary()?, u.arbitrary()?),
            2 => Self::SetFloat(u.arbitrary()?, u.arbitrary()?),
            3 => Self::SetString(u.arbitrary()?, bounded_bytes(u, 60)?),
            4 => Self::SetBuffer(u.arbitrary()?, bounded_bytes(u, 60)?),
            5 => Self::SetCbor(u.arbitrary()?, cbor_value(u)?),
            6 => Self::GetUint(u.arbitrary()?),
            7 => Self::GetInt(u.arbitrary()?),
            8 => Self::GetFloat(u.arbitrary()?),
            9 => Self::GetString(u.arbitrary()?, u.int_in_range(0..=60)?),
            10 => Self::GetBuffer(u.arbitrary()?, u.int_in_range(0..=60)?),
            11 => Self::GetCbor(u.arbitrary()?, u.int_in_range(0..=60)?),
            12 => Self::ValueSize(u.arbitrary()?),
            13 => Self::Remove(u.arbitrary()?),
            14 => Self::Clear(partition(u)?),
            15 => Self::Save(partition(u)?),
            16 => Self::Reload,
            17 => Self::Corrupt(partition(u)?, u.arbitrary()?, u.arbitrary()?),
            _ => Self::FixCrc(partition(u)?),
        })
    }
}

/// A script.
#[derive(Debug, Clone)]
pub struct ConfigInput {
    /// Operations, in order.
    pub ops: Vec<Op>,
}

impl<'a> Arbitrary<'a> for ConfigInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let n = u.int_in_range(0usize..=64)?;
        let mut ops = Vec::with_capacity(n);
        for _ in 0..n {
            ops.push(u.arbitrary()?);
        }
        Ok(Self { ops })
    }
}

/// System twice as often as the others, so keys collide.
fn partition(u: &mut Unstructured<'_>) -> Result<Partition> {
    Ok(match u.int_in_range(0u8..=3)? {
        0 => Partition::User,
        3 => Partition::Hardware,
        _ => Partition::System,
    })
}

fn bounded_bytes(u: &mut Unstructured<'_>, max: usize) -> Result<Vec<u8>> {
    let len = u.int_in_range(0..=max)?;
    let mut bytes = vec![0u8; len];
    u.fill_buffer(&mut bytes)?;
    Ok(bytes)
}

/// A `set_config_cbor` value: raw bytes, or something shaped like a string,
/// a chunked string, a scalar or a container head, with trailing bytes.
fn cbor_value(u: &mut Unstructured<'_>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    match u.int_in_range(0u8..=4)? {
        0 => return bounded_bytes(u, 52),
        1 => {
            let major = if u.arbitrary()? { 0x40 } else { 0x60 };
            let body = bounded_bytes(u, 52)?;
            push_head(&mut out, major, body.len() as u64);
            out.extend(body);
        }
        2 => {
            let major = if u.arbitrary()? { 0x40 } else { 0x60 };
            out.push(major | 31);
            for _ in 0..u.int_in_range(0..=4)? {
                // Mostly the same major type; sometimes the other, or a
                // nested indefinite chunk, both of which tinycbor refuses.
                let chunk_major = match u.int_in_range(0u8..=7)? {
                    0 => major ^ 0x20,
                    _ => major,
                };
                if u.ratio(1u8, 16)? {
                    out.push(chunk_major | 31);
                }
                let body = bounded_bytes(u, 10)?;
                push_head(&mut out, chunk_major, body.len() as u64);
                out.extend(body);
            }
            if u.ratio(7u8, 8)? {
                out.push(0xff);
            }
        }
        3 => match u.int_in_range(0u8..=4)? {
            0 => push_head(&mut out, 0x00, u.arbitrary()?),
            1 => push_head(&mut out, 0x20, u.arbitrary()?),
            2 => {
                out.push(0xfa);
                out.extend(u.arbitrary::<[u8; 4]>()?);
            }
            3 => {
                out.push(0xf9);
                out.extend(u.arbitrary::<[u8; 2]>()?);
            }
            _ => {
                out.push(0xfb);
                out.extend(u.arbitrary::<[u8; 8]>()?);
            }
        },
        _ => {
            let major = [0x80u8, 0xa0, 0xc0, 0xe0][u.choose_index(4)?];
            if u.arbitrary()? {
                out.push(major | 31);
            } else {
                push_head(&mut out, major, u.int_in_range(0..=300)?);
            }
        }
    }
    out.extend(bounded_bytes(u, 4)?);
    Ok(out)
}

/// A minimal-length head.
fn push_head(out: &mut Vec<u8>, major: u8, arg: u64) {
    match arg {
        0..=23 => out.push(major | arg as u8),
        24..=0xff => out.extend([major | 24, arg as u8]),
        0x100..=0xffff => {
            out.push(major | 25);
            out.extend((arg as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(major | 26);
            out.extend((arg as u32).to_be_bytes());
        }
        _ => {
            out.push(major | 27);
            out.extend(arg.to_be_bytes());
        }
    }
}

/// The Rust store, its flash, and a handle on the C's.
struct Pair {
    store: ConfigStore,
    flash: RamConfigStorage,
}

fn c_partition(p: Partition) -> bm_wire_sys::BmConfigPartition {
    p as bm_wire_sys::BmConfigPartition
}

/// The C's RAM image for `p`.
fn c_image(p: Partition) -> Vec<u8> {
    let mut n = 0u8;
    // SAFETY: `get_stored_keys` returns `&CONFIGS[p].ram_buffer[HEADER_LEN]`,
    // a buffer of 10 KiB of which the `ConfigPartition` is the first
    // IMAGE_LEN bytes, so the slice is inside one live static object.
    unsafe {
        let keys = bm_wire_sys::get_stored_keys(c_partition(p), &raw mut n);
        let start = keys.cast::<u8>().sub(HEADER_LEN);
        std::slice::from_raw_parts(start, IMAGE_LEN).to_vec()
    }
}

/// The shim's flash image for `p`.
fn c_flash(p: Partition) -> Vec<u8> {
    let mut buf = vec![0u8; IMAGE_LEN];
    // SAFETY: `buf` is live and IMAGE_LEN long.
    let ok =
        unsafe { bm_wire_sys::bm_config_read(c_partition(p), 0, buf.as_mut_ptr(), IMAGE_LEN, 0) };
    assert!(ok, "the shim holds {IMAGE_LEN} bytes per partition");
    buf
}

fn c_write_flash(p: Partition, image: &mut [u8]) {
    // SAFETY: `image` is live and its length is passed with it.
    let ok = unsafe {
        bm_wire_sys::bm_config_write(c_partition(p), 0, image.as_mut_ptr(), image.len(), 0)
    };
    assert!(ok);
}

fn c_needs_commit(p: Partition) -> bool {
    // SAFETY: a plain read of a flag.
    unsafe { bm_wire_sys::needs_commit(c_partition(p)) }
}

fn c_config_init() {
    // SAFETY: loads each partition from the shim's flash into `CONFIGS`.
    unsafe { bm_wire_sys::config_init() };
}

/// [`Op::FixCrc`]'s edit, applied to a flash image.
fn fix_crc(image: &mut [u8]) {
    image[8] %= MAX_NUM_KV as u8 + 1;
    let crc = crc32_ieee(&image[4..]);
    image[..4].copy_from_slice(&crc.to_le_bytes());
}

impl Pair {
    /// Both stores empty and committed, both flashes zero.
    fn reset() -> Self {
        for p in Partition::ALL {
            if c_needs_commit(p) {
                // SAFETY: writes the partition to the shim's flash.
                assert!(unsafe { bm_wire_sys::save_config(c_partition(p), false) });
            }
            c_write_flash(p, &mut [0u8; IMAGE_LEN]);
        }
        c_config_init();

        let mut pair = Self {
            store: ConfigStore::new(LAYOUT),
            flash: RamConfigStorage::new(),
        };
        config_init(&mut pair.store, &mut pair.flash);
        pair.compare_state("reset");
        pair
    }

    fn compare_state(&self, after: &str) {
        for p in Partition::ALL {
            let rust = self.store.partition(p);
            let c = c_image(p);
            if let Some(at) = rust.image().iter().zip(&c).position(|(a, b)| a != b) {
                panic!(
                    "{p:?} RAM image diverged at byte {at} after {after}: \
                     Rust {:02x?}, C {:02x?}",
                    &rust.image()[at..(at + 16).min(IMAGE_LEN)],
                    &c[at..(at + 16).min(IMAGE_LEN)]
                );
            }
            assert_eq!(
                rust.needs_commit(),
                c_needs_commit(p),
                "{p:?} needs_commit diverged after {after}"
            );
            assert_eq!(
                rust.stored_keys().count(),
                usize::from(c[8]),
                "get_stored_keys count"
            );
            assert!(
                self.flash.bytes(p)[..IMAGE_LEN] == c_flash(p)[..],
                "{p:?} flash diverged after {after}"
            );
        }
    }

    fn step(&mut self, op: &Op) {
        let desc = format!("{op:?}");
        match op {
            Op::SetUint(t, v) => self.set(
                t,
                &desc,
                |k, part| part.set_uint(k, *v),
                |p, k, l| unsafe { bm_wire_sys::set_config_uint(p, k, l, *v) },
            ),
            Op::SetInt(t, v) => self.set(
                t,
                &desc,
                |k, part| part.set_int(k, *v),
                |p, k, l| unsafe { bm_wire_sys::set_config_int(p, k, l, *v) },
            ),
            Op::SetFloat(t, bits) => {
                let v = f32::from_bits(*bits);
                self.set(
                    t,
                    &desc,
                    |k, part| part.set_float(k, v),
                    |p, k, l| unsafe { bm_wire_sys::set_config_float(p, k, l, v) },
                );
            }
            Op::SetString(t, value) => {
                self.set(
                    t,
                    &desc,
                    |k, part| part.set_string(k, value),
                    |p, k, l| unsafe {
                        bm_wire_sys::set_config_string(p, k, l, value.as_ptr().cast(), value.len())
                    },
                );
            }
            Op::SetBuffer(t, value) => {
                self.set(
                    t,
                    &desc,
                    |k, part| part.set_buffer(k, value),
                    |p, k, l| unsafe {
                        bm_wire_sys::set_config_buffer(p, k, l, value.as_ptr(), value.len())
                    },
                );
            }
            Op::SetCbor(t, value) => {
                let mut c_value = value.clone();
                self.set(
                    t,
                    &desc,
                    |k, part| part.set_cbor(k, value),
                    |p, k, l| unsafe {
                        bm_wire_sys::set_config_cbor(p, k, l, c_value.as_mut_ptr(), c_value.len())
                    },
                );
            }
            Op::GetUint(t) => {
                let rust = self.store.partition(t.partition).get_uint(t.key.key());
                let mut out = 0u32;
                let ok = with_c_key(t, |p, k, l| unsafe {
                    bm_wire_sys::get_config_uint(p, k, l, &raw mut out)
                });
                assert_eq!(rust, ok.then_some(out), "{desc}");
            }
            Op::GetInt(t) => {
                if self.is_int64_min_minus_one(t) {
                    return;
                }
                let rust = self.store.partition(t.partition).get_int(t.key.key());
                let mut out = 0i32;
                let ok = with_c_key(t, |p, k, l| unsafe {
                    bm_wire_sys::get_config_int(p, k, l, &raw mut out)
                });
                assert_eq!(rust, ok.then_some(out), "{desc}");
            }
            Op::GetFloat(t) => {
                let rust = self.store.partition(t.partition).get_float(t.key.key());
                let mut out = 0f32;
                let ok = with_c_key(t, |p, k, l| unsafe {
                    bm_wire_sys::get_config_float(p, k, l, &raw mut out)
                });
                assert_eq!(
                    rust.map(f32::to_bits),
                    ok.then_some(out.to_bits()),
                    "{desc}"
                );
            }
            Op::GetString(t, cap) | Op::GetBuffer(t, cap) => {
                let text = matches!(op, Op::GetString(..));
                let cap = usize::from(*cap);
                let mut rust_buf = vec![0xa5u8; cap];
                let part = self.store.partition(t.partition);
                let rust = if text {
                    part.get_string(t.key.key(), &mut rust_buf)
                } else {
                    part.get_buffer(t.key.key(), &mut rust_buf)
                };
                let rust = match rust {
                    Ok(n) => (true, n),
                    Err(CopyError::TooSmall(n)) => (false, n),
                    Err(CopyError::Refused) => (false, cap),
                };
                let mut c_buf = vec![0xa5u8; cap];
                let mut len = cap;
                let ok = with_c_key(t, |p, k, l| unsafe {
                    if text {
                        bm_wire_sys::get_config_string(
                            p,
                            k,
                            l,
                            c_buf.as_mut_ptr().cast(),
                            &raw mut len,
                        )
                    } else {
                        bm_wire_sys::get_config_buffer(p, k, l, c_buf.as_mut_ptr(), &raw mut len)
                    }
                });
                assert_eq!(rust, (ok, len), "{desc}: (returned, *value_len)");
                assert_eq!(rust_buf, c_buf, "{desc}: the caller's buffer");
            }
            Op::GetCbor(t, cap) => {
                let cap = usize::from(*cap);
                let mut rust_buf = vec![0xa5u8; cap];
                let rust = self
                    .store
                    .partition(t.partition)
                    .get_cbor(t.key.key(), &mut rust_buf);
                let mut c_buf = vec![0xa5u8; cap];
                let mut len = cap;
                let ok = with_c_key(t, |p, k, l| unsafe {
                    bm_wire_sys::get_config_cbor(p, k, l, c_buf.as_mut_ptr(), &raw mut len)
                });
                assert_eq!(
                    rust.map_or((false, cap), |n| (true, n)),
                    (ok, len),
                    "{desc}"
                );
                assert_eq!(rust_buf, c_buf, "{desc}: the caller's buffer");
            }
            Op::ValueSize(t) => {
                let rust = self.store.partition(t.partition).value_size(t.key.key());
                let mut size = 0usize;
                let ok = with_c_key(t, |p, k, l| unsafe {
                    bm_wire_sys::get_value_size(p, k, l, &raw mut size)
                });
                assert_eq!(rust, ok.then_some(size), "{desc}");
            }
            Op::Remove(t) => self.set(
                t,
                &desc,
                |k, part| part.remove_key(k),
                |p, k, l| unsafe { bm_wire_sys::remove_key(p, k, l) },
            ),
            Op::Clear(p) => {
                self.store.partition_mut(*p).clear();
                // SAFETY: zeroes one partition of `CONFIGS`.
                assert!(unsafe { bm_wire_sys::clear_partition(c_partition(*p)) });
            }
            Op::Save(p) => {
                let rust = save_config(&mut self.store, *p, &mut self.flash, false);
                // SAFETY: writes one partition to the shim's flash.
                let c = unsafe { bm_wire_sys::save_config(c_partition(*p), false) };
                assert_eq!(rust, c, "{desc}");
            }
            Op::Reload => {
                config_init(&mut self.store, &mut self.flash);
                c_config_init();
            }
            Op::Corrupt(p, offset, byte) => {
                let at = usize::from(*offset) % IMAGE_LEN;
                self.flash.bytes_mut(*p)[at] = *byte;
                let mut c = c_flash(*p);
                c[at] = *byte;
                c_write_flash(*p, &mut c);
            }
            Op::FixCrc(p) => {
                fix_crc(&mut self.flash.bytes_mut(*p)[..IMAGE_LEN]);
                let mut c = c_flash(*p);
                fix_crc(&mut c);
                c_write_flash(*p, &mut c);
            }
        }
        self.compare_state(&desc);
    }

    /// A mutation with a `bool` result.
    fn set(
        &mut self,
        t: &Target,
        desc: &str,
        rust: impl FnOnce(Key<'_>, &mut bm_wire::configuration::ConfigPartition) -> bool,
        c: impl FnOnce(bm_wire_sys::BmConfigPartition, *const std::ffi::c_char, usize) -> bool,
    ) {
        let rust = rust(t.key.key(), self.store.partition_mut(t.partition));
        let c = with_c_key(t, c);
        assert_eq!(rust, c, "{desc}");
    }

    /// Whether `t` names a value `get_config_int` negates `INT64_MIN` for.
    fn is_int64_min_minus_one(&self, t: &Target) -> bool {
        let mut slot = [0u8; 50];
        self.store
            .partition(t.partition)
            .get_cbor(t.key.key(), &mut slot)
            .and_then(|_| Head::parse(&slot))
            .is_some_and(|h| h.major == 1 && h.arg == 1 << 63)
    }
}

/// Call into the C with `t`'s partition and a NUL-padded copy of its key.
fn with_c_key<T>(
    t: &Target,
    f: impl FnOnce(bm_wire_sys::BmConfigPartition, *const std::ffi::c_char, usize) -> T,
) -> T {
    let bytes = t.key.c_bytes();
    f(c_partition(t.partition), bytes.as_ptr().cast(), t.key.len)
}

/// Run `input` against both stores.
///
/// # Panics
///
/// On any divergence in a return value, an output, a RAM image, a
/// `needs_commit` flag or a flash image.
pub fn check(input: &ConfigInput) {
    let _guard = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut pair = Pair::reset();
    for op in &input.ops {
        pair.step(op);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(p: Partition, key: &[u8]) -> Target {
        Target {
            partition: p,
            key: KeyInput {
                text: key.to_vec(),
                len: key.len(),
            },
        }
    }

    fn sys(key: &[u8]) -> Target {
        t(Partition::System, key)
    }

    fn run(ops: Vec<Op>) {
        check(&ConfigInput { ops });
    }

    const SILLY: &[u8] = b"The quick brown fox jumps over the lazy dog";
    const BYTES: &[u8] = &[0xde, 0xad, 0xbe, 0xef, 0x5a, 0xad, 0xda, 0xad, 0xb0, 0xdd];

    /// `configuration_test.cpp`'s `BasicTest`, as a script.
    #[test]
    fn basic_test() {
        run(vec![
            Op::SetUint(sys(b"foo"), 42),
            Op::GetUint(sys(b"foo")),
            Op::SetUint(sys(b"foo"), 999),
            Op::SetInt(sys(b"bar"), -1000),
            Op::SetFloat(sys(b"baz"), 0x4049_0fd0),
            Op::SetString(sys(b"silly"), SILLY.to_vec()),
            Op::GetString(sys(b"silly"), 100),
            Op::SetBuffer(sys(b"bytes"), BYTES.to_vec()),
            Op::GetBuffer(sys(b"bytes"), 10),
            Op::Remove(sys(b"foo")),
            Op::GetUint(sys(b"foo")),
            Op::GetInt(sys(b"bar")),
            Op::GetFloat(sys(b"baz")),
            Op::Save(Partition::System),
            Op::Reload,
            Op::GetString(sys(b"silly"), 43),
            Op::GetBuffer(sys(b"bytes"), 9),
        ]);
    }

    /// `cborGetSet`: a whole slot copied under another key, stale tail and
    /// all.
    #[test]
    fn cbor_get_set() {
        run(vec![
            Op::SetUint(sys(b"foo"), 42),
            Op::GetCbor(sys(b"foo"), 50),
            Op::SetString(sys(b"silly"), SILLY.to_vec()),
            Op::SetCbor(sys(b"bar"), {
                let mut v = vec![0x78, 0x2b];
                v.extend_from_slice(SILLY);
                v.extend([0; 5]);
                v
            }),
            Op::ValueSize(sys(b"bar")),
            Op::GetString(sys(b"bar"), 100),
        ]);
    }

    /// Divergence #45: a 32-byte key is stored as 31 and never found again,
    /// so every set appends.
    #[test]
    fn a_32_byte_key_appends_on_every_set() {
        let key = b"abcdefghijklmnopqrstuvwxyz012345";
        let mut ops = vec![];
        for v in 0..52 {
            ops.push(Op::SetUint(sys(key), v));
            ops.push(Op::GetUint(sys(key)));
        }
        run(ops);
    }

    /// Divergence #46: a string too long for the slot leaves its head there.
    #[test]
    fn an_oversized_string_overwrites_the_head_of_the_old_value() {
        run(vec![
            Op::SetString(sys(b"silly"), b"hello".to_vec()),
            Op::SetString(sys(b"silly"), vec![b'x'; 60]),
            Op::GetString(sys(b"silly"), 60),
            Op::ValueSize(sys(b"silly")),
            Op::SetBuffer(sys(b"new"), vec![1; 49]),
        ]);
    }

    /// Divergence #46: typed setters cannot overwrite at 50 keys;
    /// `set_config_cbor` can.
    #[test]
    fn a_full_partition() {
        let mut ops: Vec<Op> = (0..50)
            .map(|i| Op::SetUint(sys(format!("k{i}").as_bytes()), i))
            .collect();
        ops.push(Op::SetUint(sys(b"k0"), 7));
        ops.push(Op::SetCbor(sys(b"k0"), vec![0x07]));
        ops.push(Op::GetUint(sys(b"k0")));
        ops.push(Op::Remove(sys(b"k10")));
        ops.push(Op::SetUint(sys(b"k0"), 8));
        run(ops);
    }

    /// Divergence #47: a corrupt image is kept in RAM with only its header
    /// reset, and saved back out.
    #[test]
    fn a_corrupt_image_survives_in_ram() {
        run(vec![
            Op::SetString(sys(b"a"), b"kept".to_vec()),
            Op::Save(Partition::System),
            Op::Corrupt(Partition::System, 3000, 0x55),
            Op::Reload,
            Op::GetString(sys(b"a"), 10),
            Op::SetUint(sys(b"b"), 1),
            Op::Save(Partition::System),
        ]);
    }

    /// A crafted image with a valid CRC: a key with no NUL and a value the
    /// setters would never write.
    #[test]
    fn a_crafted_image_with_a_valid_crc() {
        let key_len_at = HEADER_LEN + 32;
        run(vec![
            Op::SetUint(sys(b"foo"), 1),
            Op::Save(Partition::System),
            Op::Corrupt(Partition::System, 8, 3),
            Op::Corrupt(Partition::System, HEADER_LEN as u16 + 3, b'x'),
            Op::Corrupt(Partition::System, key_len_at as u16, 5),
            Op::Corrupt(Partition::System, LAYOUT.value_offset(1) as u16, 0x7f),
            Op::FixCrc(Partition::System),
            Op::Reload,
            Op::GetUint(sys(b"foox")),
            Op::Remove(Target {
                partition: Partition::System,
                key: KeyInput {
                    text: b"foox".to_vec(),
                    len: 5,
                },
            }),
            Op::GetString(sys(b""), 20),
            Op::GetString(t(Partition::System, b""), 5),
        ]);
    }

    /// Chunked strings: partial copies, and the refusals.
    #[test]
    fn chunked_strings() {
        for value in [
            vec![0x7f, 0x63, b'a', b'b', b'c', 0x62, b'd', b'e', 0xff],
            vec![0x7f, 0x63, b'a', b'b', b'c', 0x42, b'd', b'e', 0xff],
            vec![0x7f, 0x63, b'a', b'b', b'c', 0x7f, 0x61, b'd', 0xff, 0xff],
            vec![0x7f, 0x63, b'a', b'b', b'c'],
            vec![0x5f, 0xff],
            vec![0x78, 0x40, b'a'],
        ] {
            let mut ops = vec![Op::SetCbor(sys(b"s"), value)];
            for cap in [0, 2, 3, 4, 5, 6, 40] {
                ops.push(Op::GetString(sys(b"s"), cap));
                ops.push(Op::GetBuffer(sys(b"s"), cap));
            }
            ops.push(Op::ValueSize(sys(b"s")));
            run(ops);
        }
    }

    /// Divergence #45: `strncmp` stops at the first NUL, so two keys of the
    /// same length that differ only after it are one key.
    #[test]
    fn keys_that_differ_after_a_nul_are_one_key() {
        run(vec![
            Op::SetUint(sys(b"ab\0c"), 1),
            Op::SetUint(sys(b"ab\0d"), 2),
            Op::GetUint(sys(b"ab\0c")),
            Op::GetUint(sys(b"ab")),
            Op::Remove(sys(b"ab\0d")),
        ]);
    }

    /// The key `bcmp/config.c` hands `set_config_cbor`: the value follows it
    /// with no NUL, so `key_buf` gets the value's leading bytes.
    #[test]
    fn a_key_followed_by_its_value() {
        let key = KeyInput {
            text: b"foo\x18\x2a".to_vec(),
            len: 3,
        };
        run(vec![
            Op::SetCbor(
                Target {
                    partition: Partition::System,
                    key,
                },
                vec![0x18, 0x2a],
            ),
            Op::GetUint(sys(b"foo")),
            Op::SetUint(sys(b"foo"), 1),
        ]);
    }
}
