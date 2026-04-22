use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // zig/ lives at the workspace root, two levels above crates/ghostty/
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("unexpected crate layout");

    let zig_dir = workspace_root.join("zig");

    let ghostty_dir = zig_dir.join("ghostty");
    if !ghostty_dir.exists() {
        panic!("zig/ghostty is missing; run `git submodule update --init --recursive` and retry");
    }

    let zig_paths: Vec<PathBuf> = vec![
        workspace_root.join("zig/src"),
        workspace_root.join("zig/ghostty"),
        workspace_root.join("zig/ghostty_shim.zig"),
        workspace_root.join("zig/zconpty_shim.zig"),
        workspace_root.join("zig/build.zig"),
        workspace_root.join("zig/build.zig.zon"),
    ];

    for path in zig_paths {
        println!("cargo:rerun-if-changed={}", &path.display());
    }

    let zig_version = Command::new("zig").arg("version").output().ok();
    if zig_version.is_none() {
        panic!("Zig 0.15.2 is required");
    }

    let zig_target = zig_target();
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let prefix = out_dir.join("zig-out");

    let status = Command::new("zig")
        .current_dir(&zig_dir)
        .arg("build")
        .arg("-Doptimize=ReleaseFast")
        .arg(format!("-Dtarget={zig_target}"))
        .arg("--prefix")
        .arg(&prefix)
        .status()
        .expect("failed to invoke zig");
    if !status.success() {
        panic!("zig build failed");
    }

    let lib_dir = prefix.join("lib");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=ghostty_shim");
}

fn zig_target() -> &'static str {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        panic!("ghostty shim only supports Windows targets");
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        panic!("ghostty shim only supports MSVC targets");
    }

    match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86_64-windows-msvc",
        Ok("aarch64") => "aarch64-windows-msvc",
        arch => panic!("unsupported target arch for ghostty shim: {arch:?}"),
    }
}
