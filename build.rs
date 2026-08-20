use std::env;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from("vendor/bm_core");

    // Only meaningful on a host target; never try this for thumbv8m.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("none") {
        panic!("bm-wire-sys is host-only; it must not be a dependency of bm-wire");
    }

    // --- compile the C ---
    let mut build = cc::Build::new();
    build
        .include(&root)
        .include(root.join("common"))
        .file(root.join("network/l2_policy.c"));

    // Match the sanitizer cargo-fuzz is using on the Rust side.
    if env::var("CARGO_CFG_FUZZING").is_ok() {
        build
            .flag("-fsanitize=address,undefined")
            .flag("-fno-omit-frame-pointer");
    }
    build.compile("bm_core"); // emits libbm_core.a and the link flags

    // --- generate the bindings ---
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", root.display()))
        .clang_arg(format!("-I{}", root.join("common").display()))
        .allowlist_function("bm_l2_policy_.*")
        .allowlist_type("BmL2Policy.*|BmIpAddr|L2LinkLocalRoutingCb")
        .derive_debug(true)
        .derive_default(true)
        .derive_partialeq(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("bindgen failed");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings.write_to_file(out.join("bindings.rs")).unwrap();
}
