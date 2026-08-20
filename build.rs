use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// bm_core sources, grouped by what they need from the platform shim.
/// See README.md for the tier contract and the exclusion list.
mod tiers {
    /// T0 — pure functions; no shim required.
    pub const T0: &[&str] = &[
        "third_party/crc/crc16.c",
        "third_party/crc/crc32.c",
        "common/util.c",
        "common/lib_state_machine.c",
        "common/device.c",
        "network/l2_policy.c",
    ];

    /// T1 — needs the bm_os shim (allocation, queues, tasks, timers).
    pub const T1: &[&str] = &[
        "common/aligned_malloc.c",
        "common/ll.c",
        "common/q.c",
        "common/pcap.c",
        "common/cb_queue.c",
        "common/timer_callback_handler.c",
        "bcmp/packet.c",
    ];

    /// T2 — needs tinycbor. The CBOR codecs are the payload half of the wire
    /// format, so these are prime differential-fuzzing targets.
    pub const T2: &[&str] = &[
        "third_party/tinycbor/src/cborparser.c",
        "third_party/tinycbor/src/cborencoder.c",
        "third_party/tinycbor/src/cborencoder_float.c",
        "third_party/tinycbor/src/cborerrorstrings.c",
        "third_party/tinycbor/src/cborvalidation.c",
        "third_party/tinycbor/src/cborpretty.c",
        "bcmp/configuration.c",
        "middleware/cbor_service_helper.c",
        // bm_common_messages ships some types as .c and some as namespaced
        // C++; only the C half is bound for now. sensor_header_msg exists as
        // both -- take the .c.
        "bm_common_messages/bm_messages_helper.c",
        "bm_common_messages/config_cbor_map_srv_reply_msg.c",
        "bm_common_messages/config_cbor_map_srv_request_msg.c",
        "bm_common_messages/metrics_reply_msg.c",
        "bm_common_messages/sensor_header_msg.c",
        "bm_common_messages/sys_info_svc_reply_msg.c",
        "bm_common_messages/power_info_reply_msg.c",
    ];

    /// The platform layer bm_core leaves to the integrator, implemented in
    /// this repo rather than vendored. Paths are relative to csrc/.
    pub const SHIM: &[&str] = &["bm_os_shim.c", "bm_generic_shim.c"];
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("vendor/bm_core");
    let csrc = manifest.join("csrc");

    // Only meaningful on a host target; never try this for thumbv8m.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("none") {
        panic!("bm-wire-sys is host-only; it must not be a dependency of bm-wire");
    }

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Include paths mirror BM_INCLUDES in vendor/bm_core/CMakeLists.txt, with
    // csrc/ first so our bm_config.h wins.
    let module_dirs: Vec<PathBuf> = [
        csrc.clone(),
        root.join("bcmp"),
        root.join("common"),
        root.join("integrations"),
        root.join("middleware"),
        root.join("network"),
        root.join("third_party"),
        root.join("third_party/crc"),
        root.join("third_party/tinycbor/src"),
        root.join("bm_common_messages"),
    ]
    .into_iter()
    .collect();


    // --- compile the C ---
    let mut build = cc::Build::new();
    for inc in &module_dirs {
        build.include(inc);
    }
    build
        // Matches the BM_HOSTED option in vendor/bm_core/CMakeLists.txt.
        .define("BM_HOSTED", None)
        // tinycbor configuration, from the same CMakeLists.
        .define("CBOR_CUSTOM_ALLOC_INCLUDE", Some("\"tinycbor_alloc.h\""))
        .define("CBOR_PARSER_MAX_RECURSIONS", Some("10"));

    for src in tiers::T0.iter().chain(tiers::T1).chain(tiers::T2) {
        build.file(root.join(src));
    }
    for src in tiers::SHIM {
        build.file(csrc.join(src));
    }

    // Match the sanitizer cargo-fuzz is using on the Rust side.
    if env::var("CARGO_CFG_FUZZING").is_ok() {
        build
            .flag("-fsanitize=address,undefined")
            .flag("-fno-omit-frame-pointer");
    }
    build.compile("bm_core"); // emits libbm_core.a and the link flags

    // --- generate the bindings ---
    let mut bindings = bindgen::Builder::default()
        .header(manifest.join("wrapper.h").to_str().unwrap())
        .derive_debug(true)
        .derive_default(true)
        .derive_partialeq(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    let guarded_roots = guarded_header_tree(&module_dirs, &out.join("guarded"));
    for inc in &guarded_roots {
        bindings = bindings.clang_arg(format!("-I{}", inc.display()));
    }

    // Bind everything declared by bm_core itself (and our shim control
    // surface), and nothing from libc. Allowlisting by file rather than by
    // symbol means coverage tracks the submodule as it moves upstream.
    // Everything in the guarded tree came from bm_core or csrc/, and nothing
    // from libc did, so allowlisting the tree binds exactly our surface.
    // Matching by file rather than by symbol means coverage tracks the
    // submodule as it moves upstream.
    for root in &guarded_roots {
        bindings = bindings.allowlist_file(file_regex(root));
    }

    let bindings = bindings.generate().expect("bindgen failed");

    bindings.write_to_file(out.join("bindings.rs")).unwrap();

    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed={}", csrc.display());
}

