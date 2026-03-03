use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Only needed for Windows targets.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x64",
        Ok("x86") => "x86",
        Ok("aarch64") => "arm64",
        arch => panic!("unsupported target arch for ConPTY: {arch:?}"),
    };

    // OUT_DIR = target/{profile}/build/{crate}-{hash}/out
    // Walk up three parents to reach target/{profile}/
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let profile_dir = out_dir
        .parent() // drop "out"
        .and_then(|p| p.parent()) // drop "{crate}-{hash}"
        .and_then(|p| p.parent()) // drop "build"
        .expect("OUT_DIR did not have the expected depth inside target/");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/pty -> crates -> workspace root
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("unexpected crate layout");
    let script = workspace_root.join("scripts/fetch-conpty.ts");

    println!("cargo:rerun-if-changed={}", script.display());

    let status = Command::new("bun")
        .arg("run")
        .arg(&script)
        .arg(arch)
        .arg(profile_dir)
        .status()
        .expect("failed to run fetch-conpty.ts — is bun installed?");

    if !status.success() {
        panic!("fetch-conpty.ts failed; ConPTY binaries could not be fetched");
    }
}
