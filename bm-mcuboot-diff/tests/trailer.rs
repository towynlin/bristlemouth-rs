//! `bm_mcuboot::set_pending`, `set_confirmed`, `read_swap_state` and
//! `swap_type` against `boot_set_pending`, `boot_set_confirmed` and
//! `boot_swap_type`, from the same flash.

use bm_mcuboot::{
    Trailer, Version, read_swap_state, set_confirmed, set_pending, swap_type as rust_swap_type,
};
use bm_mcuboot_diff::{RamSlot, image, refusal};
use bm_mcuboot_sys::{Area, Booted, Build, HEADER_SIZE, Oracle, Refusal, lock, swap_type};

const T: Trailer = Trailer::BM;
const SIZE: u32 = Area::Secondary.size();

#[derive(Clone, Copy, Debug)]
enum Op {
    /// On the secondary slot.
    Pending { permanent: bool },
    /// On the primary slot.
    Confirmed,
}

fn fresh() -> Oracle {
    let mut oracle = lock(Build::Unsigned);
    oracle.reset();
    oracle
}

fn version(major: u8) -> Version {
    Version {
        major,
        ..Version::default()
    }
}

fn image_a() -> Vec<u8> {
    image(b"image a", version(1))
}

fn image_b() -> Vec<u8> {
    image(b"image b, which is longer", version(2))
}

/// `a` in slot 1 and `b` in slot 2, no trailers.
fn two_images() -> Oracle {
    let mut oracle = fresh();
    oracle.write(Area::Primary, 0, &image_a());
    oracle.write(Area::Secondary, 0, &image_b());
    oracle
}

fn booted(image: &[u8]) -> Result<Booted, Refusal> {
    Ok(Booted {
        image_off: Area::Primary.offset(),
        flash_dev_id: 0,
        header: image[..HEADER_SIZE].try_into().unwrap(),
    })
}

/// The last `n` bytes of `area`.
fn tail(oracle: &Oracle, area: Area, n: u32) -> Vec<u8> {
    let mut buf = vec![0; n as usize];
    oracle.read(area, area.size() - n, &mut buf);
    buf
}

fn starts_with(oracle: &Oracle, area: Area, image: &[u8]) -> bool {
    let mut buf = vec![0; image.len()];
    oracle.read(area, 0, &mut buf);
    buf == image
}

/// `value`, then `0xFF` to 16 bytes: one trailer field.
fn field(value: u8) -> [u8; 16] {
    let mut field = [0xFF; 16];
    field[0] = value;
    field
}

/// Panics with the first differing offset instead of two megabyte dumps.
fn assert_same(rust: &[u8], c: &[u8], what: &str) {
    if rust != c {
        let at = rust.iter().zip(c).position(|(a, b)| a != b).unwrap();
        panic!(
            "{what}: differs at {at:#x}: rust {:#04x}, c {:#04x}",
            rust[at], c[at]
        );
    }
}

/// `bm_mcuboot::swap_type` over copies of the slots, as `boot_swap_type`
/// numbers it.
fn rust_type(primary: &mut RamSlot, secondary: &mut RamSlot) -> i32 {
    let primary = read_swap_state(primary, &T).unwrap();
    let secondary = read_swap_state(secondary, &T).unwrap();
    rust_swap_type(&primary, &secondary) as i32
}

/// Run `op` on the oracle and on copies of its slots. Results, both slots'
/// bytes, and the swap type before and after must agree.
fn both(oracle: &mut Oracle, op: Op) -> Result<(), Refusal> {
    let mut primary = RamSlot::of(oracle, Area::Primary);
    let mut secondary = RamSlot::of(oracle, Area::Secondary);
    assert_eq!(
        Ok(rust_type(&mut primary, &mut secondary)),
        oracle.swap_type(),
        "swap type before {op:?}"
    );

    let rust = refusal(match op {
        Op::Pending { permanent } => set_pending(&mut secondary, &T, permanent),
        Op::Confirmed => set_confirmed(&mut primary, &T),
    });
    let c = match op {
        Op::Pending { permanent } => oracle.set_pending(permanent),
        Op::Confirmed => oracle.set_confirmed(),
    };

    assert_eq!(rust, c, "{op:?}");
    assert_same(
        &primary.bytes,
        &oracle.read_area(Area::Primary),
        &format!("slot 1 after {op:?}"),
    );
    assert_same(
        &secondary.bytes,
        &oracle.read_area(Area::Secondary),
        &format!("slot 2 after {op:?}"),
    );
    assert_eq!(
        Ok(rust_type(&mut primary, &mut secondary)),
        oracle.swap_type(),
        "swap type after {op:?}"
    );
    c
}

