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
    assert!(
        ghostty_dir.exists(),
        "zig/ghostty is missing; run `git submodule update --init --recursive` and retry"
    );

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
    assert!(zig_version.is_some(), "Zig 0.15.2 is required");

    let os = std::env::var("CARGO_CFG_TARGET_OS");
    let os_mismatch_error = "Ghostty shim only supports Windows targets";
    assert!(os.as_deref() == Ok("windows"), "{}", os_mismatch_error);

    let zig_arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86_64",
        Ok("aarch64") => "aarch64",
        arch => panic!("unsupported target architecture: {arch:?}"),
    };

    let zig_env = match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => "msvc",
        Ok("gnu") => panic!("Ghostty shim only supports MSVC targets"),
        env => panic!("unsupported target environment: {env:?}"),
    };

    let zig_target = format!("{zig_arch}-windows-{zig_env}");
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
    assert!(status.success(), "zig build failed");

    let lib_dir = prefix.join("lib");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=ghostty_shim");
}
