use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    #[cfg(target_os = "windows")]
    compile_shaders();
}

fn compile_shaders() {
    let shader_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("src/shaders");
    let vs_path = shader_dir.join("shader_vs.hlsl");
    let ps_path = shader_dir.join("shader_ps.hlsl");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    println!("cargo:rerun-if-changed={}", shader_dir.display());

    let fxc_path = find_fxc_compiler();

    let rust_binding_path = out_dir.join("renderer_shaders_bytes.rs");
    if rust_binding_path.exists() {
        fs::remove_file(&rust_binding_path).expect("failed to remove existing shader bindings");
    }

    compile_shader(
        &fxc_path,
        vs_path.to_str().unwrap(),
        shader_dir.as_path(),
        &out_dir,
        &rust_binding_path,
        "shader_vs",
        "vs_4_1",
        "main",
        "RENDERER_VERTEX_BYTES",
        "renderer_vs.cso",
    );
    compile_shader(
        &fxc_path,
        ps_path.to_str().unwrap(),
        shader_dir.as_path(),
        &out_dir,
        &rust_binding_path,
        "shader_ps_bg",
        "ps_4_1",
        "backgroundMain",
        "RENDERER_BG_FRAGMENT_BYTES",
        "renderer_background_ps.cso",
    );
    compile_shader(
        &fxc_path,
        ps_path.to_str().unwrap(),
        shader_dir.as_path(),
        &out_dir,
        &rust_binding_path,
        "shader_ps_fg",
        "ps_4_1",
        "foregroundMain",
        "RENDERER_FG_FRAGMENT_BYTES",
        "renderer_foreground_ps.cso",
    );
}

fn find_fxc_compiler() -> String {
    if let Ok(path) = std::env::var("GPUI_FXC_PATH")
        && Path::new(&path).exists()
    {
        return path;
    }

    if let Ok(output) = Command::new("where.exe").arg("fxc.exe").output()
        && output.status.success()
        && let Some(path) = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
    {
        return path.to_owned();
    }

    if let Some(path) = find_windows_sdk_binary("fxc.exe") {
        return path.to_string_lossy().into_owned();
    }

    panic!("failed to find fxc.exe; set GPUI_FXC_PATH to the shader compiler");
}

fn find_windows_sdk_binary(binary: &str) -> Option<std::path::PathBuf> {
    let base = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => return None,
    };

    let mut versions: Vec<_> = std::fs::read_dir(&base)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    versions.sort_by_key(|s| {
        s.split('.')
            .filter_map(|part| part.parse::<u32>().ok())
            .collect::<Vec<_>>()
    });

    versions
        .into_iter()
        .rev()
        .map(|version| base.join(version).join(arch).join(binary))
        .find(|path| path.exists())
}

#[allow(clippy::too_many_arguments)]
fn compile_shader(
    fxc_path: &str,
    shader_path: &str,
    include_dir: &Path,
    out_dir: &Path,
    rust_binding_path: &Path,
    module: &str,
    profile: &str,
    entry_point: &str,
    const_name: &str,
    output_name: &str,
) {
    let output_path = out_dir.join(output_name);
    let mut args: Vec<&str> = vec![
        "/T",
        profile,
        "/E",
        entry_point,
        "/Fo",
        output_path.to_str().unwrap(),
        "/I",
        include_dir.to_str().unwrap(),
        "/Ges",
        "/WX",
    ];
    args.extend(["/O3", "/Qstrip_debug", "/Qstrip_reflect"]);
    args.push(shader_path);

    let status = Command::new(fxc_path)
        .args(args)
        .status()
        .unwrap_or_else(|err| panic!("failed to compile {module} shader with fxc.exe: {err}"));

    assert!(
        status.success(),
        "fxc.exe failed compiling {module} shader entry {entry_point}"
    );

    let mut rust_bindings = OpenOptions::new()
        .create(true)
        .append(true)
        .open(rust_binding_path)
        .expect("failed to open renderer shader binding output");
    writeln!(
        rust_bindings,
        "pub const {const_name}: &[u8] = include_bytes!(r#\"{}\"#);",
        output_path.display()
    )
    .expect("failed to write renderer shader binding");
}
