//! Replay committed seed files through the comparators.
//!
//! `bm-wire/fuzz/corpus/` is generated and gitignored, so it cannot carry
//! anything durable. `bm-wire/fuzz/seeds/` is committed instead, and every file
//! in it is replayed by `cargo test`. That gives three things at once:
//!
//! * CI runs differential coverage without needing to run a fuzzer;
//! * a crash found by the fuzzer becomes a permanent regression test by
//!   dropping the minimized artifact into the matching seeds directory;
//! * `cargo fuzz run <target> seeds/<target>` starts from real traffic instead
//!   of from nothing.
//!
//! Seed files are decoded exactly as libFuzzer decodes them — via
//! [`Arbitrary::arbitrary_take_rest`] — so a file that replays here behaves
//! identically under the fuzzer.

use std::path::{Path, PathBuf};

use arbitrary::{Arbitrary, Unstructured};

/// Directory holding the committed seed corpora, one subdirectory per target.
#[must_use]
pub fn seeds_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("bm-wire")
        .join("fuzz")
        .join("seeds")
}

/// Decode `bytes` the way the fuzz target would, then run `check` on it.
///
/// A file that does not carry enough bytes to build the input is skipped
/// rather than failing: `Arbitrary` is allowed to refuse, and libFuzzer treats
/// that the same way.
fn replay_one<'a, T, F>(bytes: &'a [u8], check: F)
where
    T: Arbitrary<'a>,
    F: FnOnce(&T),
{
    if let Ok(input) = T::arbitrary_take_rest(Unstructured::new(bytes)) {
        check(&input);
    }
}

/// Replay every seed for `target`.
///
/// # Panics
///
/// If any seed diverges, or if `target` is not a known fuzz target.
pub fn replay_target(target: &str) -> usize {
    let dir = seeds_dir().join(target);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return 0,
    };

    let mut count = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).expect("seed file is readable");
        match target {
            "crc" => replay_one::<crate::crc::CrcInput, _>(&bytes, crate::crc::check),
            "l2_policy" => replay_one::<crate::l2_policy::L2PolicyInput, _>(&bytes, |i| {
                crate::l2_policy::check(i);
            }),
            "bcmp" => replay_one::<crate::bcmp::BcmpInput, _>(&bytes, |i| {
                crate::bcmp::check(i);
            }),
            "checksum" => replay_one::<crate::checksum::ChecksumInput, _>(&bytes, |i| {
                crate::checksum::check(i);
            }),
            "wildcard" => replay_one::<crate::util::WildcardInput, _>(&bytes, |i| {
                crate::util::check_wildcard(i);
            }),
            "strnlen" => replay_one::<crate::util::StrnlenInput, _>(&bytes, |i| {
                crate::util::check_strnlen(i);
            }),
            "addr" => replay_one::<crate::util::AddrInput, _>(&bytes, |i| {
                crate::util::check_addr(i);
            }),
            "date_time" => replay_one::<crate::util::DateTimeInput, _>(&bytes, |i| {
                crate::util::check_date_time(i);
            }),
            "time_remaining" => replay_one::<crate::util::TimeRemainingInput, _>(&bytes, |i| {
                crate::util::check_time_remaining(i);
            }),
            other => panic!("unknown fuzz target {other} (seed {})", path.display()),
        }
        count += 1;
    }
    count
}

/// Every fuzz target that has a seeds directory.
pub const TARGETS: &[&str] = &[
    "addr",
    "bcmp",
    "checksum",
    "crc",
    "date_time",
    "l2_policy",
    "strnlen",
    "time_remaining",
    "wildcard",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_committed_seed_still_agrees_with_the_c() {
        let mut total = 0;
        for target in TARGETS {
            total += replay_target(target);
        }
        assert!(
            total > 0,
            "no seeds replayed -- expected files under {}",
            seeds_dir().display()
        );
        eprintln!("replayed {total} seeds across {} targets", TARGETS.len());
    }
}