/// A bindgen allowlist_file regex matching every header under `dir`.
/// bindgen matches against the full path clang reports, so anchor on the
/// absolute directory and escape the regex metacharacters a path can contain.
fn file_regex(dir: &Path) -> String {
    let escaped = dir.display().to_string().replace('.', r"\.");
    format!("{escaped}/.*\\.h")
}

/// Copy every header from `module_dirs` into a parallel tree under `dest`,
/// adding an include guard to the two dozen bm_core headers that lack one, and
/// return the copied include roots.
///
/// A generated stub that forwards with `#include_next` is not enough: for a
/// quoted include, the compiler searches the including file's own directory
/// first, so a sibling header reaches the unguarded original directly and
/// bypasses the stub. Only shadowing the whole tree catches every path. The C
/// build still compiles against vendor/ so debug info points at real sources;
/// this tree exists purely so bindgen can parse every header in one unit.
///
/// Adding guards upstream in bm_core would make all of this unnecessary.
fn guarded_header_tree(module_dirs: &[PathBuf], dest: &Path) -> Vec<PathBuf> {
    let _ = fs::remove_dir_all(dest);
    let mut roots = Vec::new();

    for (index, dir) in module_dirs.iter().enumerate() {
        // Index rather than basename: several include roots are nested
        // (third_party, third_party/crc) and would otherwise collide.
        let root = dest.join(index.to_string());
        for header in headers_under(dir) {
            let rel = header.strip_prefix(dir).unwrap();
            let copy = root.join(rel);
            fs::create_dir_all(copy.parent().unwrap()).unwrap();
            let text = fs::read_to_string(&header).unwrap_or_default();
            fs::write(&copy, guarded(&text, rel)).unwrap();
        }
        roots.push(root);
    }
    roots
}

fn guarded(text: &str, rel: &Path) -> String {
    if has_include_guard(text) {
        return text.to_string();
    }
    let guard = format!(
        "BM_WIRE_SYS_GUARD_{}",
        rel.to_string_lossy()
            .to_uppercase()
            .replace(['/', '.', '-'], "_")
    );
    format!("#ifndef {guard}\n#define {guard}\n{text}\n#endif // {guard}\n")
}

fn headers_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(headers_under(&path));
        } else if path.extension().is_some_and(|e| e == "h") {
            found.push(path);
        }
    }
    found.sort();
    found
}

fn has_include_guard(text: &str) -> bool {
    text.contains("#pragma once")
        || text
            .lines()
            .any(|line| line.trim_start().starts_with("#ifndef __"))
}
