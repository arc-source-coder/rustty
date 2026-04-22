const std = @import("std");
const windows = @import("windows.zig");

// Signals used to control the ConPTY via the signal pipe
const PTY_SIGNAL_RESIZE_WINDOW: u16 = 8;

// CreatePseudoConsole Flags
const PSEUDOCONSOLE_INHERIT_CURSOR = 0x1;
const PSEUDOCONSOLE_GLYPH_WIDTH__MASK = 0x18;
const PSEUDOCONSOLE_GLYPH_WIDTH_GRAPHEMES = 0x08;
const PSEUDOCONSOLE_GLYPH_WIDTH_WCSWIDTH = 0x10;
const PSEUDOCONSOLE_GLYPH_WIDTH_CONSOLE = 0x18;
const PSEUDOCONSOLE_AMBIGUOUS_IS_WIDE = 0x20;

const alloc = std.heap.smp_allocator;

const ConsoleHostPath = struct {
    wide: [:0]u16,
    utf8: [:0]u8,

    fn deinit(self: ConsoleHostPath, allocator: std.mem.Allocator) void {
        allocator.free(self.wide);
        allocator.free(self.utf8);
    }
};

const extended_path_prefix = std.unicode.utf8ToUtf16LeStringLiteral("\\\\?\\");

fn ensureDriverIsLoaded() void {
    var info: windows.SYSTEM_CONSOLE_INFORMATION = .{ .DriverLoaded = 1 };
    _ = windows.NtSetSystemInformation(
        windows.SYSTEM_CONSOLE_INFORMATION_CLASS,
        &info,
        @sizeOf(windows.SYSTEM_CONSOLE_INFORMATION),
    );
}

fn consoleHostPath() !ConsoleHostPath {
    return resolveConsoleHostPath(alloc);
}

fn resolveConsoleHostPath(allocator: std.mem.Allocator) !ConsoleHostPath {
    // Get the executable's location
    const self_path = try std.fs.selfExePathAlloc(allocator);
    defer allocator.free(self_path);

    // Find the directory where the executable is located.
    const dir = std.fs.path.dirname(self_path) orelse return inboxConsoleHostPath(allocator);

    // Check for OpenConsole.exe next to the executable
    const open_console_path = try std.fs.path.joinZ(allocator, &.{ dir, "OpenConsole.exe" });
    errdefer allocator.free(open_console_path);

    const open_console_path_w = try std.unicode.wtf8ToWtf16LeAllocZ(allocator, open_console_path);
    errdefer allocator.free(open_console_path_w);

    const attrs = windows.GetFileAttributesW(open_console_path_w.ptr);
    if (attrs == windows.INVALID_FILE_ATTRIBUTES) {
        // OpenConsole.exe not found - use conhost.exe instead.
        allocator.free(open_console_path_w);
        allocator.free(open_console_path);
        return inboxConsoleHostPath(allocator);
    }

    return .{
        .wide = open_console_path_w,
        .utf8 = open_console_path,
    };
}

fn inboxConsoleHostPath(allocator: std.mem.Allocator) !ConsoleHostPath {
    const conhost_exe_suffix = std.unicode.utf8ToUtf16LeStringLiteral("\\conhost.exe");

    const sysdir = windows.getSystemDirectoryWtf16Le(); // e.g. C:\Windows\System32
    const total_len = extended_path_prefix.len + sysdir.len + conhost_exe_suffix.len;

    const out = try allocator.alloc(u16, total_len + 1);
    @memcpy(out[0..extended_path_prefix.len], extended_path_prefix);
    @memcpy(out[extended_path_prefix.len .. extended_path_prefix.len + sysdir.len], sysdir);
    @memcpy(out[extended_path_prefix.len + sysdir.len .. total_len], conhost_exe_suffix);

    out[total_len] = 0;

    errdefer allocator.free(out);
    const utf8 = try std.unicode.wtf16LeToWtf8AllocZ(allocator, out[0..total_len]);

    return .{
        .wide = out[0..total_len :0],
        .utf8 = utf8,
    };
}