#[test]
fn an_erased_trailer_becomes_pending() {
    let mut oracle = two_images();
    assert_eq!(both(&mut oracle, Op::Pending { permanent: false }), Ok(()));
    assert_eq!(
        tail(&oracle, Area::Secondary, 80),
        [[0xFF; 16], field(0x02), [0xFF; 16], [0xFF; 16], T.magic()].concat()
    );
    assert_eq!(oracle.swap_type(), Ok(swap_type::TEST));
    assert!(starts_with(&oracle, Area::Secondary, &image_b()));
}

#[test]
fn an_erased_trailer_becomes_permanent() {
    let mut oracle = two_images();
    assert_eq!(both(&mut oracle, Op::Pending { permanent: true }), Ok(()));
    assert_eq!(
        tail(&oracle, Area::Secondary, 80),
        [[0xFF; 16], field(0x03), [0xFF; 16], field(0x01), T.magic()].concat()
    );
    assert_eq!(oracle.swap_type(), Ok(swap_type::PERM));
}

#[test]
fn a_pending_trailer_is_left_alone() {
    let mut oracle = two_images();
    assert_eq!(both(&mut oracle, Op::Pending { permanent: false }), Ok(()));
    let pending = oracle.read_area(Area::Secondary);

    assert_eq!(both(&mut oracle, Op::Pending { permanent: false }), Ok(()));
    // Asking for permanent does not make a test swap permanent.
    assert_eq!(both(&mut oracle, Op::Pending { permanent: true }), Ok(()));
    assert!(oracle.read_area(Area::Secondary) == pending);
    assert_eq!(oracle.swap_type(), Ok(swap_type::TEST));
}

#[test]
fn a_confirmed_trailer_is_left_alone() {
    // Slot 2 pending permanently: its image_ok is set.
    let mut oracle = two_images();
    assert_eq!(both(&mut oracle, Op::Pending { permanent: true }), Ok(()));
    let permanent = oracle.read_area(Area::Secondary);
    assert_eq!(both(&mut oracle, Op::Pending { permanent: false }), Ok(()));
    assert!(oracle.read_area(Area::Secondary) == permanent);

    // Slot 1 after a permanent swap: magic, copy_done and image_ok all set.
    assert_eq!(oracle.boot_go(), booted(&image_b()));
    assert_eq!(
        tail(&oracle, Area::Primary, 48),
        [field(0x01), field(0x01), T.magic()].concat()
    );
    let confirmed = oracle.read_area(Area::Primary);
    assert_eq!(both(&mut oracle, Op::Confirmed), Ok(()));
    assert!(oracle.read_area(Area::Primary) == confirmed);
    assert_eq!(oracle.swap_type(), Ok(swap_type::NONE));
}

#[test]
fn a_slot_with_no_image_is_marked_all_the_same() {
    // Neither function looks at the image.
    let mut oracle = fresh();
    assert_eq!(both(&mut oracle, Op::Confirmed), Ok(()));
    assert!(oracle.read_area(Area::Primary).iter().all(|&b| b == 0xFF));
    assert_eq!(both(&mut oracle, Op::Pending { permanent: false }), Ok(()));
    assert_eq!(oracle.swap_type(), Ok(swap_type::TEST));
    assert_eq!(oracle.boot_go(), Err(Refusal::Code(1)));

    // With an image to fall back on, the bootloader erases the empty slot.
    drop(oracle);
    let mut oracle = fresh();
    oracle.write(Area::Primary, 0, &image_a());
    assert_eq!(both(&mut oracle, Op::Pending { permanent: false }), Ok(()));
    assert_eq!(oracle.boot_go(), booted(&image_a()));
    assert!(oracle.read_area(Area::Secondary).iter().all(|&b| b == 0xFF));
    assert_eq!(both(&mut oracle, Op::Confirmed), Ok(()));
}

