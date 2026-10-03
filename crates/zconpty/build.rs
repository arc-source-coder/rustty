use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

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

    let zig_target = format!("{zig_arch}-windows-{zig_env}");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root =
        manifest_dir.parent().and_then(|p| p.parent()).expect("unexpected crate layout");
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
        .current_dir(workspace_root.join("zig"))
        .status()
        .unwrap_or_else(|error| panic!("failed to run `zig build` for zconpty shim: {error}"));
    assert!(status.success(), "zig build failed for zconpty shim");

    let lib_dir = prefix.join("lib");
    assert!(
        contains_static_library(&lib_dir, "zconpty_shim"),
        "zconpty_shim static library was not found in {}",
        lib_dir.display()
    );
    assert!(
        contains_static_library(&lib_dir, "ghostty_shim"),
        "ghostty_shim static library was not found in {}",
        lib_dir.display()
    );

    let bin_dir = prefix.join("bin");
    let wslz = bin_dir.join("wslz.exe");

    let error = format!("wslz.exe was not found at {}", bin_dir.display());
    assert!(wslz.exists(), "{}", error);

    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR should be target/<profile>/build/<pkg>/out")
        .to_path_buf();
    std::fs::copy(&wslz, profile_dir.join("wslz.exe"))
        .expect("failed to copy wslz.exe next to final executable");

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=zconpty_shim");
    println!("cargo:rustc-link-lib=static=ghostty_shim");

    println!("cargo:rustc-link-lib=advapi32");
}

fn contains_static_library(lib_dir: &Path, name: &str) -> bool {
    lib_dir.join(format!("{name}.lib")).exists() || lib_dir.join(format!("lib{name}.a")).exists()
}
