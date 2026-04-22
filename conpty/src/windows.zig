const std = @import("std");
const windows_new = @import("windows/windows.zig");

pub const DWORD = std.os.windows.DWORD;
pub const ULONG = std.os.windows.ULONG;
pub const SIZE_T = std.os.windows.SIZE_T;

pub const WCHAR = std.os.windows.WCHAR;
pub const LPWSTR = std.os.windows.LPWSTR;
pub const LPCWSTR = std.os.windows.LPCWSTR;

pub const COORD = std.os.windows.COORD;
pub const UNICODE_STRING = windows_new.UNICODE_STRING;

pub const HRESULT = std.os.windows.HRESULT;
pub const NTSTATUS = windows_new.NTSTATUS;
pub const Win32Error = std.os.windows.Win32Error;

pub const HANDLE = std.os.windows.HANDLE;
pub const ACCESS_MASK = windows_new.ACCESS_MASK;
pub const IO_STATUS_BLOCK = windows_new.IO_STATUS_BLOCK;
pub const LARGE_INTEGER = windows_new.LARGE_INTEGER;
pub const OBJECT = windows_new.OBJECT;
pub const FILE = windows_new.FILE;

pub const INVALID_HANDLE_VALUE = std.os.windows.INVALID_HANDLE_VALUE;
pub const E_FAIL = std.os.windows.E_FAIL;
pub const E_INVALIDARG = std.os.windows.E_INVALIDARG;
pub const E_OUTOFMEMORY = std.os.windows.E_OUTOFMEMORY;
pub const S_OK = std.os.windows.S_OK;

pub const TRUE = std.os.windows.TRUE;
pub const FALSE = std.os.windows.FALSE;

pub const OBJ_INHERIT = std.os.windows.OBJ_INHERIT;
pub const OBJ_CASE_INSENSITIVE = std.os.windows.OBJ_CASE_INSENSITIVE;

pub const FILE_SHARE_DELETE = std.os.windows.FILE_SHARE_DELETE;
pub const FILE_SHARE_READ = std.os.windows.FILE_SHARE_READ;
pub const FILE_SHARE_WRITE = std.os.windows.FILE_SHARE_WRITE;
pub const FILE_SYNCHRONOUS_IO_NONALERT = std.os.windows.FILE_SYNCHRONOUS_IO_NONALERT;

pub const PseudoConsole = struct {
    hSignal: HANDLE,
    hPtyReference: HANDLE,
    hConPtyProcess: HANDLE,
};

// NTDLL / Windows API Declarations
// Copied from the Zig 0.16 standard library
// TODO: Replace with standard library declarations after Zig 0.16.0
const ntdll = @import("windows/ntdll.zig");

pub const getSystemDirectoryWtf16Le = windows_new.getSystemDirectoryWtf16Le;

pub const GENERIC_ALL = std.os.windows.GENERIC_ALL;
pub const GENERIC_WRITE = std.os.windows.GENERIC_WRITE;
pub const GENERIC_READ = std.os.windows.GENERIC_READ;
pub const SYNCHRONIZE = std.os.windows.SYNCHRONIZE;
pub const BOOLEAN = windows_new.BOOLEAN;
pub const BOOL = windows_new.BOOL;

pub const NtWriteFile = ntdll.NtWriteFile;
pub const NtOpenFile = ntdll.NtOpenFile;
pub const NtClose = ntdll.NtClose;

pub const PROCESS_INFORMATION = std.os.windows.PROCESS_INFORMATION;
pub const SECURITY_ATTRIBUTES = std.os.windows.SECURITY_ATTRIBUTES;
pub const STARTUPINFOW = std.os.windows.STARTUPINFOW;

pub const STARTUPINFOEXW = extern struct {
    StartupInfo: STARTUPINFOW,
    lpAttributeList: ?*anyopaque,
};

pub const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x00020002;
pub const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x00020016;

pub const CreatePipe = std.os.windows.CreatePipe;
pub const SetHandleInformation = std.os.windows.SetHandleInformation;
pub const GetLastError = std.os.windows.GetLastError;