export fn ptyCreate(
    size: windows.COORD,
    h_input: windows.HANDLE,
    h_output: windows.HANDLE,
    dw_flags: windows.DWORD,
    command_line: ?windows.LPWSTR,
    current_directory: ?windows.LPCWSTR,
    environment: ?*anyopaque,
    creation_flags: windows.DWORD,
    out_hpcon: *isize,
    out_child_process: *windows.HANDLE,
) callconv(.c) windows.HRESULT {
    if (command_line == null) return windows.E_INVALIDARG;
    if (!handleIsValid(h_input) or !handleIsValid(h_output)) return windows.E_INVALIDARG;

    out_hpcon.* = 0;
    out_child_process.* = windows.INVALID_HANDLE_VALUE;

    var duplicated_input: windows.HANDLE = windows.INVALID_HANDLE_VALUE;
    var duplicated_output: windows.HANDLE = windows.INVALID_HANDLE_VALUE;
    defer {
        if (handleIsValid(duplicated_input)) _ = windows.NtClose(duplicated_input);
        if (handleIsValid(duplicated_output)) _ = windows.NtClose(duplicated_output);
    }

    const current_process = windows.GetCurrentProcess();
    if (windows.DuplicateHandle(
        current_process,
        h_input,
        current_process,
        &duplicated_input,
        0,
        .TRUE,
        windows.DUPLICATE_SAME_ACCESS,
    ) == .FALSE) {
        return hresultFromWin32(windows.GetLastError());
    }
    if (windows.DuplicateHandle(
        current_process,
        h_output,
        current_process,
        &duplicated_output,
        0,
        .TRUE,
        windows.DUPLICATE_SAME_ACCESS,
    ) == .FALSE) {
        return hresultFromWin32(windows.GetLastError());
    }

    const pty = alloc.create(windows.PseudoConsole) catch return windows.E_OUTOFMEMORY;
    errdefer alloc.destroy(pty);
    pty.* = .{
        .hSignal = windows.INVALID_HANDLE_VALUE,
        .hPtyReference = windows.INVALID_HANDLE_VALUE,
        .hConPtyProcess = windows.INVALID_HANDLE_VALUE,
    };

    const hr = createPseudoConsole(
        windows.INVALID_HANDLE_VALUE,
        size,
        duplicated_input,
        duplicated_output,
        dw_flags,
        pty,
    );
    if (hr != windows.S_OK) return hr;
    errdefer closePseudoConsole(pty);

    var child_process: windows.HANDLE = windows.INVALID_HANDLE_VALUE;
    const spawn_hr = spawnPtyChildProcess(
        pty,
        command_line.?,
        current_directory,
        environment,
        creation_flags,
        &child_process,
    );
    if (spawn_hr != windows.S_OK) return spawn_hr;

    out_child_process.* = child_process;
    out_hpcon.* = @intCast(@intFromPtr(pty));
    return windows.S_OK;
}

export fn ptyRelease(hpcon: isize) callconv(.c) windows.HRESULT {
    if (hpcon == 0) return windows.E_INVALIDARG;

    const pty: *windows.PseudoConsole = @ptrFromInt(@as(usize, @intCast(hpcon)));
    // Releasing the reference lets the console host exit once the last client disconnects.
    if (handleIsValid(pty.hPtyReference)) {
        _ = windows.NtClose(pty.hPtyReference);
        pty.hPtyReference = windows.INVALID_HANDLE_VALUE;
    }

    return windows.S_OK;
}

export fn ptyResize(hpcon: isize, size: windows.COORD) callconv(.c) windows.HRESULT {
    if (hpcon == 0) return windows.E_INVALIDARG;
    const pty: *const windows.PseudoConsole = @ptrFromInt(@as(usize, @intCast(hpcon)));
    return resizePseudoConsole(pty, size);
}

export fn ptyClose(hpcon: isize) callconv(.c) void {
    if (hpcon == 0) return;
    const pty: *windows.PseudoConsole = @ptrFromInt(@as(usize, @intCast(hpcon)));
    closePseudoConsole(pty);
    alloc.destroy(pty);
}

