//! This image's version, in the three places that must agree for a DFU to be
//! confirmed (`docs/mcuboot-todo.md`, contract 8): [`crate::DevkitIdentity`]
//! reports these constants, [`NOTE`] carries them in the image, and
//! `bm-image` copies them from the note into the MCUboot header.

/// The first 8 hex digits of the commit built, as bm_protocol reports its
/// own; 0 outside a git checkout.
pub const GIT_SHA: u32 = match u32::from_str_radix(env!("BM_DEVKIT_GIT_SHA"), 16) {
    Ok(sha) => sha,
    Err(_) => 0,
};
/// This crate's major version.
pub const MAJOR: u8 = part(env!("CARGO_PKG_VERSION_MAJOR"));
/// This crate's minor version.
pub const MINOR: u8 = part(env!("CARGO_PKG_VERSION_MINOR"));
/// This crate's patch version.
pub const REVISION: u8 = part(env!("CARGO_PKG_VERSION_PATCH"));
/// `bm-devkit@v<version>+<sha>`, after the C's `<app>@<describe>+<sha>`.
pub const VERSION_STRING: &str = concat!(
    "bm-devkit@v",
    env!("CARGO_PKG_VERSION"),
    "+",
    env!("BM_DEVKIT_GIT_SHA")
);

/// A Cargo version component as a `u8`, saturating.
const fn part(part: &str) -> u8 {
    match u8::from_str_radix(part, 10) {
        Ok(n) => n,
        Err(_) => u8::MAX,
    }
}

/// `VERSION_MAGIC`.
pub const MAGIC: u64 = 0xDF7F_9AFD_EC06_627C;
/// `MAX_VERSION_STR_LEN`.
pub const MAX_VERSION_STR_LEN: usize = 96;
/// `VER_ENG_FLAG_OFFSET`'s bit: not a release build.
pub const FLAG_ENG: u32 = 1 << 0;
/// `sizeof(versionInfo_t)`, packed.
pub const INFO_LEN: usize = 22 + MAX_VERSION_STR_LEN;
/// `sizeof(versionNote_t)`: the ELF note header, the name, the info.
pub const NOTE_LEN: usize = 12 + 8 + INFO_LEN;

/// bm_protocol's `versionNote` (`src/lib/common/version.h`, filled by
/// `cmake/git_version.cmake`): an ELF note named `VERSION`, type `0x10`,
/// holding a packed little-endian `versionInfo_t`.
///
/// | Field | Value |
/// |---|---|
/// | `gitSHA`, `maj`, `min`, `rev` | [`GIT_SHA`], [`MAJOR`], [`MINOR`], [`REVISION`] |
/// | `hwVersion` | 0, as `DeviceInfo::hw_ver` |
/// | `flags` | [`FLAG_ENG`]; the dirty flag is not computed |
/// | `versionStr` | [`VERSION_STRING`], cut to 96 bytes |
///
/// `devkit.x` places it at `0x0800C438`, where the C image has its own, and
/// fails the link if it is absent.
#[used]
#[unsafe(link_section = ".note.sofar.version")]
pub static NOTE: [u8; NOTE_LEN] = note(
    GIT_SHA,
    MAJOR,
    MINOR,
    REVISION,
    FLAG_ENG,
    VERSION_STRING.as_bytes(),
);

const fn put(mut out: [u8; NOTE_LEN], at: usize, bytes: &[u8]) -> [u8; NOTE_LEN] {
    let mut i = 0;
    while i < bytes.len() {
        out[at + i] = bytes[i];
        i += 1;
    }
    out
}

const fn note(
    git_sha: u32,
    major: u8,
    minor: u8,
    revision: u8,
    flags: u32,
    version: &[u8],
) -> [u8; NOTE_LEN] {
    let (version, _) = if version.len() > MAX_VERSION_STR_LEN {
        version.split_at(MAX_VERSION_STR_LEN)
    } else {
        (version, version)
    };
    let mut out = [0u8; NOTE_LEN];
    // ElfNoteSection_t: namesz, descsz, type.
    out = put(out, 0, &8u32.to_le_bytes());
    out = put(out, 4, &(INFO_LEN as u32).to_le_bytes());
    out = put(out, 8, &0x10u32.to_le_bytes());
    out = put(out, 12, b"VERSION\0");
    // versionInfo_t.
    out = put(out, 20, &MAGIC.to_le_bytes());
    out = put(out, 28, &git_sha.to_le_bytes());
    // maj, min, rev, hwVersion.
    out = put(out, 32, &[major, minor, revision, 0]);
    out = put(out, 36, &flags.to_le_bytes());
    out = put(out, 40, &(version.len() as u16).to_le_bytes());
    put(out, 42, version)
}

// `bm-image/testdata/app.s`'s note, which is the C `hello_world`'s at
// bm_protocol `62d8b5d0`. This crate has no host target to run a test on.
const _: () = {
    let note = note(0x62d8_b5d0, 0, 13, 12, FLAG_ENG, b"v0.13.12");
    let head: [u8; 50] = [
        8, 0, 0, 0, 118, 0, 0, 0, 0x10, 0, 0, 0, b'V', b'E', b'R', b'S', b'I', b'O', b'N', 0, 0x7C,
        0x62, 0x06, 0xEC, 0xFD, 0x9A, 0x7F, 0xDF, 0xD0, 0xB5, 0xD8, 0x62, 0, 13, 12, 0, 1, 0, 0, 0,
        8, 0, b'v', b'0', b'.', b'1', b'3', b'.', b'1', b'2',
    ];
    let mut i = 0;
    while i < NOTE_LEN {
        let want = if i < head.len() { head[i] } else { 0 };
        assert!(note[i] == want);
        i += 1;
    }
};
