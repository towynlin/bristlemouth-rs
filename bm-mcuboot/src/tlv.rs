//! The TLV area after an image's body: `struct image_tlv_info`, then
//! `struct image_tlv` entries (`image.h`), little-endian.
//!
//! Encoding is `imgtool`'s for bm_protocol's arguments: no protected area,
//! and the TLVs in the order `scripts/imgtool/image.py` writes them.
//!
//! Decoding walks the area as `boot/bootutil/src/tlv.c` does with
//! `IMAGE_TLV_ANY`, over a byte slice instead of flash. Where the C reads
//! past what the area declares, this reports [`TlvError::Truncated`]:
//!
//! | Input | `tlv.c` | Here |
//! |---|---|---|
//! | `ih_hdr_size + ih_img_size` over `u32::MAX` | wraps | `Truncated` |
//! | The area ends past the slice | reads flash beyond it | `Truncated` |
//! | An entry's header or value passes `it_tlv_tot` | yielded; the caller reads flash beyond the area | `Truncated` |

use crate::image::Header;

/// `IMAGE_TLV_INFO_MAGIC`.
pub const INFO_MAGIC: u16 = 0x6907;
/// `IMAGE_TLV_PROT_INFO_MAGIC`.
pub const PROT_INFO_MAGIC: u16 = 0x6908;
/// `sizeof(struct image_tlv_info)`.
pub const INFO_SIZE: usize = 4;
/// `sizeof(struct image_tlv)`.
pub const ENTRY_SIZE: usize = 4;

/// `IMAGE_TLV_KEYHASH`: SHA-256 of the public key.
pub const KEYHASH: u16 = 0x01;
/// `IMAGE_TLV_SHA256`: SHA-256 of the header, its padding and the body.
pub const SHA256: u16 = 0x10;
/// `IMAGE_TLV_ED25519`: signature of the SHA-256 digest.
pub const ED25519: u16 = 0x24;

/// Length of a SHA-256 digest.
pub const SHA256_LEN: usize = 32;
/// Length of an ed25519 signature.
pub const ED25519_LEN: usize = 64;
/// Size of an unsigned image's TLV area.
pub const UNSIGNED_SIZE: usize = INFO_SIZE + ENTRY_SIZE + SHA256_LEN;
/// Size of an ed25519-signed image's TLV area.
pub const ED25519_SIZE: usize = UNSIGNED_SIZE + ENTRY_SIZE + SHA256_LEN + ENTRY_SIZE + ED25519_LEN;

/// An unsigned image's TLV area: the info header and a [`SHA256`] entry.
pub const fn encode_unsigned(sha256: &[u8; SHA256_LEN]) -> [u8; UNSIGNED_SIZE] {
    let mut out = [0; UNSIGNED_SIZE];
    let at = info(&mut out, UNSIGNED_SIZE as u16);
    entry(&mut out, at, SHA256, sha256);
    out
}

/// An ed25519-signed image's TLV area: [`SHA256`], [`KEYHASH`], [`ED25519`].
pub const fn encode_ed25519(
    sha256: &[u8; SHA256_LEN],
    keyhash: &[u8; SHA256_LEN],
    signature: &[u8; ED25519_LEN],
) -> [u8; ED25519_SIZE] {
    let mut out = [0; ED25519_SIZE];
    let at = info(&mut out, ED25519_SIZE as u16);
    let at = entry(&mut out, at, SHA256, sha256);
    let at = entry(&mut out, at, KEYHASH, keyhash);
    entry(&mut out, at, ED25519, signature);
    out
}

const fn info(out: &mut [u8], total: u16) -> usize {
    put(out, 0, &INFO_MAGIC.to_le_bytes());
    put(out, 2, &total.to_le_bytes());
    INFO_SIZE
}

const fn entry(out: &mut [u8], at: usize, kind: u16, value: &[u8]) -> usize {
    put(out, at, &kind.to_le_bytes());
    put(out, at + 2, &(value.len() as u16).to_le_bytes());
    put(out, at + ENTRY_SIZE, value);
    at + ENTRY_SIZE + value.len()
}

const fn put(out: &mut [u8], at: usize, bytes: &[u8]) {
    let mut i = 0;
    while i < bytes.len() {
        out[at + i] = bytes[i];
        i += 1;
    }
}

/// Why a TLV area could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlvError {
    /// The slice, or the area's declared length, ends before the bytes
    /// needed.
    Truncated,
    /// The info header's magic is not [`INFO_MAGIC`].
    BadMagic,
    /// `ih_protect_tlv_size` is not the protected area's `it_tlv_tot`, or is
    /// non-zero with no protected area.
    ProtectedSize,
}

/// One entry of the area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tlv<'a> {
    /// `it_type`.
    pub kind: u16,
    /// The `it_len` bytes after the entry header.
    pub value: &'a [u8],
    /// Whether the entry is in the protected area.
    pub protected: bool,
}