fn spawnPtyChildProcess(
    pty: *windows.PseudoConsole,
    command_line: windows.LPWSTR,
    current_directory: ?windows.LPCWSTR,
    environment: ?*anyopaque,
    creation_flags: windows.DWORD,
    out_child_process: *windows.HANDLE,
) windows.HRESULT {
    var startup_info_ex: windows.STARTUPINFOEXW = .{
        .StartupInfo = std.mem.zeroes(windows.STARTUPINFOW),
        .lpAttributeList = null,
    };
    startup_info_ex.StartupInfo.cb = @sizeOf(windows.STARTUPINFOEXW);
    startup_info_ex.StartupInfo.dwFlags |= windows.STARTF_USESTDHANDLES;

    var attr_list_size: windows.SIZE_T = 0;
    _ = windows.InitializeProcThreadAttributeList(null, 1, 0, &attr_list_size);

    const attr_list_mem = alloc.alloc(u8, attr_list_size) catch return windows.E_OUTOFMEMORY;
    defer alloc.free(attr_list_mem);

    startup_info_ex.lpAttributeList = @ptrCast(attr_list_mem.ptr);
    if (windows.InitializeProcThreadAttributeList(startup_info_ex.lpAttributeList, 1, 0, &attr_list_size) == .FALSE) {
        return hresultFromWin32(windows.GetLastError());
    }
    defer windows.DeleteProcThreadAttributeList(startup_info_ex.lpAttributeList);

    const hpcon_value_ptr: *anyopaque = @ptrFromInt(@as(usize, @intCast(@intFromPtr(pty))));
    if (windows.UpdateProcThreadAttribute(
        startup_info_ex.lpAttributeList,
        0,
        windows.PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
        hpcon_value_ptr,
        @sizeOf(isize),
        null,
        null,
    ) == .FALSE) {
        return hresultFromWin32(windows.GetLastError());
    }

    const extended_startupinfo_present: windows.DWORD = @bitCast(windows.CreateProcessFlags{
        .extended_startupinfo_present = true,
    });
    const full_creation_flags = creation_flags | extended_startupinfo_present;

    var process_info: windows.PROCESS_INFORMATION = undefined;
    if (windows.CreateProcessW(
        null,
        command_line,
        null,
        null,
        .FALSE,
        full_creation_flags,
        environment,
        current_directory,
        &startup_info_ex.StartupInfo,
        &process_info,
    ) == .FALSE) {
        return hresultFromWin32(windows.GetLastError());
    }

    _ = windows.NtClose(process_info.hThread);
    out_child_process.* = process_info.hProcess;
    return windows.S_OK;
}

