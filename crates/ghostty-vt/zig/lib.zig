const std = @import("std");
const terminal = @import("ghostty/src/terminal/main.zig");
const Allocator = std.mem.Allocator;

// --- Callback function pointer types ---
const BellCallback = *const fn (?*anyopaque) callconv(.c) void;
const TitleCallback = *const fn (?*anyopaque, [*]const u8, usize) callconv(.c) void;

const Callbacks = struct {
    userdata: ?*anyopaque = null,
    bell: ?BellCallback = null,
    title: ?TitleCallback = null,
};

// --- ShimHandler: wraps ReadonlyHandler + fires callbacks ---
const ShimHandler = struct {
    readonly: terminal.ReadonlyHandler,
    callbacks: *Callbacks,

    const Action = terminal.StreamAction;

    pub fn deinit(self: *ShimHandler) void {
        self.readonly.deinit();
        self.* = undefined;
    }

    pub fn vt(
        self: *ShimHandler,
        comptime action: Action.Tag,
        value: Action.Value(action),
    ) !void {
        // Delegate ALL state mutation to ReadonlyHandler.
        // ReadonlyHandler no-ops on side-effect actions, so this is safe.
        try self.readonly.vt(action, value);

        // Intercept side-effect actions that ReadonlyHandler ignores.
        switch (action) {
            .bell => {
                if (self.callbacks.bell) |cb| cb(self.callbacks.userdata);
            },
            .window_title => {
                if (self.callbacks.title) |cb|
                    cb(self.callbacks.userdata, value.title.ptr, value.title.len);
            },
            else => {},
        }
    }
};

// --- TerminalHandle: owns all terminal state ---
const TerminalHandle = struct {
    alloc: Allocator,
    terminal_inst: terminal.Terminal,
    stream: terminal.Stream(*ShimHandler),
    handler: ShimHandler,
    callbacks: Callbacks,
    render_state: terminal.RenderState,

    fn init(alloc: Allocator, cols: u16, rows: u16) !*TerminalHandle {
        const handle = try alloc.create(TerminalHandle);
        errdefer alloc.destroy(handle);

        const t = try terminal.Terminal.init(alloc, .{
            .cols = cols,
            .rows = rows,
        });
        errdefer {
            var tmp = t;
            tmp.deinit(alloc);
        }

        handle.* = .{
            .alloc = alloc,
            .terminal_inst = t,
            .callbacks = .{},
            .handler = .{
                .readonly = .{ .terminal = undefined },
                .callbacks = undefined,
            },
            .stream = undefined,
            .render_state = .empty,
        };

        // Fix up self-referential pointers
        handle.handler.readonly.terminal = &handle.terminal_inst;
        handle.handler.callbacks = &handle.callbacks;
        handle.stream = terminal.Stream(*ShimHandler).initAlloc(alloc, &handle.handler);

        return handle;
    }

    fn deinit(self: *TerminalHandle) void {
        const alloc = self.alloc;
        self.render_state.deinit(alloc);
        self.stream.deinit();
        self.terminal_inst.deinit(alloc);
        self.* = undefined;
        alloc.destroy(self);
    }
};

export fn ghostty_vt_terminal_new(cols: u16, rows: u16) callconv(.c) ?*anyopaque {
    const alloc = std.heap.smp_allocator;
    const handle = TerminalHandle.init(alloc, cols, rows) catch return null;
    return @ptrCast(handle);
}

export fn ghostty_vt_terminal_free(ptr: ?*anyopaque) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.deinit();
}

export fn ghostty_vt_terminal_set_callbacks(
    ptr: ?*anyopaque,
    userdata: ?*anyopaque,
    bell: ?BellCallback,
    title: ?TitleCallback,
) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.callbacks = .{
        .userdata = userdata,
        .bell = bell,
        .title = title,
    };
}

export fn ghostty_vt_terminal_feed(
    ptr: ?*anyopaque,
    bytes: [*]const u8,
    len: usize,
) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.stream.nextSlice(bytes[0..len]) catch return 2;
    return 0;
}

export fn ghostty_vt_terminal_resize(
    ptr: ?*anyopaque,
    cols: u16,
    rows: u16,
) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.terminal_inst.resize(handle.alloc, cols, rows) catch return 2;
    return 0;
}
