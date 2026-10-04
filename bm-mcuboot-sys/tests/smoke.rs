//! `boot_go`, `boot_set_pending` and `boot_set_confirmed` on both builds,
//! with images built by hand here and one signed by `imgtool`.

use bm_mcuboot_sys::{Area, Booted, Build, HEADER_SIZE, Oracle, Refusal, lock, sha256, swap_type};

/// `imgtool sign --header-size`.
const HDR_SIZE: usize = 0x200;

/// `testdata/body.bin` signed with `testdata/test_ed25519_key.pem`; the
/// command is in `README.md`.
const SIGNED: &[u8] = include_bytes!("../testdata/body.signed.dfu.bin");

/// Contract 2's unsigned image: a `struct image_header`, `0xFF` to
/// `HDR_SIZE`, `body`, and a TLV area holding the SHA-256 of all of that.
fn image(body: &[u8], major: u8) -> Vec<u8> {
    let mut image = Vec::new();
    image.extend_from_slice(&0x96f3_b83d_u32.to_le_bytes()); // ih_magic
    image.extend_from_slice(&0_u32.to_le_bytes()); // ih_load_addr
    image.extend_from_slice(&(HDR_SIZE as u16).to_le_bytes()); // ih_hdr_size
    image.extend_from_slice(&0_u16.to_le_bytes()); // ih_protect_tlv_size
    image.extend_from_slice(&(body.len() as u32).to_le_bytes()); // ih_img_size
    image.extend_from_slice(&0_u32.to_le_bytes()); // ih_flags
    image.extend_from_slice(&[major, 0, 0, 0, 0, 0, 0, 0]); // ih_ver
    image.extend_from_slice(&0_u32.to_le_bytes()); // _pad1
    assert_eq!(image.len(), HEADER_SIZE);
    image.resize(HDR_SIZE, 0xFF);
    image.extend_from_slice(body);

    let digest = sha256(&image);
    image.extend_from_slice(&0x6907_u16.to_le_bytes()); // IMAGE_TLV_INFO_MAGIC
    image.extend_from_slice(&40_u16.to_le_bytes()); // it_tlv_tot
    image.extend_from_slice(&0x10_u16.to_le_bytes()); // IMAGE_TLV_SHA256
    image.extend_from_slice(&32_u16.to_le_bytes());
    image.extend_from_slice(&digest);
    image
}

fn fresh(build: Build) -> Oracle {
    let mut oracle = lock(build);
    oracle.reset();
    oracle
}

fn header(image: &[u8]) -> [u8; HEADER_SIZE] {
    image[..HEADER_SIZE].try_into().unwrap()
}

/// The first `image.len()` bytes of `area`.
fn holds(oracle: &Oracle, area: Area, image: &[u8]) -> bool {
    let mut buf = vec![0; image.len()];
    oracle.read(area, 0, &mut buf);
    buf == image
}

fn booted(image: &[u8]) -> Result<Booted, Refusal> {
    Ok(Booted {
        image_off: Area::Primary.offset(),
        flash_dev_id: 0,
        header: header(image),
    })
}

/// `boot_go` when slot 1 fails `boot_validate_slot`.
const NO_IMAGE: Result<Booted, Refusal> = Err(Refusal::Code(1));

#[test]
fn sha256_is_sha256() {
    // FIPS 180-2's "abc".
    assert_eq!(
        sha256(b"abc"),
        [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad
        ]
    );
}

#[test]
fn the_map_is_contract_1() {
    assert_eq!(Area::Primary.offset(), 0xC000);
    assert_eq!(Area::Secondary.offset(), 0xFE000);
    assert_eq!(Area::Scratch.offset(), 0x1F0000);
    assert_eq!(Area::Scratch.offset() + Area::Scratch.size(), 0x200000);

    let mut oracle = fresh(Build::Unsigned);
    for area in [
        Area::Bootloader,
        Area::Primary,
        Area::Secondary,
        Area::Scratch,
    ] {
        assert!(oracle.read_area(area).iter().all(|&b| b == 0xFF));
    }
    // Areas are adjacent: the last byte of one is not the first of the next.
    oracle.write(Area::Primary, Area::Primary.size() - 1, &[0x5A]);
    let mut first = [0];
    oracle.read(Area::Secondary, 0, &mut first);
    assert_eq!(first, [0xFF]);
}

#[test]
fn empty_flash_boots_nothing() {
    for build in [Build::Unsigned, Build::Ed25519] {
        let mut oracle = fresh(build);
        assert_eq!(oracle.boot_go(), NO_IMAGE, "{build:?}");
    }
}

