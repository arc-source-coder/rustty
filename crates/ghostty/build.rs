use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // zig/ lives at the workspace root, two levels above crates/ghostty/
    let zig_dir = manifest_dir.join("../../zig");

    println!(
        "cargo:rerun-if-changed={}",
        zig_dir.join("build.zig").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        zig_dir.join("build.zig.zon").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        zig_dir.join("lib.zig").display()
    );
    emit_rerun_for_zig_sources(&zig_dir);

    let ghostty_dir = zig_dir.join("ghostty");
    if !ghostty_dir.exists() {
        panic!("zig/ghostty is missing; run `git submodule update --init --recursive` and retry");
    }

    let zig_version = Command::new("zig").arg("version").output().ok();
    if zig_version.is_none() {
        panic!("Zig 0.15.2 is required");
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let prefix = out_dir.join("zig-out");

    let status = Command::new("zig")
        .current_dir(&zig_dir)
        .arg("build")
        .arg("-Doptimize=ReleaseFast")
        .arg("-Dtarget=x86_64-windows-msvc")
        .arg("--prefix")
        .arg(&prefix)
        .status()
        .expect("failed to invoke zig");
    if !status.success() {
        panic!("zig build failed");
    }

    // Static library always lands in lib/ on all platforms.
    // Each Zig dependency (simdutf, highway, utfcpp) is also installed
    // here by its own build.zig.  Link them all.
    let lib_dir = prefix.join("lib");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    for entry in std::fs::read_dir(&lib_dir).expect("zig-out/lib/ not found") {
        let path = entry.expect("failed to read zig-out/lib/").path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        // Match "ghostty_shim.lib", "libghostty_shim.a", etc.
        let stem = if let Some(s) = name.strip_suffix(".lib") {
            s
        } else if let Some(s) = name.strip_prefix("lib").and_then(|s| s.strip_suffix(".a")) {
            s
        } else {
            continue;
        };
        println!("cargo:rustc-link-lib=static={stem}");
    }
}

fn emit_rerun_for_zig_sources(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }

            if path.extension().is_some_and(|ext| ext == "zig") {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
}
