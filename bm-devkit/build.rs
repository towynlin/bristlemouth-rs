//! Link scripts for the binaries: cortex-m-rt's `link.x`, which includes
//! `memory.x` from this directory, and defmt's. `BM_DEVKIT_GIT_SHA`: the
//! first 8 hex digits of `HEAD`, or `0` outside a git checkout, for
//! `DevkitIdentity`.

use std::process::Command;

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    println!("cargo:rustc-link-search={dir}");
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");

    let sha = git(&["rev-parse", "HEAD"])
        .and_then(|sha| sha.get(..8).map(str::to_owned))
        .unwrap_or_else(|| "0".to_owned());
    println!("cargo:rustc-env=BM_DEVKIT_GIT_SHA={sha}");
    for path in ["HEAD", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(head) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &head])
    {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