#[test]
fn an_image_in_slot_1_boots() {
    let a = image(b"image a", 1);
    let mut oracle = fresh(Build::Unsigned);
    oracle.write(Area::Primary, 0, &a);

    assert_eq!(oracle.swap_type(), Ok(swap_type::NONE));
    assert_eq!(oracle.boot_go(), booted(&a));
    // MCUBOOT_VALIDATE_PRIMARY_SLOT: a flipped body byte is refused.
    oracle.write(Area::Primary, HDR_SIZE as u32, b"I");
    assert_eq!(oracle.boot_go(), NO_IMAGE);
}

#[test]
fn a_pending_image_swaps_and_reverts_unless_confirmed() {
    let a = image(b"image a", 1);
    let b = image(b"image b, which is longer", 2);
    let mut oracle = fresh(Build::Unsigned);
    oracle.write(Area::Primary, 0, &a);
    oracle.write(Area::Secondary, 0, &b);

    // Not pending: slot 2 is left alone.
    assert_eq!(oracle.boot_go(), booted(&a));
    assert!(holds(&oracle, Area::Secondary, &b));

    assert_eq!(oracle.set_pending(false), Ok(()));
    assert_eq!(oracle.swap_type(), Ok(swap_type::TEST));
    assert_eq!(oracle.boot_go(), booted(&b));
    assert!(holds(&oracle, Area::Primary, &b));
    assert!(holds(&oracle, Area::Secondary, &a));

    // No boot_set_confirmed: the next boot swaps back, and stays there.
    assert_eq!(oracle.swap_type(), Ok(swap_type::REVERT));
    assert_eq!(oracle.boot_go(), booted(&a));
    assert!(holds(&oracle, Area::Primary, &a));
    assert!(holds(&oracle, Area::Secondary, &b));
    assert_eq!(oracle.swap_type(), Ok(swap_type::NONE));
    assert_eq!(oracle.boot_go(), booted(&a));
}

#[test]
fn a_confirmed_image_stays() {
    let a = image(b"image a", 1);
    let b = image(b"image b, which is longer", 2);
    let mut oracle = fresh(Build::Unsigned);
    oracle.write(Area::Primary, 0, &a);
    oracle.write(Area::Secondary, 0, &b);

    assert_eq!(oracle.set_pending(false), Ok(()));
    assert_eq!(oracle.boot_go(), booted(&b));
    assert_eq!(oracle.set_confirmed(), Ok(()));
    assert_eq!(oracle.swap_type(), Ok(swap_type::NONE));
    assert_eq!(oracle.boot_go(), booted(&b));
    assert!(holds(&oracle, Area::Primary, &b));
    assert!(holds(&oracle, Area::Secondary, &a));
}

#[test]
fn the_builds_have_separate_flash() {
    let a = image(b"image a", 1);
    // One lock throughout: another test's reset must not come between.
    let mut unsigned = fresh(Build::Unsigned);
    unsigned.write(Area::Primary, 0, &a);
    let mut signing = unsigned.switch(Build::Ed25519);
    signing.reset();
    assert!(!holds(&signing, Area::Primary, &a));
    assert!(holds(&signing.switch(Build::Unsigned), Area::Primary, &a));
}

#[test]
fn the_signing_build_refuses_an_unsigned_image() {
    let a = image(b"image a", 1);
    let mut oracle = fresh(Build::Ed25519);
    oracle.write(Area::Primary, 0, &a);
    assert_eq!(oracle.boot_go(), NO_IMAGE);
}

#[test]
fn both_builds_boot_a_signed_image() {
    for build in [Build::Unsigned, Build::Ed25519] {
        let mut oracle = fresh(build);
        oracle.write(Area::Primary, 0, SIGNED);
        assert_eq!(oracle.boot_go(), booted(SIGNED), "{build:?}");
    }
}

#[test]
fn the_signing_build_refuses_a_changed_signature() {
    // The image hash still matches; only the last signature byte differs.
    let mut changed = SIGNED.to_vec();
    *changed.last_mut().unwrap() ^= 1;

    let mut signing = fresh(Build::Ed25519);
    signing.write(Area::Primary, 0, &changed);
    assert_eq!(signing.boot_go(), NO_IMAGE);
    drop(signing);

    let mut unsigned = fresh(Build::Unsigned);
    unsigned.write(Area::Primary, 0, &changed);
    assert_eq!(unsigned.boot_go(), booted(&changed));
}

#[test]
fn the_signing_build_erases_an_unsigned_update() {
    let b = image(b"image b", 2);
    let mut oracle = fresh(Build::Ed25519);
    oracle.write(Area::Primary, 0, SIGNED);
    oracle.write(Area::Secondary, 0, &b);

    assert_eq!(oracle.set_pending(false), Ok(()));
    assert_eq!(oracle.boot_go(), booted(SIGNED));
    assert!(holds(&oracle, Area::Primary, SIGNED));
    // boot_validate_slot erases a secondary slot it refuses.
    assert!(oracle.read_area(Area::Secondary).iter().all(|&b| b == 0xFF));
}
