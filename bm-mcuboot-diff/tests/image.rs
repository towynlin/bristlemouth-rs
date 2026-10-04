//! `bm_mcuboot`'s header and TLV codecs on an image `imgtool` signed and on
//! images `boot_go` boots.

use bm_mcuboot::image::{BM_HDR_SIZE, HEADER_SIZE, IMAGE_MAGIC};
use bm_mcuboot::{Header, Tlv, TlvArea, Version, tlv};
use bm_mcuboot_diff::image;
use bm_mcuboot_sys::{Area, Build, lock, sha256};

/// `body.bin` signed with the test key by `imgtool`; the command is in
/// `bm-mcuboot-sys/README.md`.
const SIGNED: &[u8] = include_bytes!("../../bm-mcuboot-sys/testdata/body.signed.dfu.bin");
const BODY: &[u8] = include_bytes!("../../bm-mcuboot-sys/testdata/body.bin");

const HDR_SIZE: usize = BM_HDR_SIZE as usize;

/// `--version 1.2.3+4`.
const SIGNED_VERSION: Version = Version {
    major: 1,
    minor: 2,
    revision: 3,
    build_num: 4,
};

fn entries<'a>(area: &TlvArea<'a>) -> Vec<Tlv<'a>> {
    area.iter().collect::<Result<_, _>>().unwrap()
}

#[test]
fn imgtools_signed_image_decodes() {
    let header = Header::decode(SIGNED).unwrap();
    assert_eq!(
        header,
        Header {
            magic: IMAGE_MAGIC,
            load_addr: 0,
            hdr_size: BM_HDR_SIZE,
            protect_tlv_size: 0,
            img_size: 64,
            flags: 0,
            version: SIGNED_VERSION,
            pad1: 0,
        }
    );
    assert_eq!(header, Header::new(64, SIGNED_VERSION));
    assert_eq!(header.encode(), SIGNED[..HEADER_SIZE]);
    assert!(SIGNED[HEADER_SIZE..HDR_SIZE].iter().all(|&b| b == 0xFF));
    assert_eq!(&SIGNED[HDR_SIZE..HDR_SIZE + 64], BODY);

    let area = TlvArea::parse(SIGNED, &header).unwrap();
    assert_eq!(area.start(), HDR_SIZE + 64);
    assert_eq!(area.end(), SIGNED.len());
    assert_eq!(area.end() - area.start(), tlv::ED25519_SIZE);

    let tlvs = entries(&area);
    assert_eq!(
        tlvs.iter()
            .map(|t| (t.kind, t.value.len(), t.protected))
            .collect::<Vec<_>>(),
        [
            (tlv::SHA256, 32, false),
            (tlv::KEYHASH, 32, false),
            (tlv::ED25519, 64, false)
        ]
    );
    assert_eq!(tlvs[0].value, sha256(&SIGNED[..area.start()]));
    assert_eq!(area.find(tlv::ED25519), Some(tlvs[2].value));
}

#[test]
fn imgtools_signed_image_reencodes() {
    let header = Header::decode(SIGNED).unwrap();
    let area = TlvArea::parse(SIGNED, &header).unwrap();
    let tlvs = entries(&area);

    let mut rebuilt = Header::new(BODY.len() as u32, SIGNED_VERSION)
        .encode()
        .to_vec();
    rebuilt.resize(HDR_SIZE, 0xFF);
    rebuilt.extend_from_slice(BODY);
    let digest = sha256(&rebuilt);
    rebuilt.extend_from_slice(&tlv::encode_ed25519(
        &digest,
        tlvs[1].value.try_into().unwrap(),
        tlvs[2].value.try_into().unwrap(),
    ));
    assert_eq!(rebuilt, SIGNED);
}

#[test]
fn an_encoded_unsigned_image_boots_and_decodes() {
    let version = Version {
        major: 0,
        minor: 13,
        revision: 12,
        build_num: 1_658_369_488,
    };
    let body: Vec<u8> = (0..1000_u32).map(|i| i as u8).collect();
    let bytes = image(&body, version);
    assert_eq!(bytes.len(), HDR_SIZE + body.len() + tlv::UNSIGNED_SIZE);

    for build in [Build::Unsigned, Build::Ed25519] {
        let mut oracle = lock(build);
        oracle.reset();
        oracle.write(Area::Primary, 0, &bytes);
        let booted = oracle.boot_go();
        match build {
            Build::Unsigned => {
                let booted = booted.unwrap();
                assert_eq!(booted.image_off, Area::Primary.offset());
                // The header MCUboot hands the application is the one encoded.
                assert_eq!(
                    Header::decode(&booted.header),
                    Some(Header::new(1000, version))
                );
            }
            // The signing build wants the two TLVs this image lacks.
            Build::Ed25519 => assert!(booted.is_err()),
        }
    }

    let header = Header::decode(&bytes).unwrap();
    let area = TlvArea::parse(&bytes, &header).unwrap();
    assert_eq!(area.end(), bytes.len());
    let tlvs = entries(&area);
    assert_eq!(tlvs.len(), 1);
    assert_eq!(tlvs[0].kind, tlv::SHA256);
    assert_eq!(tlvs[0].value, sha256(&bytes[..area.start()]));
}

#[test]
fn the_signed_image_boots_from_slot_2_after_a_rust_mark() {
    use bm_mcuboot::{Trailer, set_confirmed, set_pending};
    use bm_mcuboot_diff::RamSlot;

    let mut oracle = lock(Build::Ed25519);
    oracle.reset();
    oracle.write(Area::Primary, 0, SIGNED);
    // The same image with another version: its signature no longer matches.
    let mut other = SIGNED.to_vec();
    other[20] = 9;
    oracle.write(Area::Secondary, 0, &other);

    let mut slot = RamSlot::of(&oracle, Area::Secondary);
    assert_eq!(set_pending(&mut slot, &Trailer::BM, false), Ok(()));
    oracle.write(Area::Secondary, 0, &slot.bytes);
    // Refused and erased; slot 1 boots.
    assert_eq!(oracle.boot_go().unwrap().header, SIGNED[..HEADER_SIZE]);
    assert!(oracle.read_area(Area::Secondary).iter().all(|&b| b == 0xFF));

    // A properly signed update swaps in and is confirmed.
    oracle.write(Area::Secondary, 0, SIGNED);
    let mut slot = RamSlot::of(&oracle, Area::Secondary);
    assert_eq!(set_pending(&mut slot, &Trailer::BM, false), Ok(()));
    oracle.write(Area::Secondary, 0, &slot.bytes);
    assert_eq!(oracle.boot_go().unwrap().header, SIGNED[..HEADER_SIZE]);
    assert_eq!(oracle.swap_type(), Ok(bm_mcuboot_sys::swap_type::REVERT));

    let mut slot = RamSlot::of(&oracle, Area::Primary);
    assert_eq!(set_confirmed(&mut slot, &Trailer::BM), Ok(()));
    oracle.write(Area::Primary, 0, &slot.bytes);
    assert_eq!(oracle.swap_type(), Ok(bm_mcuboot_sys::swap_type::NONE));
}
