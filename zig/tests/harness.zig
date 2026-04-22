const std = @import("std");

fn labelPass() []const u8 {
    if (std.fs.File.stderr().isTty()) {
        return "\x1b[32mPASS\x1b[0m";
    }

    return "PASS";
}

fn labelFail() []const u8 {
    if (std.fs.File.stderr().isTty()) {
        return "\x1b[31mFAIL\x1b[0m";
    }

    return "FAIL";
}

pub const Suite = struct {
    passed_count: usize = 0,
    failed_count: usize = 0,

    pub fn run(self: *Suite, name: []const u8, test_fn: *const fn () anyerror!void) !void {
        test_fn() catch |err| {
            self.failed_count += 1;
            std.debug.print("{s} {s}: {s}\n", .{ labelFail(), name, @errorName(err) });
            return;
        };

        self.passed_count += 1;
        std.debug.print("{s} {s}\n", .{ labelPass(), name });
    }

    pub fn finish(self: *const Suite) !void {
        const total_count = self.passed_count + self.failed_count;
        std.debug.print(
            "\n{d} passed, {d} failed, {d} total\n",
            .{ self.passed_count, self.failed_count, total_count },
        );

        if (self.failed_count != 0) {
            return error.TestFailure;
        }
    }
};
