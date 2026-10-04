//! bm_protocol's `versionInfo_t` (`src/lib/common/version.h`), packed and
//! little-endian, as tools find it in a binary: by its magic
//! (`tools/scripts/util/fwinfo.py`).

use bm_mcuboot::Version;

/// `VERSION_MAGIC`.
pub const MAGIC: u64 = 0xDF7F_9AFD_EC06_627C;
/// The fields before `versionStr`.
pub const FIXED_LEN: usize = 22;
/// `MAX_VERSION_STR_LEN`.
pub const MAX_VERSION_STR_LEN: usize = 96;

/// `versionInfo_t` without its magic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionInfo {
    /// `gitSHA`.
    pub git_sha: u32,
    /// `maj`.
    pub major: u8,
    /// `min`.
    pub minor: u8,
    /// `rev`.
    pub revision: u8,
    /// `hwVersion`.
    pub hw_version: u8,
    /// `flags`.
    pub flags: u32,
    /// `versionStr`, its first `versionStrLen` bytes, at most
    /// [`MAX_VERSION_STR_LEN`] and no more than `bytes` holds.
    pub version_str: String,
}

impl VersionInfo {
    /// The first `versionInfo_t` in `bytes` and its offset. `fwinfo.py`
    /// takes the first match too.
    pub fn find(bytes: &[u8]) -> Option<(usize, Self)> {
        let magic = MAGIC.to_le_bytes();
        let at = bytes
            .windows(FIXED_LEN)
            .position(|w| w.starts_with(&magic))?;
        let b = &bytes[at..];
        let len = usize::from(u16::from_le_bytes([b[20], b[21]])).min(MAX_VERSION_STR_LEN);
        let text = &b[FIXED_LEN..];
        let text = &text[..len.min(text.len())];
        Some((
            at,
            Self {
                git_sha: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
                major: b[12],
                minor: b[13],
                revision: b[14],
                hw_version: b[15],
                flags: u32::from_le_bytes([b[16], b[17], b[18], b[19]]),
                version_str: String::from_utf8_lossy(text).into_owned(),
            },
        ))
    }

    /// `ih_ver` for this image: `<maj>.<min>.<rev>+<gitSHA>`
    /// (`cmake/git_version.cmake`, `MCUBOOT_VERSION_STR`).
    ///
    /// A build whose tag is not `vX.Y.Z` has `0xFF` in all three fields and
    /// `0.0.0` in its header; so here.
    pub fn mcuboot_version(&self) -> Version {
        let (major, minor, revision) = match (self.major, self.minor, self.revision) {
            (0xFF, 0xFF, 0xFF) => (0, 0, 0),
            v => v,
        };
        Version {
            major,
            minor,
            revision: u16::from(revision),
            build_num: self.git_sha,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(maj: u8, min: u8, rev: u8, text: &[u8], text_len: u16) -> Vec<u8> {
        let mut b = MAGIC.to_le_bytes().to_vec();
        b.extend_from_slice(&0x62d8_b5d0_u32.to_le_bytes());
        b.extend_from_slice(&[maj, min, rev, 12]);
        b.extend_from_slice(&3_u32.to_le_bytes());
        b.extend_from_slice(&text_len.to_le_bytes());
        b.extend_from_slice(text);
        b
    }

    #[test]
    fn finds_the_first_magic() {
        let mut bytes = vec![0x7C, 0x62, 0x06];
        bytes.extend(note(0, 13, 12, b"v0.13.12\0\0", 8));
        bytes.extend(note(9, 9, 9, b"second", 6));
        let (at, info) = VersionInfo::find(&bytes).unwrap();
        assert_eq!(at, 3);
        assert_eq!(
            info,
            VersionInfo {
                git_sha: 0x62d8_b5d0,
                major: 0,
                minor: 13,
                revision: 12,
                hw_version: 12,
                flags: 3,
                version_str: "v0.13.12".into(),
            }
        );
        assert_eq!(
            info.mcuboot_version(),
            Version {
                major: 0,
                minor: 13,
                revision: 12,
                build_num: 0x62d8_b5d0
            }
        );
    }

    #[test]
    fn needs_the_fixed_fields() {
        let bytes = note(1, 2, 3, b"", 0);
        assert!(VersionInfo::find(&bytes).is_some());
        assert_eq!(VersionInfo::find(&bytes[..FIXED_LEN - 1]), None);
        assert_eq!(VersionInfo::find(&[0; 64]), None);
    }

    #[test]
    fn version_str_is_bounded() {
        // A length past the array, and past the bytes.
        let (_, info) = VersionInfo::find(&note(1, 2, 3, &[b'a'; 200], 500)).unwrap();
        assert_eq!(info.version_str.len(), MAX_VERSION_STR_LEN);
        let (_, info) = VersionInfo::find(&note(1, 2, 3, b"abc", 8)).unwrap();
        assert_eq!(info.version_str, "abc");
    }

    #[test]
    fn an_untagged_build_is_0_0_0() {
        let (_, info) = VersionInfo::find(&note(0xFF, 0xFF, 0xFF, b"62d8b5d0", 8)).unwrap();
        assert_eq!(
            info.mcuboot_version(),
            Version {
                major: 0,
                minor: 0,
                revision: 0,
                build_num: 0x62d8_b5d0
            }
        );
    }
}