fn createPseudoConsole(
    h_token: windows.HANDLE,
    size: windows.COORD,
    h_input: windows.HANDLE,
    h_output: windows.HANDLE,
    dw_flags: windows.DWORD,
    p_pty: ?*windows.PseudoConsole,
) callconv(.winapi) windows.HRESULT {
    const pty = p_pty orelse return windows.E_INVALIDARG;
    if (size.X == 0 or size.Y == 0) return windows.E_INVALIDARG;

    const token: ?windows.HANDLE = if (h_token == windows.INVALID_HANDLE_VALUE) null else h_token;

    var server_handle: windows.HANDLE = windows.INVALID_HANDLE_VALUE;
    var reference_handle: windows.HANDLE = windows.INVALID_HANDLE_VALUE;
    var signal_pipe_conhost_side: windows.HANDLE = windows.INVALID_HANDLE_VALUE;
    var signal_pipe_our_side: windows.HANDLE = windows.INVALID_HANDLE_VALUE;

    errdefer {
        if (handleIsValid(signal_pipe_our_side)) _ = windows.NtClose(signal_pipe_our_side);
        if (handleIsValid(signal_pipe_conhost_side)) _ = windows.NtClose(signal_pipe_conhost_side);
        if (handleIsValid(reference_handle)) _ = windows.NtClose(reference_handle);
        if (handleIsValid(server_handle)) _ = windows.NtClose(server_handle);
    }

    var status = createServerHandle(&server_handle, .TRUE);
    if (!ntSuccess(status)) {
        // ConDrv can be lazily loaded; mirror WT by retrying once after requesting load.
        ensureDriverIsLoaded();
        status = createServerHandle(&server_handle, .TRUE);
        if (!ntSuccess(status)) return hresultFromNt(status);
    }

    const reference_handle_name = std.unicode.utf8ToUtf16LeStringLiteral("\\Reference");
    status = createClientHandle(&reference_handle, server_handle, reference_handle_name, .FALSE);
    if (!ntSuccess(status)) return hresultFromNt(status);

    var security_attributes: windows.SECURITY_ATTRIBUTES = .{
        .nLength = @sizeOf(windows.SECURITY_ATTRIBUTES),
        .lpSecurityDescriptor = null,
        .bInheritHandle = windows.FALSE,
    };

    windows.CreatePipe(
        &signal_pipe_conhost_side,
        &signal_pipe_our_side,
        &security_attributes,
    ) catch {
        return hresultFromWin32(windows.GetLastError());
    };
    windows.SetHandleInformation(
        signal_pipe_conhost_side,
        windows.HANDLE_FLAG_INHERIT,
        windows.HANDLE_FLAG_INHERIT,
    ) catch {
        return hresultFromWin32(windows.GetLastError());
    };

    const inherit = (dw_flags & PSEUDOCONSOLE_INHERIT_CURSOR) != 0;
    const inherit_cursor = if (inherit) "--inheritcursor " else "";
    const is_wide = (dw_flags & PSEUDOCONSOLE_AMBIGUOUS_IS_WIDE) != 0;
    const ambiguous_is_wide = if (is_wide) "--ambiguousIsWide " else "";

    const text_measurement = switch (dw_flags & PSEUDOCONSOLE_GLYPH_WIDTH__MASK) {
        PSEUDOCONSOLE_GLYPH_WIDTH_GRAPHEMES => "--textMeasurement graphemes ",
        PSEUDOCONSOLE_GLYPH_WIDTH_WCSWIDTH => "--textMeasurement wcswidth ",
        PSEUDOCONSOLE_GLYPH_WIDTH_CONSOLE => "--textMeasurement console ",
        else => "",
    };

    const conhost_path = consoleHostPath() catch return windows.E_OUTOFMEMORY;
    defer conhost_path.deinit(alloc);

    const cmd_utf8 = std.fmt.allocPrint(
        alloc,
        "\"{s}\" --headless {s}{s}{s}--width {d} --height {d} --signal 0x{x} --server 0x{x}",
        .{
            conhost_path.utf8,
            inherit_cursor,
            ambiguous_is_wide,
            text_measurement,
            size.X,
            size.Y,
            @intFromPtr(signal_pipe_conhost_side),
            @intFromPtr(server_handle),
        },
    ) catch return windows.E_OUTOFMEMORY;
    defer alloc.free(cmd_utf8);

    const cmd = std.unicode.wtf8ToWtf16LeAllocZ(alloc, cmd_utf8) catch return windows.E_OUTOFMEMORY;
    defer alloc.free(cmd);

    var si_ex: windows.STARTUPINFOEXW = .{
        .StartupInfo = std.mem.zeroes(windows.STARTUPINFOW),
        .lpAttributeList = null,
    };
    si_ex.StartupInfo.cb = @sizeOf(windows.STARTUPINFOEXW);
    si_ex.StartupInfo.hStdInput = h_input;
    si_ex.StartupInfo.hStdOutput = h_output;
    si_ex.StartupInfo.hStdError = h_output;
    si_ex.StartupInfo.dwFlags |= windows.STARTF_USESTDHANDLES;

    var inherited_handles = [_]windows.HANDLE{ server_handle, h_input, h_output, signal_pipe_conhost_side };

    var attr_list_size: windows.SIZE_T = 0;
    _ = windows.InitializeProcThreadAttributeList(null, 1, 0, &attr_list_size);

    const attr_list_mem = alloc.alloc(u8, attr_list_size) catch return windows.E_OUTOFMEMORY;
    defer alloc.free(attr_list_mem);

    si_ex.lpAttributeList = @ptrCast(attr_list_mem.ptr);
    if (windows.InitializeProcThreadAttributeList(si_ex.lpAttributeList, 1, 0, &attr_list_size) == .FALSE) {
        return hresultFromWin32(windows.GetLastError());
    }
    defer windows.DeleteProcThreadAttributeList(si_ex.lpAttributeList);

    if (windows.UpdateProcThreadAttribute(
        si_ex.lpAttributeList,
        0,
        windows.PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        @ptrCast(&inherited_handles),
        inherited_handles.len * @sizeOf(windows.HANDLE),
        null,
        null,
    ) == .FALSE) {
        return hresultFromWin32(windows.GetLastError());
    }

    var pi: windows.PROCESS_INFORMATION = undefined;
    const creation_flags: windows.DWORD = @bitCast(windows.CreateProcessFlags{
        .extended_startupinfo_present = true,
    });

    if (windows.CreateProcessAsUserW(
        token,
        conhost_path.wide.ptr,
        cmd.ptr,
        null,
        null,
        .TRUE,
        creation_flags,
        null,
        null,
        &si_ex.StartupInfo,
        &pi,
    ) == .FALSE) {
        return hresultFromWin32(windows.GetLastError());
    }
    _ = windows.NtClose(pi.hThread);

    pty.hSignal = signal_pipe_our_side;
    pty.hPtyReference = reference_handle;
    pty.hConPtyProcess = pi.hProcess;

    _ = windows.NtClose(signal_pipe_conhost_side);
    _ = windows.NtClose(server_handle);

    return windows.S_OK;
}

