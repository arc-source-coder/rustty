use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let zig_target = zig_target();

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("unexpected crate layout");
    let conpty_paths: Vec<PathBuf> = vec![
        workspace_root.join("vendor/zconpty/src"),
        workspace_root.join("vendor/zconpty/build.zig"),
        workspace_root.join("vendor/zconpty//build.zig.zon"),
    ];
    let zig_paths: Vec<PathBuf> = vec![
        workspace_root.join("zig/src"),
        workspace_root.join("zig/ghostty"),
        workspace_root.join("zig/ghostty_shim.zig"),
        workspace_root.join("zig/zconpty_shim.zig"),
        workspace_root.join("zig/build.zig"),
        workspace_root.join("zig/build.zig.zon"),
    ];

    for path in conpty_paths {
        println!("cargo:rerun-if-changed={}", &path.display());
    }
    for path in zig_paths {
        println!("cargo:rerun-if-changed={}", &path.display());
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let prefix = out_dir.join("zig-out");

    let status = Command::new("zig")
        .arg("build")
        .arg("-Doptimize=ReleaseFast")
        .arg(format!("-Dtarget={zig_target}"))
        .arg("--prefix")
        .arg(&prefix)
        .current_dir(&workspace_root.join("zig"))
        .status()
        .unwrap_or_else(|error| panic!("failed to run `zig build` for zconpty shim: {error}"));
    if !status.success() {
        panic!("zig build failed for zconpty shim");
    }

    let lib_dir = prefix.join("lib");
    if !contains_static_library(&lib_dir, "zconpty_shim") {
        panic!(
            "zconpty_shim static library was not found in {}",
            lib_dir.display()
        );
    }
    if !contains_static_library(&lib_dir, "ghostty_shim") {
        panic!(
            "ghostty_shim static library was not found in {}",
            lib_dir.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=zconpty_shim");
    println!("cargo:rustc-link-lib=static=ghostty_shim");

    println!("cargo:rustc-link-lib=advapi32");
}

fn zig_target() -> String {
    let zig_arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86_64",
        Ok("aarch64") => "aarch64",
        arch => panic!("unsupported target arch for conpty: {arch:?}"),
    };
    let zig_env = match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => "msvc",
        Ok("gnu") => "gnu",
        env => panic!("unsupported target env for conpty: {env:?}"),
    };

    format!("{zig_arch}-windows-{zig_env}")
}

fn contains_static_library(lib_dir: &Path, name: &str) -> bool {
    lib_dir.join(format!("{name}.lib")).exists() || lib_dir.join(format!("lib{name}.a")).exists()
}
