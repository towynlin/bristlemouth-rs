//! What a `.dfu.bin` says about itself.

use std::fmt;

use bm_mcuboot::image::IMAGE_MAGIC;
use bm_mcuboot::{Header, TlvArea, tlv};
use bm_wire::crc::crc16_ccitt;
use sha2::{Digest, Sha256};

use crate::{Error, VersionInfo};

/// A `.dfu.bin`'s header, TLVs and version note, and the `BmDfuImgInfo`
/// fields a DFU host sends for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    /// The file's length: `BmDfuImgInfo::image_size`.
    pub size: u32,
    /// CRC-16/KERMIT of the file: `BmDfuImgInfo::crc16`.
    pub crc16: u16,
    /// The MCUboot header.
    pub header: Header,
    /// Each TLV's type and value, in order.
    pub tlvs: Vec<(u16, Vec<u8>)>,
    /// Whether the SHA-256 TLV is the hash of the header and body.
    pub sha256_ok: bool,
    /// Bytes in the file after the TLV area.
    pub trailing: usize,
    /// The first `versionInfo_t` and its offset in the file.
    pub version: Option<(usize, VersionInfo)>,
}

impl Info {
    /// Read a `.dfu.bin`. The signature is not checked: the file does not
    /// hold the public key.
    pub fn read(file: &[u8]) -> Result<Self, Error> {
        let header = Header::decode(file)
            .filter(|h| h.magic == IMAGE_MAGIC)
            .ok_or(Error::NotAnImage)?;
        let area = TlvArea::parse(file, &header).map_err(Error::Tlv)?;
        let tlvs = area
            .iter()
            .map(|t| t.map(|t| (t.kind, t.value.to_vec())))
            .collect::<Result<Vec<_>, _>>()
            .map_err(Error::Tlv)?;
        let digest = Sha256::digest(&file[..area.start()]);
        let sha256_ok = area.find(tlv::SHA256) == Some(digest.as_slice());
        Ok(Self {
            size: u32::try_from(file.len()).map_err(|_| Error::NotAnImage)?,
            crc16: crc16_ccitt(0, file),
            header,
            tlvs,
            sha256_ok,
            trailing: file.len() - area.end(),
            version: VersionInfo::find(file),
        })
    }

    /// The `KEYHASH` TLV, if the image is ed25519-signed: it has that and
    /// an `ED25519` TLV.
    pub fn signed_by(&self) -> Option<&[u8]> {
        let find = |kind| self.tlvs.iter().find(|(k, _)| *k == kind);
        find(tlv::ED25519)?;
        find(tlv::KEYHASH).map(|(_, v)| v.as_slice())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl fmt::Display for Info {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = &self.header;
        let v = &h.version;
        writeln!(f, "size:     {} ({:#x})", self.size, self.size)?;
        writeln!(f, "crc16:    {:#06x}", self.crc16)?;
        writeln!(
            f,
            "header:   magic {:#010x}, load_addr {:#x}, hdr_size {:#x}, protect_tlv_size {}, \
             img_size {} ({:#x}), flags {:#x}",
            h.magic, h.load_addr, h.hdr_size, h.protect_tlv_size, h.img_size, h.img_size, h.flags
        )?;
        writeln!(
            f,
            "ih_ver:   {}.{}.{}+{} (build {:#010x})",
            v.major, v.minor, v.revision, v.build_num, v.build_num
        )?;
        for (kind, value) in &self.tlvs {
            let name = match *kind {
                tlv::SHA256 => "SHA256",
                tlv::KEYHASH => "KEYHASH",
                tlv::ED25519 => "ED25519",
                _ => "",
            };
            let check = match (*kind, self.sha256_ok) {
                (tlv::SHA256, true) => " (matches)",
                (tlv::SHA256, false) => " (DOES NOT MATCH)",
                _ => "",
            };
            writeln!(f, "tlv:      {kind:#04x} {name} {}{check}", hex(value))?;
        }
        match self.signed_by() {
            Some(keyhash) => writeln!(f, "signed:   ed25519, key hash {}", hex(keyhash))?,
            None => writeln!(f, "signed:   no")?,
        }
        if self.trailing != 0 {
            writeln!(f, "trailing: {} bytes after the TLV area", self.trailing)?;
        }
        match &self.version {
            Some((at, n)) => {
                writeln!(
                    f,
                    "note:     at {at:#x}: gitSHA {:#010x}, {}.{}.{}, hwVersion {}, flags {:#x}, \
                     {:?}",
                    n.git_sha, n.major, n.minor, n.revision, n.hw_version, n.flags, n.version_str
                )?;
                writeln!(
                    f,
                    "BmDfuImgInfo: image_size {}, crc16 {:#06x}, major_ver {}, minor_ver {}, \
                     gitSHA {:#010x}",
                    self.size, self.crc16, n.major, n.minor, n.git_sha
                )
            }
            None => writeln!(f, "note:     none"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_what_is_not_an_image() {
        assert_eq!(Info::read(&[]), Err(Error::NotAnImage));
        assert_eq!(Info::read(&[0; 64]), Err(Error::NotAnImage));
        let header = Header::new(4, bm_mcuboot::Version::default()).encode();
        assert_eq!(
            Info::read(&header),
            Err(Error::Tlv(bm_mcuboot::TlvError::Truncated))
        );
    }

    /// CRC-16/KERMIT's check value.
    #[test]
    fn crc_is_kermit() {
        assert_eq!(crc16_ccitt(0, b"123456789"), 0x2189);
    }
}