fn resizePseudoConsole(
    p_pty: ?*const windows.PseudoConsole,
    size: windows.COORD,
) callconv(.winapi) windows.HRESULT {
    const pty = p_pty orelse return windows.E_INVALIDARG;
    if (size.X < 0 or size.Y < 0) return windows.E_INVALIDARG;

    const signal_packet: [3]u16 = .{
        PTY_SIGNAL_RESIZE_WINDOW,
        @bitCast(size.X),
        @bitCast(size.Y),
    };

    var iosb: windows.IO_STATUS_BLOCK = undefined;
    const status = windows.NtWriteFile(
        pty.hSignal,
        null,
        null,
        null,
        &iosb,
        @ptrCast(&signal_packet),
        @sizeOf(@TypeOf(signal_packet)),
        null,
        null,
    );

    if (!ntSuccess(status)) return hresultFromNt(status);
    if (!ntSuccess(iosb.u.Status)) return hresultFromNt(iosb.u.Status);
    return windows.S_OK;
}

fn closePseudoConsole(p_pty: ?*windows.PseudoConsole) callconv(.winapi) void {
    if (p_pty) |pty| {
        if (handleIsValid(pty.hSignal)) {
            _ = windows.NtClose(pty.hSignal);
            pty.hSignal = windows.INVALID_HANDLE_VALUE;
        }
        if (handleIsValid(pty.hPtyReference)) {
            _ = windows.NtClose(pty.hPtyReference);
            pty.hPtyReference = windows.INVALID_HANDLE_VALUE;
        }
        if (handleIsValid(pty.hConPtyProcess)) {
            _ = windows.NtClose(pty.hConPtyProcess);
            pty.hConPtyProcess = windows.INVALID_HANDLE_VALUE;
        }
    }
}

fn createClientHandle(
    p_handle: *windows.HANDLE,
    server_handle: windows.HANDLE,
    name: [*:0]const windows.WCHAR,
    inheritable: windows.BOOLEAN,
) windows.NTSTATUS {
    const desired_access: windows.ACCESS_MASK = @bitCast(@as(
        windows.DWORD,
        windows.GENERIC_WRITE | windows.GENERIC_READ | windows.SYNCHRONIZE,
    ));
    const open_options = windows.FILE_SYNCHRONOUS_IO_NONALERT;
    return createHandle(p_handle, name, desired_access, server_handle, inheritable, open_options);
}

fn createServerHandle(
    p_handle: *windows.HANDLE,
    inheritable: windows.BOOLEAN,
) windows.NTSTATUS {
    const device_name = std.unicode.utf8ToUtf16LeStringLiteral("\\Device\\ConDrv\\Server");
    const desired_access: windows.ACCESS_MASK = @bitCast(@as(windows.DWORD, windows.GENERIC_ALL));
    return createHandle(p_handle, device_name, desired_access, null, inheritable, 0);
}

fn createHandle(
    p_handle: *windows.HANDLE,
    device_name: [*:0]const windows.WCHAR,
    desired_access: windows.ACCESS_MASK,
    parent: ?windows.HANDLE,
    inheritable: windows.BOOLEAN,
    open_options: windows.ULONG,
) windows.NTSTATUS {
    var object_flags: windows.OBJECT.ATTRIBUTES.Flags = .{ .CASE_INSENSITIVE = true };
    if (inheritable != .FALSE) object_flags.INHERIT = true;

    const name_len_bytes = std.mem.len(device_name) * @sizeOf(windows.WCHAR);
    var name: windows.UNICODE_STRING = .{
        .Length = @intCast(name_len_bytes),
        .MaximumLength = @intCast(name_len_bytes + @sizeOf(windows.WCHAR)),
        .Buffer = @constCast(device_name),
    };

    var object_attributes: windows.OBJECT.ATTRIBUTES = .{
        .RootDirectory = parent,
        .ObjectName = &name,
        .Attributes = object_flags,
        .SecurityDescriptor = null,
        .SecurityQualityOfService = null,
    };

    var iosb: windows.IO_STATUS_BLOCK = undefined;
    const share_access: windows.FILE.SHARE = .{ .READ = true, .WRITE = true, .DELETE = true };

    return windows.NtOpenFile(
        p_handle,
        desired_access,
        &object_attributes,
        &iosb,
        share_access,
        @bitCast(open_options),
    );
}

inline fn handleIsValid(h: windows.HANDLE) bool {
    return (h != windows.INVALID_HANDLE_VALUE) and (@intFromPtr(h) != 0);
}

inline fn ntSuccess(status: windows.NTSTATUS) bool {
    return @as(i32, @bitCast(@intFromEnum(status))) >= 0;
}

fn hresultFromWin32(err: windows.Win32Error) windows.HRESULT {
    const code: u32 = @intFromEnum(err) & 0x0000_FFFF;
    return @bitCast(0x8007_0000 | code);
}

fn hresultFromNt(status: windows.NTSTATUS) windows.HRESULT {
    const raw: u32 = @intFromEnum(status);
    return @bitCast(raw | 0x1000_0000);
}
