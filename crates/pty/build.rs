use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Only needed for Windows targets.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let zig_arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86_64",
        Ok("x86") => "x86",
        Ok("aarch64") => "aarch64",
        arch => panic!("unsupported target arch for PTY: {arch:?}"),
    };
    let zig_env = match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => "msvc",
        Ok("gnu") => "gnu",
        env => panic!("unsupported target env for PTY: {env:?}"),
    };
    let zig_target = format!("{zig_arch}-windows-{zig_env}");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/pty -> crates -> workspace root
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("unexpected crate layout");
    let conpty_dir = workspace_root.join("conpty");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let profile_dir = target_profile_dir(&out_dir);

    println!(
        "cargo:rerun-if-changed={}",
        conpty_dir.join("build.zig").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        conpty_dir.join("build.zig.zon").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        conpty_dir.join("src").display()
    );

    let status = Command::new("zig")
        .arg("build")
        .arg("-Doptimize=ReleaseFast")
        .arg(format!("-Dtarget={zig_target}"))
        .current_dir(&conpty_dir)
        .status()
        .expect("failed to run `zig build` for conpty");

    if !status.success() {
        panic!("zig build failed for conpty");
    }

    let script = workspace_root.join("scripts/fetch-openconsole.ts");
    println!("cargo:rerun-if-changed={}", script.display());

    let fetch_status = Command::new("bun")
        .arg("run")
        .arg(&script)
        .arg(match zig_arch {
            "x86_64" => "x64",
            "x86" => "x86",
            "aarch64" => "arm64",
            _ => unreachable!(),
        })
        .arg(profile_dir)
        .status()
        .expect("failed to run fetch-openconsole.ts — is bun installed?");

    if !fetch_status.success() {
        panic!("fetch-openconsole.ts failed; OpenConsole.exe could not be fetched");
    }

    let lib_dir = conpty_dir.join("zig-out").join("lib");
    if !contains_conpty_library(&lib_dir) {
        panic!(
            "conpty static library was not produced in {}",
            lib_dir.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=conpty");
}

fn contains_conpty_library(lib_dir: &Path) -> bool {
    lib_dir.join("conpty.lib").exists() || lib_dir.join("libconpty.a").exists()
}

fn target_profile_dir(out_dir: &Path) -> &Path {
    out_dir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("OUT_DIR did not have the expected depth inside target/")
}
