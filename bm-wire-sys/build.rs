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

    /// T3 — the wire path. Needs an implementation of bm_ip.h and a
    /// NetworkDevice underneath L2.
    ///
    /// bm_linux.c is bm_core's own bm_ip.h backend and turns out to be exactly
    /// what an oracle wants: a software IPv6 stack that builds and parses
    /// Ethernet/IPv6/UDP frames in malloc'd buffers, with no sockets, threads,
    /// or clock anywhere in it. Using it rather than a hand-written stub means
    /// the framing a Rust port is compared against is bm_core's real framing.
    pub const T3: &[&str] = &[
        "network/bm_linux.c",
        "network/l2.c",
        "bcmp/bcmp.c",
        "bcmp/heartbeat.c",
        "bcmp/info.c",
        "bcmp/neighbors.c",
        "bcmp/ping.c",
        "bcmp/time.c",
        "bcmp/resource_discovery.c",
        "bcmp/config.c",
        "middleware/middleware.c",
        "middleware/pubsub.c",
        "middleware/bm_service.c",
        "middleware/bm_service_request.c",
        "middleware/echo_service.c",
        "middleware/sys_info_service.c",
        "middleware/power_info_service.c",
        "middleware/metrics_service.c",
        "middleware/config_cbor_map_service.c",
        "integrations/topology.c",
        "integrations/spotter.c",
        "integrations/file_ops.c",
    ];

    /// T4 — DFU. middleware/bristlemouth.c is excluded: it hard-wires
    /// adin2111_network_device(), so it cannot be brought up on any device but
    /// the real PHY. csrc/bm_stack_shim.c mirrors its init sequence against
    /// the capture device instead, the same way bm_sbc's runtime does.
    pub const T4: &[&str] = &[
        "bcmp/dfu_core.c",
        "bcmp/dfu_client.c",
        "bcmp/dfu_host.c",
    ];

    /// Compiled on its own so the warning suppression the mavlink headers
    /// need does not have to be applied to bm_core's own code.
    pub const MAVLINK: &str = "middleware/bm_mavlink.c";

    /// The platform layer bm_core leaves to the integrator, implemented in
    /// this repo rather than vendored. Paths are relative to csrc/.
    pub const SHIM: &[&str] = &[
        "bm_os_shim.c",
        "bm_generic_shim.c",
        "bm_net_device_shim.c",
        "bm_stack_shim.c",
    ];
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
    let mut build = bm_core_build(&module_dirs);
    for src in tiers::T0
        .iter()
        .chain(tiers::T1)
        .chain(tiers::T2)
        .chain(tiers::T3)
        .chain(tiers::T4)
    {
        build.file(root.join(src));
    }
    for src in tiers::SHIM {
        build.file(csrc.join(src));
    }
    build.compile("bm_core"); // emits libbm_core.a and the link flags

    // The mavlink headers take the address of packed members all over, which
    // gcc warns about 60 times over. bm_core suppresses exactly this on its
    // mavlink target, citing mavlink's own build-warnings advice, so do the
    // same -- scoped to the one translation unit that includes them rather
    // than blanketed over bm_core's own code.
    bm_core_build(&module_dirs)
        .flag("-Wno-address-of-packed-member")
        .file(root.join(tiers::MAVLINK))
        .compile("bm_core_mavlink");

    // --- generate the bindings ---
    let mut bindings = bindgen::Builder::default()
        .header(manifest.join("wrapper.h").to_str().unwrap())
        .derive_debug(true)
        .derive_default(true)
        .derive_partialeq(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        // util.h defines ip_to_nodeid and the uint8_to_uint* helpers as
        // `static inline`, which bindgen otherwise drops. They are endianness
        // code, which is exactly what a differential test should cover.
        .wrap_static_fns(true)
        .wrap_static_fns_path(out.join("static_fns"));

    let guarded_roots = guarded_header_tree(&module_dirs, &out.join("guarded"));
    for inc in &guarded_roots {
        bindings = bindings.clang_arg(format!("-I{}", inc.display()));
    }

    // Everything in the guarded tree came from bm_core or csrc/, and nothing
    // from libc did, so allowlisting the tree binds exactly our surface.
    // Matching by file rather than by symbol means coverage tracks the
    // submodule as it moves upstream.
    for root in &guarded_roots {
        bindings = bindings.allowlist_file(file_regex(root));
    }

    let bindings = bindings.generate().expect("bindgen failed");

    bindings.write_to_file(out.join("bindings.rs")).unwrap();

    // wrap_static_fns emits callable out-of-line copies of the static inline
    // functions; they have to be compiled and linked like any other source.
    // Compiled against the guarded header tree, not vendor/: static_fns.c
    // includes wrapper.h, so it hits the same missing-include-guard problem
    // bindgen does.
    bm_core_build(&guarded_roots)
        .file(out.join("static_fns.c"))
        // The generated file redeclares each wrapped function, which the -Wall
        // the rest of the build runs with objects to.
        .flag("-Wno-missing-prototypes")
        // It includes wrapper.h, so it pulls in the mavlink headers too.
        .flag("-Wno-address-of-packed-member")
        .compile("bm_core_static_fns");

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

/// A cc::Build carrying the settings every translation unit in this crate
/// needs, so the main build, the mavlink unit and bindgen's generated
/// static-function wrappers cannot drift apart.
fn bm_core_build(includes: &[PathBuf]) -> cc::Build {
    let mut build = cc::Build::new();
    for inc in includes {
        build.include(inc);
    }
    build
        // CMAKE_C_STANDARD 17 with C_EXTENSIONS NO, from
        // vendor/bm_core/CMakeLists.txt. Not cosmetic: gcc 15 defaults to
        // gnu23, whose stddef.h defines unreachable() and collides with
        // tinycbor's. An oracle should compile bm_core under the same
        // standard the firmware does.
        .std("c17")
        // Matches the BM_HOSTED option in the same CMakeLists.
        .define("BM_HOSTED", None)
        // tinycbor configuration, likewise.
        .define("CBOR_CUSTOM_ALLOC_INCLUDE", Some("\"tinycbor_alloc.h\""))
        .define("CBOR_PARSER_MAX_RECURSIONS", Some("10"));

    // Match the sanitizer cargo-fuzz is using on the Rust side.
    if env::var("CARGO_CFG_FUZZING").is_ok() {
        build
            .flag("-fsanitize=address,undefined")
            .flag("-fno-omit-frame-pointer");
    }
    build
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
