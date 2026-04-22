const std = @import("std");
const TerminalBuildOptions = @import("ghostty/src/terminal/build_options.zig").Options;

const LibraryNames = struct {
    raw: []const u8,
    merged: []const u8,
};

const Context = struct {
    target: std.Build.ResolvedTarget,
    optimize: std.builtin.OptimizeMode,
    terminal_options: TerminalBuildOptions,
    uucode_tables: std.Build.LazyPath,
    props_output: std.Build.LazyPath,
    symbols_output: std.Build.LazyPath,
};

fn libraryNames(target: std.Build.ResolvedTarget) LibraryNames {
    if (target.result.os.tag == .windows) {
        return .{ .raw = "ghostty_shim_raw.lib", .merged = "ghostty_shim.lib" };
    }
    return .{ .raw = "libghostty_shim_raw.a", .merged = "libghostty_shim.a" };
}

fn configureGhosttyModule(b: *std.Build, module: *std.Build.Module, ctx: Context) !void {
    const build_opts = b.addOptions();
    build_opts.addOption(bool, "simd", ctx.terminal_options.simd);
    module.addOptions("build_options", build_opts);

    ctx.terminal_options.add(b, module);

    const uucode_dep = b.dependency("uucode", .{
        .target = ctx.target,
        .optimize = ctx.optimize,
        .tables_path = ctx.uucode_tables,
        .build_config_path = b.path("ghostty/src/build/uucode_config.zig"),
    });
    module.addImport("uucode", uucode_dep.module("uucode"));

    module.addAnonymousImport("unicode_tables", .{ .root_source_file = ctx.props_output });
    module.addAnonymousImport("symbols_tables", .{ .root_source_file = ctx.symbols_output });

    if (!ctx.terminal_options.simd) return;

    // Configure SIMD dependencies
    const simdutf_dep = b.dependency("simdutf", .{ .target = ctx.target, .optimize = ctx.optimize });
    const highway_dep = b.dependency("highway", .{ .target = ctx.target, .optimize = ctx.optimize });
    const utfcpp_dep = b.dependency("utfcpp", .{ .target = ctx.target, .optimize = ctx.optimize });

    const simdutf = simdutf_dep.artifact("simdutf");
    const highway = highway_dep.artifact("highway");
    const utfcpp = utfcpp_dep.artifact("utfcpp");

    module.linkLibrary(simdutf);
    module.linkLibrary(highway);
    module.linkLibrary(utfcpp);

    module.addIncludePath(b.path("ghostty/src"));

    // ziglint-ignore: Z006
    const HWY_AVX10_2: c_int = 1 << 3;
    // ziglint-ignore: Z006
    const HWY_AVX3_SPR: c_int = 1 << 4;
    // ziglint-ignore: Z006
    const HWY_AVX3_ZEN4: c_int = 1 << 6;
    // ziglint-ignore: Z006
    const HWY_AVX3_DL: c_int = 1 << 7;
    // ziglint-ignore: Z006
    const HWY_AVX3: c_int = 1 << 8;
    // ziglint-ignore: Z006
    const HWY_DISABLED_TARGETS: c_int =
        HWY_AVX10_2 | HWY_AVX3_SPR | HWY_AVX3_ZEN4 | HWY_AVX3_DL | HWY_AVX3;

    var simd_flags: std.ArrayList([]const u8) = .empty;
    defer simd_flags.deinit(b.allocator);
    try simd_flags.append(b.allocator, "-std=c++17");
    if (ctx.target.result.cpu.arch == .x86_64) {
        const flags = b.fmt("-DHWY_DISABLED_TARGETS={}", .{HWY_DISABLED_TARGETS});
        try simd_flags.append(b.allocator, flags);
    }

    module.addCSourceFiles(.{
        .files = &.{
            "ghostty/src/simd/base64.cpp",
            "ghostty/src/simd/codepoint_width.cpp",
            "ghostty/src/simd/index_of.cpp",
            "ghostty/src/simd/vt.cpp",
        },
        .flags = simd_flags.items,
    });
}

