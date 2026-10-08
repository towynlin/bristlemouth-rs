//! `.dfu.bin` and `.unified.bin`: bm_protocol `src/CMakeLists.txt:570-590`
//! and `:683-684`.

use bm_mcuboot::image::BM_HDR_SIZE;
use bm_mcuboot::{Header, Trailer, Version, tlv};
use sha2::{Digest, Sha256};

use crate::{Error, Key, VersionInfo, elf};

/// Where the bootloader starts.
pub const BOOTLOADER_BASE: u32 = 0x0800_0000;
/// The bootloader's area; slot 1 starts after it.
pub const BOOTLOADER_SIZE: usize = 0xC000;
/// Where an image's body is linked: slot 1 plus the header.
pub const BODY_BASE: u32 = BOOTLOADER_BASE + BOOTLOADER_SIZE as u32 + BM_HDR_SIZE as u32;

/// `imgtool`'s `DEFAULT_MAX_SECTORS`, which bm_protocol's build does not
/// override.
const IMGTOOL_MAX_SECTORS: u32 = 128;

/// The longest header, body and TLV area `imgtool` accepts with
/// bm_protocol's arguments, `0xF07B0`: the slot less `image.py`'s
/// `_trailer_size`. The bootloader's own trailer is smaller
/// (`docs/history/mcuboot-todo.md`, "Limits of the slot's contents").
pub const MAX_IMAGE_LEN: usize = match Trailer::BM.status_off(IMGTOOL_MAX_SECTORS) {
    Some(off) => off as usize,
    None => panic!("the slot holds imgtool's trailer"),
};

const HDR_SIZE: usize = BM_HDR_SIZE as usize;

/// `imgtool sign --header-size 0x200 --align 16 --slot-size 0xF2000
/// --version <version> --pad-header [--key <key>]` of `body`: the header,
/// `0xFF` to `0x200`, `body`, and the TLV area.
pub fn build(body: &[u8], version: Version, key: Option<&Key>) -> Result<Vec<u8>, Error> {
    let tlv_size = match key {
        Some(_) => tlv::ED25519_SIZE,
        None => tlv::UNSIGNED_SIZE,
    };
    let len = HDR_SIZE + body.len() + tlv_size;
    if len > MAX_IMAGE_LEN {
        return Err(Error::TooLong {
            len: len as u64,
            limit: MAX_IMAGE_LEN as u64,
        });
    }
    let mut image = Vec::with_capacity(len);
    image.extend_from_slice(&Header::new(body.len() as u32, version).encode());
    image.resize(HDR_SIZE, 0xFF);
    image.extend_from_slice(body);
    let digest: [u8; 32] = Sha256::digest(&image).into();
    match key {
        Some(key) => image.extend_from_slice(&tlv::encode_ed25519(
            &digest,
            &key.keyhash(),
            &key.sign(&digest),
        )),
        None => image.extend_from_slice(&tlv::encode_unsigned(&digest)),
    }
    Ok(image)
}

/// [`build`] with the version `body`'s own `versionInfo_t` gives, so that
/// the header and the running image report the same one.
pub fn from_body(body: &[u8], key: Option<&Key>) -> Result<Vec<u8>, Error> {
    let (_, info) = VersionInfo::find(body).ok_or(Error::NoVersion)?;
    build(body, info.mcuboot_version(), key)
}

/// A `.dfu.bin` from an application ELF: its loaded sections from
/// [`BODY_BASE`], gaps `0xFF`, through [`from_body`].
pub fn dfu(elf: &[u8], key: Option<&Key>) -> Result<Vec<u8>, Error> {
    let tlv_size = match key {
        Some(_) => tlv::ED25519_SIZE,
        None => tlv::UNSIGNED_SIZE,
    };
    let overhead = (HDR_SIZE + tlv_size) as u64;
    let limit = MAX_IMAGE_LEN as u64;
    let body = elf::flat(elf, BODY_BASE, 0xFF, limit - overhead).map_err(|e| match e {
        Error::TooLong { len, .. } => Error::TooLong {
            len: len + overhead,
            limit,
        },
        e => e,
    })?;
    from_body(&body, key)
}

