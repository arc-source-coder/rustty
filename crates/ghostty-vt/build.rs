use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("zig/build.zig").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("zig/build.zig.zon").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("zig/lib.zig").display()
    );
    emit_rerun_for_zig_sources(&manifest_dir.join("zig"));

    let ghostty_dir = manifest_dir.join("zig/ghostty");
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
        .current_dir(manifest_dir.join("zig"))
        .arg("build")
        .arg("-Doptimize=ReleaseFast")
        .arg("--prefix")
        .arg(&prefix)
        .status()
        .expect("failed to invoke zig");
    if !status.success() {
        panic!("zig build failed");
    }

    // On Windows, Zig follows the platform convention:
    //   bin/  — runtime files (.dll)
    //   lib/  — link-time stubs (.lib import libs)
    // On non-Windows both land in lib/.
    let (dll_dir, lib_dir) = if cfg!(target_os = "windows") {
        (prefix.join("bin"), prefix.join("lib"))
    } else {
        (prefix.join("lib"), prefix.join("lib"))
    };

    // rust-lld needs the import stub (.lib), which is always in lib/.
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=ghostty_shim");

    // Copy the .dll next to the final binary.
    copy_dll_to_cargo_output(&dll_dir, &out_dir);
}

/// Copy ghostty_shim.dll (and ghostty_shim.pdb if present) to target/{debug|release}/
/// dll_dir: where Zig placed the .dll  (zig-out/bin/ on Windows)
fn copy_dll_to_cargo_output(dll_dir: &Path, out_dir: &Path) {
    let dll_name = "ghostty_shim.dll";
    let src = dll_dir.join(dll_name);

    if !src.exists() {
        panic!(
            "Expected {dll_name} at {src:?} — \
             check that build.zig uses linkage = .dynamic \
             and that `zig build` completed successfully"
        );
    }

    // OUT_DIR = .../target/{profile}/build/{crate}-{hash}/out
    // parent()  = .../target/{profile}/build/{crate}-{hash}/
    // parent()  = .../target/{profile}/build/
    // parent()  = .../target/{profile}/           <-- where the exe lives
    let profile_dir = out_dir
        .parent() // drop "out"
        .and_then(|p| p.parent()) // drop "{crate}-{hash}"
        .and_then(|p| p.parent()) // drop "build"
        .expect("OUT_DIR did not have the expected depth inside target/");

    // Copy to the two locations Cargo uses for runnable binaries:
    //   target/{profile}/ — `cargo run`, final app binary
    //   target/{profile}/deps/ — `cargo test`, dependency test harnesses
    let destinations: &[_] = &[profile_dir.join(dll_name), profile_dir.join("deps").join(dll_name)];

    for dst in destinations {
        if let Some(parent) = dst.parent()
            && !parent.exists()
        {
            continue; // deps/ may not exist yet for top-level-only builds
        }

        std::fs::copy(&src, dst).unwrap_or_else(|e| {
            panic!("Failed to copy {src:?} -> {dst:?}: {e}");
        });
    }

    // Copy the .pdb (debug symbols) if Zig produced one.
    // Without it, crash stack traces show only raw addresses
    // for anything occuring inside the Zig shim.
    let pdb_name = "ghostty_shim.pdb";
    let pdb_src = dll_dir.join(pdb_name);
    if pdb_src.exists() {
        for dst in destinations {
            let pdb_dst = dst.with_file_name(pdb_name);
            let _ = std::fs::copy(&pdb_src, &pdb_dst); // best-effort
        }
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