#[test]
fn set_confirmed_before_and_after_a_test_swap() {
    let mut oracle = two_images();

    // Before: slot 1 has no trailer, and gets none.
    let before = oracle.read_area(Area::Primary);
    assert_eq!(both(&mut oracle, Op::Confirmed), Ok(()));
    assert!(oracle.read_area(Area::Primary) == before);

    assert_eq!(both(&mut oracle, Op::Pending { permanent: false }), Ok(()));
    assert_eq!(both(&mut oracle, Op::Confirmed), Ok(()));
    assert!(oracle.read_area(Area::Primary) == before);

    assert_eq!(oracle.boot_go(), booted(&image_b()));
    assert_eq!(oracle.swap_type(), Ok(swap_type::REVERT));
    assert_eq!(
        tail(&oracle, Area::Primary, 48),
        [field(0x01), [0xFF; 16], T.magic()].concat()
    );

    // After: image_ok is written, once.
    assert_eq!(both(&mut oracle, Op::Confirmed), Ok(()));
    assert_eq!(
        tail(&oracle, Area::Primary, 48),
        [field(0x01), field(0x01), T.magic()].concat()
    );
    assert_eq!(oracle.swap_type(), Ok(swap_type::NONE));
    assert_eq!(both(&mut oracle, Op::Confirmed), Ok(()));
    assert_eq!(oracle.boot_go(), booted(&image_b()));

    // The old image, now in slot 2, can be marked again.
    assert!(starts_with(&oracle, Area::Secondary, &image_a()));
    assert_eq!(both(&mut oracle, Op::Pending { permanent: false }), Ok(()));
    assert_eq!(oracle.swap_type(), Ok(swap_type::TEST));
}

#[test]
fn a_bad_magic() {
    // set_pending erases the slot and returns BOOT_EBADIMAGE.
    let mut oracle = two_images();
    oracle.write(Area::Secondary, SIZE - 1, &[0x00]);
    assert_eq!(
        both(&mut oracle, Op::Pending { permanent: false }),
        Err(Refusal::Code(3))
    );
    assert!(oracle.read_area(Area::Secondary).iter().all(|&b| b == 0xFF));

    // set_confirmed returns BOOT_EBADVECT and writes nothing.
    oracle.write(Area::Primary, SIZE - 16, &[0x00]);
    let before = oracle.read_area(Area::Primary);
    assert_eq!(both(&mut oracle, Op::Confirmed), Err(Refusal::Code(4)));
    assert!(oracle.read_area(Area::Primary) == before);
}

#[test]
fn a_failed_write() {
    // swap_info's block is not erased: the magic goes in, swap_info fails.
    let mut oracle = two_images();
    oracle.write(Area::Secondary, T.swap_info_off() + 5, &[0x00]);
    assert_eq!(
        both(&mut oracle, Op::Pending { permanent: false }),
        Err(Refusal::Code(1))
    );
    assert_eq!(tail(&oracle, Area::Secondary, 16), T.magic());
    assert_eq!(oracle.swap_type(), Ok(swap_type::TEST));
    // The magic is good now, so a second call succeeds without writing.
    assert_eq!(both(&mut oracle, Op::Pending { permanent: false }), Ok(()));

    // image_ok's block is not erased: permanent stops after the magic.
    drop(oracle);
    let mut oracle = two_images();
    oracle.write(Area::Secondary, T.image_ok_off() + 1, &[0x7F]);
    assert_eq!(
        both(&mut oracle, Op::Pending { permanent: true }),
        Err(Refusal::Code(1))
    );
    assert_eq!(
        tail(&oracle, Area::Secondary, 64)[..16],
        [0xFF; 16],
        "swap_info"
    );

    // The same for set_confirmed.
    drop(oracle);
    let mut oracle = two_images();
    oracle.write(Area::Primary, T.magic_off(), &T.magic());
    oracle.write(Area::Primary, T.image_ok_off() + 15, &[0xFE]);
    assert_eq!(both(&mut oracle, Op::Confirmed), Err(Refusal::Code(1)));
}

/// What the bootloader did with a marked slot.
#[derive(PartialEq)]
struct Outcome {
    swap_types: Vec<Result<i32, Refusal>>,
    boots: Vec<Result<Booted, Refusal>>,
    flash: Vec<Vec<u8>>,
}

/// Mark slot 2, boot, optionally confirm, boot twice more. With `rust`, the
/// marks are `bm-mcuboot`'s, stored into the oracle's flash.
fn scenario(rust: bool, permanent: bool, confirm: bool) -> Outcome {
    let mut oracle = two_images();
    let mut outcome = Outcome {
        swap_types: Vec::new(),
        boots: Vec::new(),
        flash: Vec::new(),
    };

    if rust {
        let mut slot = RamSlot::of(&oracle, Area::Secondary);
        assert_eq!(set_pending(&mut slot, &T, permanent), Ok(()));
        oracle.write(Area::Secondary, 0, &slot.bytes);
    } else {
        assert_eq!(oracle.set_pending(permanent), Ok(()));
    }
    outcome.swap_types.push(oracle.swap_type());
    outcome.boots.push(oracle.boot_go());

    if confirm {
        if rust {
            let mut slot = RamSlot::of(&oracle, Area::Primary);
            assert_eq!(set_confirmed(&mut slot, &T), Ok(()));
            oracle.write(Area::Primary, 0, &slot.bytes);
        } else {
            assert_eq!(oracle.set_confirmed(), Ok(()));
        }
    }
    for _ in 0..2 {
        outcome.swap_types.push(oracle.swap_type());
        outcome.boots.push(oracle.boot_go());
    }
    for area in [Area::Primary, Area::Secondary, Area::Scratch] {
        outcome.flash.push(oracle.read_area(area));
    }
    outcome
}

