use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=inject/kpc_inject.c");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    if target_os != "macos" || target_arch != "aarch64" {
        panic!(
            "apmc only supports macOS on Apple Silicon (aarch64). \
             Current target: {target_os}/{target_arch}"
        );
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dylib_path = out_dir.join("libkpc_inject.dylib");

    // Map the Rust build profile to C compiler optimization flags.
    let opt_flag = match env::var("PROFILE").as_deref() {
        Ok("release") => "-O2",
        _ => "-O0",
    };

    let mut cc = Command::new("cc");
    cc.args([
        "-dynamiclib",
        opt_flag,
        "-Wall",
        "-Wpedantic",
        "-Werror",
        "-o",
        dylib_path.to_str().unwrap(),
        "inject/kpc_inject.c",
        "-lpthread",
    ]);

    // Include debug symbols in debug builds.
    if env::var("PROFILE").as_deref() != Ok("release") {
        cc.arg("-g");
    }

    let status = cc
        .status()
        .expect("failed to invoke cc — is Xcode or CommandLineTools installed?");

    assert!(
        status.success(),
        "failed to compile inject/kpc_inject.c into dylib"
    );

    println!("cargo:rustc-env=KPC_INJECT_DYLIB={}", dylib_path.display());
}
