use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What bm_protocol's bootloader compiles from MCUboot
/// (`src/apps/bootloader/CMakeLists.txt:7-32`, `:77-99`), relative to
/// `vendor/mcuboot`. Both builds compile all of it, as both of bm_protocol's
/// do; `mcuboot_config.h` decides what each file contributes.
const MCUBOOT: &[&str] = &[
    "boot/bootutil/src/boot_record.c",
    "boot/bootutil/src/bootutil_misc.c",
    "boot/bootutil/src/bootutil_public.c",
    "boot/bootutil/src/caps.c",
    "boot/bootutil/src/encrypted.c",
    "boot/bootutil/src/fault_injection_hardening.c",
    "boot/bootutil/src/fault_injection_hardening_delay_rng_mbedtls.c",
    "boot/bootutil/src/image_ec.c",
    "boot/bootutil/src/image_ec256.c",
    "boot/bootutil/src/image_ed25519.c",
    "boot/bootutil/src/image_rsa.c",
    "boot/bootutil/src/image_validate.c",
    "boot/bootutil/src/loader.c",
    "boot/bootutil/src/swap_misc.c",
    "boot/bootutil/src/swap_move.c",
    "boot/bootutil/src/swap_scratch.c",
    "boot/bootutil/src/tlv.c",
    "ext/fiat/src/curve25519.c",
    "ext/tinycrypt/lib/source/utils.c",
    "ext/tinycrypt/lib/source/sha256.c",
    "ext/tinycrypt-sha512/lib/source/sha512.c",
    "ext/tinycrypt/lib/source/hmac.c",
    "ext/tinycrypt/lib/source/aes_encrypt.c",
    "ext/tinycrypt/lib/source/ctr_mode.c",
    "ext/mbedtls-asn1/src/asn1parse.c",
    "ext/mbedtls-asn1/src/platform_util.c",
];

/// `:33-42`, `:87-90`, `:100-102` of the same file, with `csrc/` in place of
/// bm_protocol's port headers.
const MCUBOOT_INCLUDES: &[&str] = &[
    "boot",
    "boot/bootutil/include",
    "ext/tinycrypt/lib/include",
    "ext/tinycrypt-sha512/lib/include",
    "ext/mbedtls-asn1/include",
];

/// This crate's C, relative to `csrc/`.
const SHIM: &[&str] = &["bm_mcuboot.c", "test_ed25519_pub_key.c"];

/// The prefix the signing build's symbols carry. `src/lib.rs` links by it.
const SIGNED_PREFIX: &str = "ed25519_";

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("none") {
        panic!("bm-mcuboot-sys is host-only; it must not be a dependency of firmware");
    }

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("vendor/mcuboot");
    let csrc = manifest.join("csrc");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    if !root.join("boot/bootutil/src/loader.c").exists() {
        panic!(
            "bm-mcuboot-sys/vendor/mcuboot is empty; run \
             `git submodule update --init bm-mcuboot-sys/vendor/mcuboot` (not --recursive)"
        );
    }

    // No signature type.
    build(&root, &csrc).compile("bm_mcuboot");

    // The same sources again with MCUBOOT_SIGN_ED25519. Both archives link
    // into one test binary, so every symbol the first defines is renamed in
    // the second by a force-included header of `#define`s.
    let rename = out.join("ed25519_rename.h");
    fs::write(&rename, rename_header(&out.join("libbm_mcuboot.a"))).unwrap();
    build(&root, &csrc)
        .define("CONFIG_BOOT_SIGN_ED25519", None)
        .flag("-include")
        .flag(rename.to_str().unwrap())
        .compile("bm_mcuboot_ed25519");

    println!("cargo:rerun-if-changed={}", csrc.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=NM");
}

fn build(root: &Path, csrc: &Path) -> cc::Build {
    let mut build = cc::Build::new();
    build.include(csrc);
    for inc in MCUBOOT_INCLUDES {
        build.include(root.join(inc));
    }
    for src in MCUBOOT {
        build.file(root.join(src));
    }
    for src in SHIM {
        build.file(csrc.join(src));
    }
    build
        // bm_protocol src/CMakeLists.txt:124 and :356.
        .define("MCUBOOT_BOOT_MAX_ALIGN", Some("16"))
        // bm_protocol's per-file suppressions for the same sources
        // (src/apps/bootloader/CMakeLists.txt:44-72), applied to all.
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-format");
    build
}

/// `#define sym ed25519_sym` for every external symbol `archive` defines.
fn rename_header(archive: &Path) -> String {
    let nm = env::var("NM").unwrap_or_else(|_| "nm".into());
    let output = Command::new(&nm)
        // POSIX format: `name type value size`.
        .args(["-g", "-P"])
        .arg(archive)
        .output()
        .unwrap_or_else(|e| panic!("running {nm}: {e}"));
    assert!(output.status.success(), "{nm} failed on {archive:?}");

    // Mach-O prefixes every C symbol with an underscore.
    let apple = env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("apple");
    let mut symbols: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let kind = fields.next()?;
            // Member headers (`lib.a[x.o]:`) have no type; `U` is a reference.
            (kind.len() == 1 && kind != "U").then_some(name)
        })
        .map(|name| match name.strip_prefix('_') {
            Some(stripped) if apple => stripped,
            _ => name,
        })
        .collect();
    symbols.sort_unstable();
    symbols.dedup();
    assert!(
        symbols.contains(&"boot_go"),
        "{nm} listed no boot_go in {archive:?}"
    );

    let mut header = String::new();
    for symbol in symbols {
        writeln!(header, "#define {symbol} {SIGNED_PREFIX}{symbol}").unwrap();
    }
    header
}