#[test]
fn the_bootloader_treats_rust_marks_as_its_own() {
    let (a, b) = (image_a(), image_b());
    for (permanent, confirm, types, boots) in [
        // Test, not confirmed: swapped, then reverted.
        (
            false,
            false,
            [swap_type::TEST, swap_type::REVERT, swap_type::NONE],
            [&b, &a, &a],
        ),
        // Test, confirmed: stays.
        (
            false,
            true,
            [swap_type::TEST, swap_type::NONE, swap_type::NONE],
            [&b, &b, &b],
        ),
        (
            true,
            false,
            [swap_type::PERM, swap_type::NONE, swap_type::NONE],
            [&b, &b, &b],
        ),
        (
            true,
            true,
            [swap_type::PERM, swap_type::NONE, swap_type::NONE],
            [&b, &b, &b],
        ),
    ] {
        let case = format!("permanent {permanent}, confirm {confirm}");
        let c = scenario(false, permanent, confirm);
        let rust = scenario(true, permanent, confirm);
        assert_eq!(c.swap_types, types.map(Ok), "{case}");
        assert_eq!(c.boots, boots.map(|image| booted(image)), "{case}");
        assert_eq!(rust.swap_types, c.swap_types, "{case}");
        assert_eq!(rust.boots, c.boots, "{case}");
        for (rust, c) in rust.flash.iter().zip(&c.flash) {
            assert_same(rust, c, &case);
        }
    }
}

/// xorshift64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> usize {
        (self.next() % n) as usize
    }

    fn byte(&mut self) -> u8 {
        self.next() as u8
    }
}

/// The last 96 bytes of a slot: a status entry, `swap_size`, `swap_info`,
/// `copy_done`, `image_ok` and the magic, each erased, valid or damaged.
fn random_tail(rng: &mut Rng) -> [u8; 96] {
    let mut tail = [0xFF; 96];
    for block in 0..5 {
        let at = block * 16;
        tail[at] = match rng.below(9) {
            0..=2 => 0xFF,
            n @ 3..=7 => n as u8 - 3,
            _ => rng.byte(),
        };
        // Padding that is not erased makes the block's write fail.
        if rng.below(8) == 0 {
            tail[at + 1 + rng.below(15)] = rng.byte();
        }
    }
    let magic = &mut tail[80..];
    match rng.below(8) {
        0..=2 => {}
        3..=5 => magic.copy_from_slice(&T.magic()),
        6 => {
            // One bit away from good or from erased.
            if rng.below(2) == 0 {
                magic.copy_from_slice(&T.magic());
            }
            magic[rng.below(16)] ^= 1 << rng.below(8);
        }
        _ => magic.fill_with(|| rng.byte()),
    }
    tail
}

#[test]
fn random_trailers() {
    let mut oracle = fresh();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut results = std::collections::BTreeMap::new();
    for _ in 0..5000 {
        for area in [Area::Primary, Area::Secondary] {
            oracle.write(area, SIZE - 96, &random_tail(&mut rng));
        }
        let op = match rng.below(3) {
            0 => Op::Pending { permanent: false },
            1 => Op::Pending { permanent: true },
            _ => Op::Confirmed,
        };
        let result = both(&mut oracle, op);
        *results.entry(format!("{op:?} {result:?}")).or_insert(0) += 1;
    }
    // Every return of both functions was reached.
    for (op, codes) in [("Pending", &[1, 3][..]), ("Confirmed", &[1, 4][..])] {
        let reached = |result: &str| {
            results
                .keys()
                .any(|k| k.starts_with(op) && k.ends_with(result))
        };
        assert!(reached("Ok(())"), "{op}: {results:?}");
        for code in codes {
            assert!(reached(&format!("Err(Code({code}))")), "{op}: {results:?}");
        }
    }
}
