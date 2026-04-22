const std = @import("std");
const harness = @import("harness.zig");
const support = @import("support.zig");

const modes = support.modes;

fn test_alternate_screen() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    try std.testing.expect(!modes.ghostty_terminal_is_alternate_screen(handle));
    // CSI ?1049h enters alternate screen.
    support.feed(handle, "\x1b[?1049h");
    try std.testing.expect(modes.ghostty_terminal_is_alternate_screen(handle));
    // CSI ?1049l returns to the primary screen.
    support.feed(handle, "\x1b[?1049l");
    try std.testing.expect(!modes.ghostty_terminal_is_alternate_screen(handle));
}

pub fn run(suite: *harness.Suite) !void {
    try suite.run("modes: test_alternate_screen", test_alternate_screen);
}
