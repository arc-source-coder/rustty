use std::path::{Path, PathBuf};

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS is set");
    if target_os != "windows" {
        println!("cargo:warning=harfbuzz C++ build is only enabled for Windows targets");
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("unexpected crates/harfbuzz layout");
    let harfbuzz_dir = workspace_root.join("vendor/harfbuzz");
    let src_dir = harfbuzz_dir.join("src");
    let amalgamation = src_dir.join("harfbuzz-world.cc");

    assert!(
        src_dir.exists(),
        "vendor/harfbuzz/src is missing; run `git submodule update --init --recursive`"
    );

    println!("cargo:rerun-if-changed={}", src_dir.display());

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .include(&src_dir)
        .warnings(false)
        .file(amalgamation)
        .define("HB_HAS_DIRECTWRITE", "1")
        .define("HAVE_DIRECTWRITE", "1")
        .define("HB_NO_AAT", "1")
        .define("HB_NO_FALLBACK_SHAPE", "1")
        .define("HB_NO_NAME", "1")
        .define("HB_NO_OT_FONT_GLYPH_NAMES", "1")
        .define("HB_NO_LEGACY", "1")
        .define("HB_NO_DRAW", "1")
        .define("HB_NO_PAINT", "1")
        .define("HB_NO_BITMAP", "1")
        .define("HB_NDEBUG", "1")
        .define("HB_DISABLE_DEPRECATED", "1")
        .define("NDEBUG", "1");

    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    assert_eq!(
        target_env, "msvc",
        "HarfBuzz is only built for Windows MSVC targets"
    );

    let target_triple = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86_64-pc-windows-msvc",
        Ok("aarch64") => "aarch64-pc-windows-msvc",
        arch => panic!("unsupported HarfBuzz target architecture: {arch:?}"),
    };

    build
        .compiler(std::env::var("HARFBUZZ_CXX").unwrap_or_else(|_| "clang.exe".to_owned()))
        .archiver(std::env::var("HARFBUZZ_AR").unwrap_or_else(|_| "llvm-lib.exe".to_owned()))
        .no_default_flags(true)
        .inherit_rustflags(false)
        .flag("--driver-mode=cl")
        .flag(format!("--target={target_triple}"))
        .flag("-O3")
        .flag("-flto=full")
        .flag("-fno-rtti")
        .flag("-fno-exceptions")
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        .flag("-fno-ident")
        .flag("-std=c++17");

    build.compile("harfbuzz");
    println!("cargo:rustc-link-lib=dwrite");
}
