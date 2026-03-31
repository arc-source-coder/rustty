const std = @import("std");
const TerminalBuildOptions = @import("ghostty/src/terminal/build_options.zig").Options;

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
        });
        break :blk uucode.namedLazyPath("tables.zig");
    };

    // --- Unicode table generators (host executables) ---

    const props_exe = b.addExecutable(.{
        .name = "props-unigen",
        .root_module = b.createModule(.{
            .root_source_file = b.path("ghostty/src/unicode/props_uucode.zig"),
            .target = b.graph.host,
        }),
        .use_llvm = true,
    });

    const symbols_exe = b.addExecutable(.{
        .name = "symbols-unigen",
        .root_module = b.createModule(.{
            .root_source_file = b.path("ghostty/src/unicode/symbols_uucode.zig"),
            .target = b.graph.host,
        }),
        .use_llvm = true,
    });

    // Add uucode import to generators (host target)
    if (b.lazyDependency("uucode", .{
        .target = b.graph.host,
        .tables_path = uucode_tables,
        .build_config_path = b.path("ghostty/src/build/uucode_config.zig"),
    })) |dep| {
        inline for (&.{ props_exe, symbols_exe }) |exe| {
            exe.root_module.addImport("uucode", dep.module("uucode"));
        }
    }

    // Capture generated table sources from stdout
    const props_run = b.addRunArtifact(props_exe);
    const symbols_run = b.addRunArtifact(symbols_exe);
    const wf = b.addWriteFiles();
    const props_output = wf.addCopyFile(props_run.captureStdOut(), "props.zig");
    const symbols_output = wf.addCopyFile(symbols_run.captureStdOut(), "symbols.zig");

    // --- Main library ---

    const lib = b.addLibrary(.{
        .name = "ghostty_shim",
        .linkage = .static,
        .root_module = b.createModule(.{
            .root_source_file = b.path("lib.zig"),
            .target = target,
            .optimize = optimize,
            // SIMD requires libc and libcpp
            // Skip linking libcpp because the MSVC SDK include directories
            // (added via linkLibC) contain both C and C++ headers.
            .link_libc = terminal_options.simd,
        }),
    });

    // Add build_options (simd code expects this module name)
    const build_opts = b.addOptions();
    build_opts.addOption(bool, "simd", terminal_options.simd);
    lib.root_module.addOptions("build_options", build_opts);

    terminal_options.add(b, lib.root_module);
    lib.bundle_compiler_rt = true;

    // Step 2: target build with tables_path to get the uucode module
    if (b.lazyDependency("uucode", .{
        .target = target,
        .optimize = optimize,
        .tables_path = uucode_tables,
        .build_config_path = b.path("ghostty/src/build/uucode_config.zig"),
    })) |dep| {
        lib.root_module.addImport("uucode", dep.module("uucode"));
    }

    // Wire up generated unicode tables
    props_output.addStepDependencies(&lib.step);
    symbols_output.addStepDependencies(&lib.step);
    lib.root_module.addAnonymousImport("unicode_tables", .{
        .root_source_file = props_output,
    });
    lib.root_module.addAnonymousImport("symbols_tables", .{
        .root_source_file = symbols_output,
    });

    // --- SIMD dependencies ---
    if (terminal_options.simd) {
        // Add include path for simd headers
        lib.root_module.addIncludePath(b.path("ghostty/src"));

        // Disable AVX512 to work around Zig 0.13 bug:
        // https://github.com/ziglang/zig/issues/20414
        const HWY_AVX10_2: c_int = 1 << 3;
        const HWY_AVX3_SPR: c_int = 1 << 4;
        const HWY_AVX3_ZEN4: c_int = 1 << 6;
        const HWY_AVX3_DL: c_int = 1 << 7;
        const HWY_AVX3: c_int = 1 << 8;
        const HWY_DISABLED_TARGETS: c_int = HWY_AVX10_2 | HWY_AVX3_SPR | HWY_AVX3_ZEN4 | HWY_AVX3_DL | HWY_AVX3;

        // MSVC requires explicit std specification otherwise SIMD C++17
        // features are guarded. Doing it unconditionally is harmless.
        var simd_flags: std.ArrayList([]const u8) = .empty;
        defer simd_flags.deinit(b.allocator);
        try simd_flags.append(b.allocator, "-std=c++17");
        if (target.result.cpu.arch == .x86_64) {
            try simd_flags.append(
                b.allocator,
                b.fmt("-DHWY_DISABLED_TARGETS={}", .{HWY_DISABLED_TARGETS}),
            );
        }

        lib.root_module.addCSourceFiles(.{
            .files = &.{
                "ghostty/src/simd/base64.cpp",
                "ghostty/src/simd/codepoint_width.cpp",
                "ghostty/src/simd/index_of.cpp",
                "ghostty/src/simd/vt.cpp",
            },
            .flags = simd_flags.items,
        });

        if (b.lazyDependency("simdutf", .{
            .target = target,
            .optimize = optimize,
        })) |dep| {
            const artifact = dep.artifact("simdutf");
            lib.root_module.linkLibrary(artifact);
            b.installArtifact(artifact);
        }

        if (b.lazyDependency("highway", .{
            .target = target,
            .optimize = optimize,
        })) |dep| {
            const artifact = dep.artifact("highway");
            lib.root_module.linkLibrary(artifact);
            b.installArtifact(artifact);
        }

        if (b.lazyDependency("utfcpp", .{
            .target = target,
            .optimize = optimize,
        })) |dep| {
            const artifact = dep.artifact("utfcpp");
            lib.root_module.linkLibrary(artifact);
            b.installArtifact(artifact);
        }
    }

    b.installArtifact(lib);

    // --- Tests ---

    const test_step = b.step("test", "Run unit tests");
    const lib_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("lib.zig"),
            .target = target,
            .optimize = optimize,
            // SIMD requires libc and libcpp
            // Skip libcpp since we use MSVC (same as main library)
            .link_libc = terminal_options.simd,
        }),
    });
    test_step.dependOn(&b.addRunArtifact(lib_tests).step);

    const fmt_check = b.addFmt(.{ .paths = &.{ "lib.zig", "handle.zig", "modes.zig", "render.zig", "scroll.zig", "build.zig", "build.zig.zon" } });
    test_step.dependOn(&fmt_check.step);
}