pub fn build(b: *std.Build) !void {
    const target = blk: {
        var result = b.standardTargetOptions(.{});
        // Set the target to MSVC unless a override is provided
        if (result.result.os.tag == .windows and result.query.abi == null) {
            var query = result.query;
            query.abi = .msvc;
            result = b.resolveTargetQuery(query);
        }
        break :blk result;
    };
    const optimize = b.standardOptimizeOption(.{});

    const terminal_options: TerminalBuildOptions = .{
        .artifact = .lib,
        .simd = true,
        .oniguruma = false,
        .c_abi = false,
        .version = .{ .major = 0, .minor = 0, .patch = 0 },
        .slow_runtime_safety = false,
    };

    // --- uucode dependency (two-step pattern) ---

    // Step 1: host build to get tables.zig (code generation artifact)
    const uucode_tables = blk: {
        const uucode = b.dependency("uucode", .{
            .build_config_path = b.path("ghostty/src/build/uucode_config.zig"),
            .optimize = optimize,
        });
        break :blk uucode.namedLazyPath("tables.zig");
    };

    // --- Unicode table generators (host executables) ---
    const props_exe = b.addExecutable(.{
        .name = "props-unigen",
        .root_module = b.createModule(.{
            .root_source_file = b.path("ghostty/src/unicode/props_uucode.zig"),
            .target = b.graph.host,
            .optimize = optimize,
        }),
        .use_llvm = true,
    });

    const symbols_exe = b.addExecutable(.{
        .name = "symbols-unigen",
        .root_module = b.createModule(.{
            .root_source_file = b.path("ghostty/src/unicode/symbols_uucode.zig"),
            .target = b.graph.host,
            .optimize = optimize,
        }),
        .use_llvm = true,
    });

    // Add uucode import to generators (host target)
    const host_uucode_dep = b.dependency("uucode", .{
        .target = b.graph.host,
        .optimize = optimize,
        .tables_path = uucode_tables,
        .build_config_path = b.path("ghostty/src/build/uucode_config.zig"),
    });
    inline for (&.{ props_exe, symbols_exe }) |exe| {
        exe.root_module.addImport("uucode", host_uucode_dep.module("uucode"));
    }

    // Capture generated table sources from stdout
    const props_run = b.addRunArtifact(props_exe);
    const symbols_run = b.addRunArtifact(symbols_exe);
    const wf = b.addWriteFiles();
    const props_output = wf.addCopyFile(props_run.captureStdOut(), "props.zig");
    const symbols_output = wf.addCopyFile(symbols_run.captureStdOut(), "symbols.zig");

    const ctx: Context = .{
        .target = target,
        .optimize = optimize,
        .terminal_options = terminal_options,
        .uucode_tables = uucode_tables,
        .props_output = props_output,
        .symbols_output = symbols_output,
    };

    // --- Main library ---
    const lib = b.addLibrary(.{
        .name = "ghostty_shim",
        .linkage = .static,
        .root_module = b.createModule(.{
            .root_source_file = b.path("ghostty_shim.zig"),
            .target = target,
            .optimize = optimize,
            // SIMD requires libc and libcpp
            // Skip linking libcpp because the MSVC SDK include directories
            // (added via linkLibC) contain both C and C++ headers.
            .link_libc = terminal_options.simd,
        }),
    });
    lib.bundle_compiler_rt = true;
    try configureGhosttyModule(b, lib.root_module, ctx);

    // Wire up generated unicode tables
    props_output.addStepDependencies(&lib.step);
    symbols_output.addStepDependencies(&lib.step);

    // --- Tests ---
    var test_terminal_ctx = ctx;
    test_terminal_ctx.terminal_options.slow_runtime_safety = true;

    const test_step = b.step("test", "Run Zig shim tests");
    const shim_tests = b.addExecutable(.{
        .name = "ghostty_shim_tests",
        .root_module = b.createModule(.{
            .root_source_file = b.path("tests.zig"),
            .target = target,
            .optimize = optimize,
            .link_libc = test_terminal_ctx.terminal_options.simd,
        }),
    });
    try configureGhosttyModule(b, shim_tests.root_module, test_terminal_ctx);
    test_step.dependOn(&b.addRunArtifact(shim_tests).step);

    const fmt_check = b.addFmt(.{
        // ziglint-ignore: Z024
        .paths = &.{ "src", "zconpty_shim.zig", "build.zig", "build.zig.zon" },
    });
    test_step.dependOn(&fmt_check.step);

    const zconpty_dep = b.dependency("zconpty", .{
        .target = target,
        .optimize = optimize,
    });

    const zconpty_shim = b.addLibrary(.{
        .name = "zconpty_shim",
        .linkage = .static,
        .root_module = b.createModule(.{
            .root_source_file = b.path("zconpty_shim.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    zconpty_shim.root_module.addImport("zconpty", zconpty_dep.module("zconpty"));
    try configureGhosttyModule(b, zconpty_shim.root_module, ctx);

    b.installArtifact(zconpty_shim);

    // ---- SIMD dependencies ----
    if (!terminal_options.simd) {
        b.installArtifact(lib);
        return;
    }

    // Merge all SIMD dependencies into the final library
    const simdutf_dep = b.dependency("simdutf", .{ .target = target, .optimize = optimize });
    const highway_dep = b.dependency("highway", .{ .target = target, .optimize = optimize });
    const utfcpp_dep = b.dependency("utfcpp", .{ .target = target, .optimize = optimize });

    const simdutf = simdutf_dep.artifact("simdutf");
    const highway = highway_dep.artifact("highway");
    const utfcpp = utfcpp_dep.artifact("utfcpp");

    const library_names = libraryNames(target);
    b.getInstallStep().dependOn(&b.addInstallArtifact(lib, .{
        .dest_sub_path = library_names.raw,
    }).step);

    // Use `zig ar` to merge the compiled SIMD library files.
    const archiver_path = b.findProgram(&.{"zig"}, &.{}) catch unreachable;
    const run = b.addSystemCommand(&.{archiver_path});

    run.addArg("ar");
    run.addArgs(&.{"qcsL"});

    const merged_library = run.addOutputFileArg(library_names.merged);
    run.addFileArg(lib.getEmittedBin());
    run.addFileArg(simdutf.getEmittedBin());
    run.addFileArg(highway.getEmittedBin());
    run.addFileArg(utfcpp.getEmittedBin());

    b.getInstallStep().dependOn(&b.addInstallLibFile(merged_library, library_names.merged).step);
}
