use std::path::PathBuf;
use std::process::{Command, Stdio};

#[test]
fn run_zig_shim_tests() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let zig_dir = manifest_dir.join("../../zig");

    let status = Command::new("zig")
        .current_dir(&zig_dir)
        .arg("build")
        .arg("test")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .expect("failed to invoke `zig build test`");

    assert!(status.success(), "zig build test failed");
}
