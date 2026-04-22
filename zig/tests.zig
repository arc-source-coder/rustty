const std = @import("std");
const core = @import("tests/core.zig");
const device_response = @import("tests/device_response.zig");
const harness = @import("tests/harness.zig");
const input = @import("tests/input.zig");
const modes = @import("tests/modes.zig");
const render = @import("tests/render.zig");
const scroll = @import("tests/scroll.zig");
const selection = @import("tests/selection.zig");
const support = @import("tests/support.zig");

pub fn main() !void {
    // Use a GPA so the shim-only harness still catches leaks after moving
    // away from Zig's built-in std.testing allocator.
    var gpa_state: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa_state.deinit();
    support.setAllocator(gpa_state.allocator());

    var suite: harness.Suite = .{};

    try core.run(&suite);
    try device_response.run(&suite);
    try input.run(&suite);
    try modes.run(&suite);
    try render.run(&suite);
    try scroll.run(&suite);
    try selection.run(&suite);

    try suite.finish();
}