/// An image's TLV area, as `bootutil_tlv_iter_begin` accepts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TlvArea<'a> {
    image: &'a [u8],
    start: usize,
    first: usize,
    prot_end: usize,
    end: usize,
}

impl<'a> TlvArea<'a> {
    /// Find the area in `image`, which starts at the header `header` was
    /// decoded from.
    pub fn parse(image: &'a [u8], header: &Header) -> Result<Self, TlvError> {
        let start = header.tlv_off().ok_or(TlvError::Truncated)? as usize;
        let protect = usize::from(header.protect_tlv_size);
        let (mut magic, mut total) = pair(image, start)?;
        if magic == PROT_INFO_MAGIC {
            if protect != usize::from(total) {
                return Err(TlvError::ProtectedSize);
            }
            (magic, total) = pair(image, start + protect)?;
        } else if protect != 0 {
            return Err(TlvError::ProtectedSize);
        }
        if magic != INFO_MAGIC {
            return Err(TlvError::BadMagic);
        }
        let prot_end = start + protect;
        let end = prot_end + usize::from(total);
        if end > image.len() {
            return Err(TlvError::Truncated);
        }
        Ok(Self {
            image,
            start,
            first: start + INFO_SIZE,
            prot_end,
            end,
        })
    }

    /// Offset of the area in the image: `BOOT_TLV_OFF`.
    pub const fn start(&self) -> usize {
        self.start
    }

    /// Offset of the first byte after the area. A `.dfu.bin` is this long.
    pub const fn end(&self) -> usize {
        self.end
    }

    /// The entries in order, protected first. Ends after the first error.
    pub const fn iter(&self) -> TlvIter<'a> {
        TlvIter {
            area: *self,
            off: self.first,
        }
    }

    /// The value of the first entry of `kind`, if the area reads without
    /// error up to it.
    pub fn find(&self, kind: u16) -> Option<&'a [u8]> {
        self.iter()
            .map_while(Result::ok)
            .find(|tlv| tlv.kind == kind)
            .map(|tlv| tlv.value)
    }
}

impl<'a> IntoIterator for &TlvArea<'a> {
    type Item = Result<Tlv<'a>, TlvError>;
    type IntoIter = TlvIter<'a>;

    fn into_iter(self) -> TlvIter<'a> {
        self.iter()
    }
}

/// `bootutil_tlv_iter_next` with `IMAGE_TLV_ANY`.
#[derive(Clone, Debug)]
pub struct TlvIter<'a> {
    area: TlvArea<'a>,
    off: usize,
}

impl<'a> Iterator for TlvIter<'a> {
    type Item = Result<Tlv<'a>, TlvError>;

    fn next(&mut self) -> Option<Self::Item> {
        let area = &self.area;
        if self.off >= area.end {
            return None;
        }
        // The unprotected area's info header sits between the two areas.
        if area.prot_end != area.start && self.off == area.prot_end {
            self.off += INFO_SIZE;
        }
        let item = (|| {
            let value_off = self.off + ENTRY_SIZE;
            if value_off > area.end {
                return Err(TlvError::Truncated);
            }
            let (kind, len) = pair(area.image, self.off)?;
            let value_end = value_off + usize::from(len);
            if value_end > area.end {
                return Err(TlvError::Truncated);
            }
            Ok((
                Tlv {
                    kind,
                    value: &area.image[value_off..value_end],
                    protected: self.off < area.prot_end,
                },
                value_end,
            ))
        })();
        Some(match item {
            Ok((tlv, next)) => {
                self.off = next;
                Ok(tlv)
            }
            Err(e) => {
                self.off = area.end;
                Err(e)
            }
        })
    }
}