/// A `.unified.bin`: `bootloader`, an ELF or a flat binary, padded with
/// `0xFF` to [`BOOTLOADER_SIZE`], then `dfu`. Programmed at
/// [`BOOTLOADER_BASE`].
pub fn unified(bootloader: &[u8], dfu: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = if elf::is_elf(bootloader) {
        elf::flat(bootloader, BOOTLOADER_BASE, 0xFF, BOOTLOADER_SIZE as u64).map_err(
            |e| match e {
                Error::TooLong { len, .. } => Error::BootloaderTooLong { len },
                e => e,
            },
        )?
    } else {
        bootloader.to_vec()
    };
    if out.len() > BOOTLOADER_SIZE {
        return Err(Error::BootloaderTooLong {
            len: out.len() as u64,
        });
    }
    if Header::decode(dfu).is_none_or(|h| h.magic != bm_mcuboot::image::IMAGE_MAGIC) {
        return Err(Error::NotAnImage);
    }
    if dfu.len() > MAX_IMAGE_LEN {
        return Err(Error::TooLong {
            len: dfu.len() as u64,
            limit: MAX_IMAGE_LEN as u64,
        });
    }
    out.resize(BOOTLOADER_SIZE, 0xFF);
    out.extend_from_slice(dfu);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSION: Version = Version {
        major: 0,
        minor: 13,
        revision: 12,
        build_num: 0x62d8_b5d0,
    };

    #[test]
    fn the_limit_is_imgtools() {
        assert_eq!(MAX_IMAGE_LEN, 0xF07B0);
        assert_eq!(BODY_BASE, 0x0800_C200);
    }

    /// The C `hello_world` `.dfu.bin`'s first 32 bytes
    /// (`docs/history/mcuboot-todo.md`, "Limits of the slot's contents").
    #[test]
    fn header_is_the_c_hello_worlds() {
        let body = vec![0; 0x3_dab4];
        let image = build(&body, VERSION, None).unwrap();
        assert_eq!(
            image[..32],
            [
                0x3d, 0xb8, 0xf3, 0x96, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0xb4, 0xda,
                0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0d, 0x0c, 0x00, 0xd0, 0xb5, 0xd8, 0x62,
                0x00, 0x00, 0x00, 0x00
            ]
        );
        assert!(image[32..0x200].iter().all(|&b| b == 0xFF));
        // 512 + image + 40.
        assert_eq!(image.len(), 512 + 0x3_dab4 + 40);
    }

    #[test]
    fn rejects_an_image_past_the_limit() {
        let key = Key::from_seed(&[7; 32]);
        for (key, tlv_size) in [(None, tlv::UNSIGNED_SIZE), (Some(&key), tlv::ED25519_SIZE)] {
            let fits = MAX_IMAGE_LEN - HDR_SIZE - tlv_size;
            assert_eq!(
                build(&vec![0; fits], VERSION, key).unwrap().len(),
                MAX_IMAGE_LEN
            );
            let err = build(&vec![0; fits + 1], VERSION, key).unwrap_err();
            assert_eq!(
                err,
                Error::TooLong {
                    len: 0xF07B1,
                    limit: 0xF07B0
                }
            );
            assert!(err.to_string().contains("the limit is 0xf07b0"));
        }
    }

    #[test]
    fn a_body_without_a_version_is_an_error() {
        assert_eq!(from_body(&[0; 64], None), Err(Error::NoVersion));
    }

    #[test]
    fn unified_pads_a_flat_bootloader() {
        let dfu = build(&[1, 2, 3], VERSION, None).unwrap();
        let out = unified(&[0xAA; 100], &dfu).unwrap();
        assert_eq!(out[..100], [0xAA; 100]);
        assert!(out[100..BOOTLOADER_SIZE].iter().all(|&b| b == 0xFF));
        assert_eq!(out[BOOTLOADER_SIZE..], dfu);

        assert_eq!(
            unified(&vec![0; BOOTLOADER_SIZE], &dfu).unwrap().len(),
            BOOTLOADER_SIZE + dfu.len()
        );
        assert_eq!(
            unified(&vec![0; BOOTLOADER_SIZE + 1], &dfu),
            Err(Error::BootloaderTooLong { len: 0xC001 })
        );
    }

    #[test]
    fn unified_wants_an_image() {
        assert_eq!(unified(&[0; 16], &[0; 64]), Err(Error::NotAnImage));
        assert_eq!(unified(&[0; 16], &[0x3d, 0xb8]), Err(Error::NotAnImage));
        let mut long = build(&[0; 16], VERSION, None).unwrap();
        long.resize(MAX_IMAGE_LEN + 1, 0);
        assert!(matches!(
            unified(&[0; 16], &long),
            Err(Error::TooLong { .. })
        ));
    }
}
