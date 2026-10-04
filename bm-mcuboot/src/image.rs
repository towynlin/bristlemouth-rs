//! `struct image_header` and `struct image_version`
//! (`boot/bootutil/include/bootutil/image.h`), little-endian.

/// `IMAGE_MAGIC`.
pub const IMAGE_MAGIC: u32 = 0x96f3_b83d;
/// `IMAGE_HEADER_SIZE`, `sizeof(struct image_header)`.
pub const HEADER_SIZE: usize = 32;
/// `imgtool --header-size` in bm_protocol's build: the body starts here and
/// the bytes from [`HEADER_SIZE`] to it are `0xFF` (`--pad-header`).
pub const BM_HDR_SIZE: u16 = 0x200;

/// `struct image_version`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Version {
    /// `iv_major`.
    pub major: u8,
    /// `iv_minor`.
    pub minor: u8,
    /// `iv_revision`.
    pub revision: u16,
    /// `iv_build_num`. bm_protocol puts the git SHA here.
    pub build_num: u32,
}

impl Version {
    /// `sizeof(struct image_version)`.
    pub const SIZE: usize = 8;

    pub const fn encode(&self) -> [u8; Self::SIZE] {
        let r = self.revision.to_le_bytes();
        let b = self.build_num.to_le_bytes();
        [self.major, self.minor, r[0], r[1], b[0], b[1], b[2], b[3]]
    }

    pub const fn decode(bytes: &[u8; Self::SIZE]) -> Self {
        Self {
            major: bytes[0],
            minor: bytes[1],
            revision: u16::from_le_bytes([bytes[2], bytes[3]]),
            build_num: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        }
    }
}

/// `struct image_header`. Every field is carried as read: `decode` checks
/// nothing, as the C's cast of the flash bytes does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// `ih_magic`: [`IMAGE_MAGIC`] in an image MCUboot accepts.
    pub magic: u32,
    /// `ih_load_addr`.
    pub load_addr: u32,
    /// `ih_hdr_size`: offset of the body.
    pub hdr_size: u16,
    /// `ih_protect_tlv_size`.
    pub protect_tlv_size: u16,
    /// `ih_img_size`: the body's length, without header or TLVs.
    pub img_size: u32,
    /// `ih_flags`.
    pub flags: u32,
    /// `ih_ver`.
    pub version: Version,
    /// `_pad1`.
    pub pad1: u32,
}

impl Header {
    /// The header `imgtool` writes with bm_protocol's arguments: load
    /// address 0, [`BM_HDR_SIZE`], no protected TLVs, no flags.
    pub const fn new(img_size: u32, version: Version) -> Self {
        Self {
            magic: IMAGE_MAGIC,
            load_addr: 0,
            hdr_size: BM_HDR_SIZE,
            protect_tlv_size: 0,
            img_size,
            flags: 0,
            version,
            pad1: 0,
        }
    }

    pub const fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut out = [0; HEADER_SIZE];
        put(&mut out, 0, &self.magic.to_le_bytes());
        put(&mut out, 4, &self.load_addr.to_le_bytes());
        put(&mut out, 8, &self.hdr_size.to_le_bytes());
        put(&mut out, 10, &self.protect_tlv_size.to_le_bytes());
        put(&mut out, 12, &self.img_size.to_le_bytes());
        put(&mut out, 16, &self.flags.to_le_bytes());
        put(&mut out, 20, &self.version.encode());
        put(&mut out, 28, &self.pad1.to_le_bytes());
        out
    }

    /// The first [`HEADER_SIZE`] bytes of `bytes`; `None` if there are fewer.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let b: &[u8; HEADER_SIZE] = bytes.get(..HEADER_SIZE)?.try_into().ok()?;
        Some(Self {
            magic: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            load_addr: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            hdr_size: u16::from_le_bytes([b[8], b[9]]),
            protect_tlv_size: u16::from_le_bytes([b[10], b[11]]),
            img_size: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
            flags: u32::from_le_bytes([b[16], b[17], b[18], b[19]]),
            version: Version::decode(&[b[20], b[21], b[22], b[23], b[24], b[25], b[26], b[27]]),
            pad1: u32::from_le_bytes([b[28], b[29], b[30], b[31]]),
        })
    }

    /// `BOOT_TLV_OFF`: `ih_hdr_size + ih_img_size`, where the TLV area
    /// starts. The C adds in `uint32_t` and wraps; this returns `None`.
    pub const fn tlv_off(&self) -> Option<u32> {
        self.img_size.checked_add(self.hdr_size as u32)
    }
}

const fn put(out: &mut [u8; HEADER_SIZE], at: usize, bytes: &[u8]) {
    let mut i = 0;
    while i < bytes.len() {
        out[at + i] = bytes[i];
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first 32 bytes of bm_protocol's C `hello_world` `.dfu.bin`
    /// (`bm_mote_v1.0-hello_world-dbg.elf.dfu.bin`, bm_core v0.13.12).
    const C_HELLO_WORLD: [u8; HEADER_SIZE] = [
        0x3d, 0xb8, 0xf3, 0x96, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0xb4, 0xda, 0x03,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0d, 0x0c, 0x00, 0xd0, 0xb5, 0xd8, 0x62, 0x00, 0x00,
        0x00, 0x00,
    ];

    #[test]
    fn the_c_images_header() {
        let header = Header::new(
            252_596,
            Version {
                major: 0,
                minor: 13,
                revision: 12,
                build_num: 1_658_369_488,
            },
        );
        assert_eq!(header.encode(), C_HELLO_WORLD);
        assert_eq!(Header::decode(&C_HELLO_WORLD), Some(header));
        assert_eq!(header.magic, 0x96f3_b83d);
        assert_eq!(header.hdr_size, 0x200);
        assert_eq!(header.tlv_off(), Some(0x200 + 252_596));

        // `--pad-header`: 0xFF from the header to the body.
        let mut padded = [0xFF; 0x200];
        padded[..HEADER_SIZE].copy_from_slice(&C_HELLO_WORLD);
        assert_eq!(Header::decode(&padded), Some(header));
    }

    #[test]
    fn every_field_round_trips() {
        let header = Header {
            magic: 0x0102_0304,
            load_addr: 0x0506_0708,
            hdr_size: 0x090a,
            protect_tlv_size: 0x0b0c,
            img_size: 0x0d0e_0f10,
            flags: 0x1112_1314,
            version: Version {
                major: 0x15,
                minor: 0x16,
                revision: 0x1718,
                build_num: 0x191a_1b1c,
            },
            pad1: 0x1d1e_1f20,
        };
        assert_eq!(
            header.encode(),
            [
                0x04, 0x03, 0x02, 0x01, 0x08, 0x07, 0x06, 0x05, 0x0a, 0x09, 0x0c, 0x0b, 0x10, 0x0f,
                0x0e, 0x0d, 0x14, 0x13, 0x12, 0x11, 0x15, 0x16, 0x18, 0x17, 0x1c, 0x1b, 0x1a, 0x19,
                0x20, 0x1f, 0x1e, 0x1d
            ]
        );
        assert_eq!(Header::decode(&header.encode()), Some(header));
    }

    #[test]
    fn a_short_header_is_none() {
        assert_eq!(Header::decode(&C_HELLO_WORLD[..HEADER_SIZE - 1]), None);
    }

    #[test]
    fn tlv_off_does_not_wrap() {
        let mut header = Header::new(u32::MAX - 0x200, Version::default());
        assert_eq!(header.tlv_off(), Some(u32::MAX));
        header.img_size += 1;
        assert_eq!(header.tlv_off(), None);
    }
}