/// Two little-endian `u16`s at `off`.
fn pair(bytes: &[u8], off: usize) -> Result<(u16, u16), TlvError> {
    let b = off
        .checked_add(4)
        .and_then(|end| bytes.get(off..end))
        .ok_or(TlvError::Truncated)?;
    Ok((
        u16::from_le_bytes([b[0], b[1]]),
        u16::from_le_bytes([b[2], b[3]]),
    ))
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::image::Version;
    use std::vec::Vec;

    /// The last 40 bytes of bm_protocol's C `hello_world` `.dfu.bin`.
    const C_HELLO_WORLD_TLVS: [u8; UNSIGNED_SIZE] = [
        0x07, 0x69, 0x28, 0x00, 0x10, 0x00, 0x20, 0x00, 0x16, 0x93, 0x7e, 0x4a, 0x8d, 0x8b, 0xc3,
        0x83, 0x72, 0x0f, 0xff, 0x23, 0x83, 0x5a, 0x0f, 0x36, 0xc2, 0x45, 0x2c, 0xde, 0x4d, 0x75,
        0xeb, 0x3c, 0xaf, 0xa3, 0xea, 0x89, 0x58, 0xd4, 0xfa, 0x86,
    ];

    /// A header, `body`, then `tlvs`, with no header padding.
    fn image(body: &[u8], protect: u16, tlvs: &[u8]) -> (Header, Vec<u8>) {
        let mut header = Header::new(body.len() as u32, Version::default());
        header.hdr_size = 32;
        header.protect_tlv_size = protect;
        let mut image = header.encode().to_vec();
        image.extend_from_slice(body);
        image.extend_from_slice(tlvs);
        (header, image)
    }

    type Entry<'a> = Result<(u16, &'a [u8], bool), TlvError>;

    fn entries<'a>(area: &TlvArea<'a>) -> Vec<Entry<'a>> {
        area.iter()
            .map(|r| r.map(|t| (t.kind, t.value, t.protected)))
            .collect()
    }

    #[test]
    fn the_c_images_tlv_area() {
        let digest: [u8; 32] = C_HELLO_WORLD_TLVS[8..].try_into().unwrap();
        assert_eq!(encode_unsigned(&digest), C_HELLO_WORLD_TLVS);

        let (header, image) = image(b"body", 0, &C_HELLO_WORLD_TLVS);
        let area = TlvArea::parse(&image, &header).unwrap();
        assert_eq!(area.start(), 36);
        assert_eq!(area.end(), image.len());
        assert_eq!(entries(&area), [Ok((SHA256, &digest[..], false))]);
        assert_eq!(area.find(SHA256), Some(&digest[..]));
        assert_eq!(area.find(ED25519), None);
    }

    #[test]
    fn a_signed_area_is_three_entries() {
        let (sha, key, sig) = ([0x11; 32], [0x22; 32], [0x33; 64]);
        let tlvs = encode_ed25519(&sha, &key, &sig);
        assert_eq!(tlvs.len(), 144);
        assert_eq!(tlvs[..8], [0x07, 0x69, 0x90, 0x00, 0x10, 0x00, 0x20, 0x00]);
        assert_eq!(tlvs[40..44], [0x01, 0x00, 0x20, 0x00]);
        assert_eq!(tlvs[76..80], [0x24, 0x00, 0x40, 0x00]);

        let (header, image) = image(b"", 0, &tlvs);
        let area = TlvArea::parse(&image, &header).unwrap();
        assert_eq!(
            entries(&area),
            [
                Ok((SHA256, &sha[..], false)),
                Ok((KEYHASH, &key[..], false)),
                Ok((ED25519, &sig[..], false)),
            ]
        );
    }

    #[test]
    fn a_protected_area_comes_first() {
        // Protected: info and one 2-byte entry of type 0x50. Then the
        // unprotected info and one empty entry.
        let tlvs = [
            0x08, 0x69, 0x0a, 0x00, 0x50, 0x00, 0x02, 0x00, 0xaa, 0xbb, // protected
            0x07, 0x69, 0x08, 0x00, 0x10, 0x00, 0x00, 0x00,
        ];
        let (header, image) = image(b"xy", 10, &tlvs);
        let area = TlvArea::parse(&image, &header).unwrap();
        assert_eq!(area.end(), image.len());
        assert_eq!(
            entries(&area),
            [
                Ok((0x50, &[0xaa, 0xbb][..], true)),
                Ok((SHA256, &[][..], false))
            ]
        );

        let mut header = header;
        header.protect_tlv_size = 9;
        assert_eq!(
            TlvArea::parse(&image, &header),
            Err(TlvError::ProtectedSize)
        );
    }

    #[test]
    fn refusals() {
        let good = encode_unsigned(&[0; 32]);

        // No protected area, but the header declares one.
        let (header, bytes) = image(b"", 4, &good);
        assert_eq!(
            TlvArea::parse(&bytes, &header),
            Err(TlvError::ProtectedSize)
        );

        let mut bad = good;
        bad[0] = 0x06;
        let (header, bytes) = image(b"", 0, &bad);
        assert_eq!(TlvArea::parse(&bytes, &header), Err(TlvError::BadMagic));

        // The slice ends inside the info header, then inside the area.
        let (header, bytes) = image(b"", 0, &good);
        assert_eq!(
            TlvArea::parse(&bytes[..35], &header),
            Err(TlvError::Truncated)
        );
        assert_eq!(
            TlvArea::parse(&bytes[..bytes.len() - 1], &header),
            Err(TlvError::Truncated)
        );

        // An entry longer than the area declares.
        let mut long = good;
        long[6] = 0x21;
        let (header, bytes) = image(b"", 0, &long);
        let area = TlvArea::parse(&bytes, &header).unwrap();
        assert_eq!(entries(&area), [Err(TlvError::Truncated)]);
        assert_eq!(area.find(SHA256), None);

        // `it_tlv_tot` smaller than its own header: no entries.
        let mut empty = good;
        empty[2] = 0x00;
        let (header, bytes) = image(b"", 0, &empty);
        let area = TlvArea::parse(&bytes, &header).unwrap();
        assert_eq!(entries(&area), []);
    }
}
