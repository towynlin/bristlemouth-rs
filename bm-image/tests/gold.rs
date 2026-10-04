//! `bm-image`'s files against `imgtool`'s and `objcopy`'s, byte for byte.
//! `README.md`, "Test data", has the commands that made them.

use bm_image::{BODY_BASE, Info, Key, VersionInfo, build, dfu, elf, from_body, unified};
use bm_mcuboot::{Header, Version, tlv};

const ELF: &[u8] = include_bytes!("../testdata/app.elf");
const BIN: &[u8] = include_bytes!("../testdata/app.bin");
const DFU: &[u8] = include_bytes!("../testdata/app.dfu.bin");
const SIGNED: &[u8] = include_bytes!("../testdata/app.signed.dfu.bin");
/// O1's test key.
const KEY: &str = include_str!("../../bm-mcuboot-sys/testdata/test_ed25519_key.pem");

/// `--version 0.13.12+1658369488`, and `app.s`'s note.
const VERSION: Version = Version {
    major: 0,
    minor: 13,
    revision: 12,
    build_num: 0x62d8_b5d0,
};

fn key() -> Key {
    Key::from_pem(KEY).unwrap()
}

#[test]
fn the_elfs_binary_is_objcopys() {
    assert_eq!(elf::flat(ELF, BODY_BASE, 0xFF, 0x1000).unwrap(), BIN);
}

#[test]
fn the_note_is_at_the_c_builds_offset() {
    let (at, info) = VersionInfo::find(DFU).unwrap();
    assert_eq!(at, 0x438 + 20);
    assert_eq!(info.mcuboot_version(), VERSION);
    assert_eq!(info.version_str, "v0.13.12");
}

#[test]
fn unsigned_is_imgtools() {
    assert_eq!(build(BIN, VERSION, None).unwrap(), DFU);
    assert_eq!(from_body(BIN, None).unwrap(), DFU);
    assert_eq!(dfu(ELF, None).unwrap(), DFU);
}

#[test]
fn signed_is_imgtools() {
    let key = key();
    assert_eq!(build(BIN, VERSION, Some(&key)).unwrap(), SIGNED);
    assert_eq!(dfu(ELF, Some(&key)).unwrap(), SIGNED);
}

/// The public key `bm-mcuboot-sys/csrc/test_ed25519_pub_key.c` embeds is
/// the one this key signs with, and its hash is `imgtool`'s `KEYHASH`.
#[test]
fn keyhash_is_imgtools() {
    let info = Info::read(SIGNED).unwrap();
    assert_eq!(info.signed_by(), Some(&key().keyhash()[..]));
    // Also in O1's own signed example.
    let o1 = include_bytes!("../../bm-mcuboot-sys/testdata/body.signed.dfu.bin");
    assert_eq!(
        Info::read(o1).unwrap().signed_by(),
        Some(&key().keyhash()[..])
    );
}

#[test]
fn info_reads_imgtools_files() {
    for (file, kinds) in [
        (DFU, &[tlv::SHA256][..]),
        (SIGNED, &[tlv::SHA256, tlv::KEYHASH, tlv::ED25519][..]),
    ] {
        let info = Info::read(file).unwrap();
        assert_eq!(info.size as usize, file.len());
        assert_eq!(info.header, Header::new(BIN.len() as u32, VERSION));
        assert_eq!(info.tlvs.iter().map(|(k, _)| *k).collect::<Vec<_>>(), kinds);
        assert!(info.sha256_ok);
        assert_eq!(info.trailing, 0);
        assert_eq!(info.signed_by().is_some(), kinds.len() == 3);
        assert_eq!(info.version.as_ref().unwrap().0, 0x438 + 20);
        assert_eq!(info.crc16, bm_wire::crc::crc16_ccitt(0, file));
    }
}

#[test]
fn info_reports_a_changed_body_and_trailing_bytes() {
    let mut file = DFU.to_vec();
    file[0x200] ^= 1;
    file.extend_from_slice(&[0xFF; 3]);
    let info = Info::read(&file).unwrap();
    assert!(!info.sha256_ok);
    assert_eq!(info.trailing, 3);
    assert!(info.to_string().contains("DOES NOT MATCH"));
}

#[test]
fn info_prints_bm_dfu_img_infos_fields() {
    let text = Info::read(SIGNED).unwrap().to_string();
    let crc = bm_wire::crc::crc16_ccitt(0, SIGNED);
    for want in [
        "size:     1387 (0x56b)".to_string(),
        "ih_ver:   0.13.12+1658369488 (build 0x62d8b5d0)".to_string(),
        "signed:   ed25519, key hash 5e8bdbe69d4929feb46bd5ac5a253800110fdff640085dd1a3964a86e48efcfe"
            .to_string(),
        format!(
            "BmDfuImgInfo: image_size 1387, crc16 {crc:#06x}, major_ver 0, minor_ver 13, \
             gitSHA 0x62d8b5d0"
        ),
    ] {
        assert!(text.contains(&want), "{want:?} not in:\n{text}");
    }
    assert!(
        Info::read(DFU)
            .unwrap()
            .to_string()
            .contains("signed:   no")
    );
}

/// An ELF bootloader is laid out from `0x08000000`; `app.elf` is not one.
#[test]
fn unified_wants_a_bootloader_at_its_base() {
    assert_eq!(
        unified(ELF, DFU),
        Err(bm_image::Error::NothingAt(bm_image::BOOTLOADER_BASE))
    );
}

/// The CLI writes the same files and `info` succeeds on them.
#[test]
fn cli() {
    use std::process::Command;
    let exe = env!("CARGO_BIN_EXE_bm-image");
    let data = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata");
    let key = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../bm-mcuboot-sys/testdata/test_ed25519_key.pem"
    );
    let tmp = std::env::temp_dir().join(format!("bm-image-cli-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = |name: &str| tmp.join(name).to_str().unwrap().to_string();
    let run = |args: &[&str]| Command::new(exe).args(args).output().unwrap();

    let elf = format!("{data}/app.elf");
    assert!(
        run(&["dfu", &elf, "-o", &path("u.dfu.bin")])
            .status
            .success()
    );
    assert_eq!(std::fs::read(path("u.dfu.bin")).unwrap(), DFU);
    assert!(
        run(&["dfu", &elf, "--key", key, "-o", &path("s.dfu.bin")])
            .status
            .success()
    );
    assert_eq!(std::fs::read(path("s.dfu.bin")).unwrap(), SIGNED);

    std::fs::write(path("boot.bin"), [0x55; 16]).unwrap();
    let out = run(&[
        "unified",
        &path("boot.bin"),
        &path("u.dfu.bin"),
        "-o",
        &path("u.unified.bin"),
    ]);
    assert!(out.status.success());
    assert_eq!(
        std::fs::read(path("u.unified.bin")).unwrap(),
        unified(&[0x55; 16], DFU).unwrap()
    );

    let out = run(&["info", &path("s.dfu.bin")]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        Info::read(SIGNED).unwrap().to_string()
    );

    // Failures exit non-zero with a message and write nothing.
    let out = run(&["dfu", &path("boot.bin"), "-o", &path("bad.dfu.bin")]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("no ELF magic")
    );
    assert!(!tmp.join("bad.dfu.bin").exists());
    let out = run(&["dfu", &elf]);
    assert!(!out.status.success());
    assert!(String::from_utf8(out.stderr).unwrap().starts_with("usage:"));

    std::fs::remove_dir_all(&tmp).unwrap();
}
