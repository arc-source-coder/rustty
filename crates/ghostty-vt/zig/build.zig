const std = @import("std");
const TerminalBuildOptions = @import("ghostty/src/terminal/build_options.zig").Options;

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const terminal_options: TerminalBuildOptions = .{
        .artifact = .lib,
        .simd = false,
        .oniguruma = false,
        .c_abi = false,
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
        .root_module = b.createModule(.{
            .root_source_file = b.path("lib.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

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

    b.installArtifact(lib);

    // --- Tests ---

    const test_step = b.step("test", "Run unit tests");
    const lib_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("lib.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    test_step.dependOn(&b.addRunArtifact(lib_tests).step);

    const fmt_check = b.addFmt(.{ .paths = &.{
        "lib.zig", "handle.zig", "modes.zig",
        "render.zig", "scroll.zig",
        "build.zig", "build.zig.zon"
    } });
    test_step.dependOn(&fmt_check.step);
}
