//! `bm-image`'s files on MCUboot's `bootutil` (`bm-mcuboot-sys`): what
//! `boot_go` boots and what it refuses, on both builds.

use bm_image::{Key, build, dfu};
use bm_mcuboot::image::HEADER_SIZE;
use bm_mcuboot::{Trailer, Version, set_pending};
use bm_mcuboot_diff::RamSlot;
use bm_mcuboot_sys::{Area, Build, Oracle, Refusal, lock, swap_type};

const ELF: &[u8] = include_bytes!("../testdata/app.elf");
const BIN: &[u8] = include_bytes!("../testdata/app.bin");
/// The key the signing build trusts.
const KEY: &str = include_str!("../../bm-mcuboot-sys/testdata/test_ed25519_key.pem");

const OLD: Version = Version {
    major: 0,
    minor: 13,
    revision: 11,
    build_num: 0x0bad_f00d,
};

fn key() -> Key {
    Key::from_pem(KEY).unwrap()
}

/// The image already in slot 1, which each build accepts.
fn old(build_kind: Build) -> Vec<u8> {
    let key = key();
    let key = match build_kind {
        Build::Unsigned => None,
        Build::Ed25519 => Some(&key),
    };
    build(b"the image before the update", OLD, key).unwrap()
}

/// Put `image` in slot 2 and mark it with `bm_mcuboot::set_pending`.
fn stage(oracle: &mut Oracle, image: &[u8]) {
    oracle.write(Area::Secondary, 0, image);
    let mut slot = RamSlot::of(oracle, Area::Secondary);
    assert_eq!(set_pending(&mut slot, &Trailer::BM, false), Ok(()));
    oracle.write(Area::Secondary, 0, &slot.bytes);
    assert_eq!(oracle.swap_type(), Ok(swap_type::TEST));
}

/// `image` boots from slot 1, and from slot 2 after a Rust mark, where the
/// swap leaves the old image in slot 2.
fn boots(build_kind: Build, image: &[u8]) {
    let mut oracle = lock(build_kind);
    oracle.reset();
    oracle.write(Area::Primary, 0, image);
    let booted = oracle.boot_go().unwrap();
    assert_eq!(booted.image_off, Area::Primary.offset());
    assert_eq!(booted.header, image[..HEADER_SIZE]);

    let old = old(build_kind);
    oracle.reset();
    oracle.write(Area::Primary, 0, &old);
    stage(&mut oracle, image);
    assert_eq!(oracle.boot_go().unwrap().header, image[..HEADER_SIZE]);
    assert_eq!(oracle.read_area(Area::Primary)[..image.len()], *image);
    assert_eq!(oracle.read_area(Area::Secondary)[..old.len()], *old);
    assert_eq!(oracle.swap_type(), Ok(swap_type::REVERT));
}

/// `image` does not boot from slot 1; staged in slot 2 it is erased and the
/// old image boots, with no swap left to do.
fn refused(build_kind: Build, image: &[u8]) {
    let mut oracle = lock(build_kind);
    oracle.reset();
    oracle.write(Area::Primary, 0, image);
    assert_eq!(oracle.boot_go(), Err(Refusal::Code(1)));

    let old = old(build_kind);
    oracle.reset();
    oracle.write(Area::Primary, 0, &old);
    stage(&mut oracle, image);
    assert_eq!(oracle.boot_go().unwrap().header, old[..HEADER_SIZE]);
    assert_eq!(oracle.read_area(Area::Primary)[..old.len()], *old);
    assert!(oracle.read_area(Area::Secondary).iter().all(|&b| b == 0xFF));
    assert_eq!(oracle.swap_type(), Ok(swap_type::NONE));
}

#[test]
fn unsigned_boots_on_the_unsigned_build() {
    boots(Build::Unsigned, &dfu(ELF, None).unwrap());
}

#[test]
fn signed_boots_on_both_builds() {
    let image = dfu(ELF, Some(&key())).unwrap();
    boots(Build::Unsigned, &image);
    boots(Build::Ed25519, &image);
}

#[test]
fn the_signing_build_refuses_an_unsigned_image() {
    refused(Build::Ed25519, &dfu(ELF, None).unwrap());
}

#[test]
fn the_signing_build_refuses_another_key() {
    let other = Key::from_seed(&[0x42; 32]);
    assert_ne!(other.public(), key().public());
    refused(Build::Ed25519, &dfu(ELF, Some(&other)).unwrap());
}

#[test]
fn a_flipped_body_byte_is_refused_by_both_builds() {
    let mut image = dfu(ELF, Some(&key())).unwrap();
    image[0x200 + BIN.len() - 1] ^= 0x01;
    refused(Build::Ed25519, &image);
    refused(Build::Unsigned, &image);
}

/// The longest image `bm-image` builds boots from both slots: the limit is
/// inside what the bootloader swaps.
#[test]
fn the_longest_image_boots() {
    let key = key();
    let body_len = bm_image::MAX_IMAGE_LEN - 0x200 - bm_mcuboot::tlv::ED25519_SIZE;
    let mut body = BIN.to_vec();
    body.resize(body_len, 0x5A);
    let image = bm_image::from_body(&body, Some(&key)).unwrap();
    assert_eq!(image.len(), bm_image::MAX_IMAGE_LEN);
    boots(Build::Ed25519, &image);
}
