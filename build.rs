use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=inject/kpc_inject.c");

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

    println!(
        "cargo:rustc-env=KPC_INJECT_DYLIB={}",
        dylib_path.display()
    );
}