pub const HANDLE_FLAG_INHERIT = std.os.windows.HANDLE_FLAG_INHERIT;
pub const STARTF_USESTDHANDLES = std.os.windows.STARTF_USESTDHANDLES;
pub const CreateProcessFlags = std.os.windows.CreateProcessFlags;
pub const CREATE_UNICODE_ENVIRONMENT = std.os.windows.CREATE_UNICODE_ENVIRONMENT;
pub const INVALID_FILE_ATTRIBUTES = std.os.windows.INVALID_FILE_ATTRIBUTES;
pub const DUPLICATE_SAME_ACCESS = std.os.windows.DUPLICATE_SAME_ACCESS;
pub const GetCurrentProcess = std.os.windows.GetCurrentProcess;

pub const SYSTEM_CONSOLE_INFORMATION_CLASS: u32 = 132;
pub const SYSTEM_CONSOLE_INFORMATION = packed struct(u32) {
    DriverLoaded: u1 = 0,
    Spare: u31 = 0,
};

pub extern "ntdll" fn NtSetSystemInformation(
    system_information_class: u32,
    system_information: ?*const anyopaque,
    system_information_length: u32,
) callconv(.winapi) NTSTATUS;

pub extern "kernel32" fn InitializeProcThreadAttributeList(
    lpAttributeList: ?*anyopaque,
    dwAttributeCount: DWORD,
    dwFlags: DWORD,
    lpSize: *SIZE_T,
) callconv(.winapi) BOOL;

pub extern "kernel32" fn UpdateProcThreadAttribute(
    lpAttributeList: ?*anyopaque,
    dwFlags: DWORD,
    Attribute: usize,
    lpValue: ?*anyopaque,
    cbSize: SIZE_T,
    lpPreviousValue: ?*anyopaque,
    lpReturnSize: ?*SIZE_T,
) callconv(.winapi) BOOL;

pub extern "kernel32" fn DeleteProcThreadAttributeList(
    lpAttributeList: ?*anyopaque,
) callconv(.winapi) void;

pub extern "advapi32" fn CreateProcessAsUserW(
    hToken: ?HANDLE,
    lpApplicationName: ?LPCWSTR,
    lpCommandLine: ?LPWSTR,
    lpProcessAttributes: ?*SECURITY_ATTRIBUTES,
    lpThreadAttributes: ?*SECURITY_ATTRIBUTES,
    bInheritHandles: BOOL,
    dwCreationFlags: DWORD,
    lpEnvironment: ?*anyopaque,
    lpCurrentDirectory: ?LPCWSTR,
    lpStartupInfo: *STARTUPINFOW,
    lpProcessInformation: *PROCESS_INFORMATION,
) callconv(.winapi) BOOL;

pub extern "kernel32" fn CreateProcessW(
    lpApplicationName: ?LPCWSTR,
    lpCommandLine: ?LPWSTR,
    lpProcessAttributes: ?*SECURITY_ATTRIBUTES,
    lpThreadAttributes: ?*SECURITY_ATTRIBUTES,
    bInheritHandles: BOOL,
    dwCreationFlags: DWORD,
    lpEnvironment: ?*anyopaque,
    lpCurrentDirectory: ?LPCWSTR,
    lpStartupInfo: *STARTUPINFOW,
    lpProcessInformation: *PROCESS_INFORMATION,
) callconv(.winapi) BOOL;

pub extern "kernel32" fn GetFileAttributesW(
    lpFileName: LPCWSTR,
) callconv(.winapi) DWORD;

pub extern "kernel32" fn DuplicateHandle(
    hSourceProcessHandle: HANDLE,
    hSourceHandle: HANDLE,
    hTargetProcessHandle: HANDLE,
    lpTargetHandle: *HANDLE,
    dwDesiredAccess: DWORD,
    bInheritHandle: BOOL,
    dwOptions: DWORD,
) callconv(.winapi) BOOL;
