use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=inject/kpc_inject.c");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    if target_os != "macos" || target_arch != "aarch64" {
        // Compile a minimal stub so include_bytes! still resolves.
        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        let stub = out_dir.join("libkpc_inject.dylib");
        std::fs::write(&stub, b"").unwrap();
        println!("cargo:rustc-env=KPC_INJECT_DYLIB={}", stub.display());
        println!(
            "cargo:warning=kpc only works on macOS/aarch64; inject dylib not compiled for {target_os}/{target_arch}"
        );
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dylib_path = out_dir.join("libkpc_inject.dylib");

    let status = Command::new("cc")
        .args([
            "-dynamiclib",
            "-O2",
            "-Wall",
            "-o",
            dylib_path.to_str().unwrap(),
            "inject/kpc_inject.c",
            "-lpthread",
        ])
        .status()
        .expect("failed to invoke cc — is Xcode or CommandLineTools installed?");

    assert!(
        status.success(),
        "failed to compile inject/kpc_inject.c into dylib"
    );

    println!("cargo:rustc-env=KPC_INJECT_DYLIB={}", dylib_path.display());
}
