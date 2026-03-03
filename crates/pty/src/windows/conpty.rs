use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io::{Error, ErrorKind, Result};
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::{mem, ptr};

use log::warn;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, S_OK};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
    PIPE_ACCESS_OUTBOUND,
};
use windows_sys::Win32::System::Console::{COORD, HPCON};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, PROCESS_INFORMATION};
use windows_sys::core::{HRESULT, PWSTR};
use windows_sys::{s, w};

use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, EXTENDED_STARTUPINFO_PRESENT,
    DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
    UpdateProcThreadAttribute,
};

use super::child::ChildProcess;
use super::{OwnedHandle, Pty, cmdline, win32_string};
use crate::{Options, WindowSize};

const PIPE_CAPACITY: u32 = 128 * 1024;
const ERROR_PIPE_CONNECTED: u32 = 535;
static PIPE_COUNTER: AtomicU32 = AtomicU32::new(0);

// Function types for the ConPTY pseudo console API.
//
// Note: The vendored conpty.dll exports both ConptyCreatePseudoConsole and
// CreatePseudoConsole as aliases pointing to the same implementation. We use
// the Conpty-prefixed versions as they are the primary exports.
type CreatePseudoConsoleFn =
    unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> HRESULT;
type ResizePseudoConsoleFn = unsafe extern "system" fn(HPCON, COORD) -> HRESULT;
type ClosePseudoConsoleFn = unsafe extern "system" fn(HPCON);

struct ConptyFns {
    create: CreatePseudoConsoleFn,
    resize: ResizePseudoConsoleFn,
    close: ClosePseudoConsoleFn,
}

impl ConptyFns {
    /// Load function pointers from the vendored conpty.dll.
    ///
    /// conpty.dll must be present next to the executable (placed there by
    /// build.rs via fetch-conpty.ts). At runtime, the DLL will look for
    /// OpenConsole.exe in the same directory; if found, it uses OpenConsole
    /// as the console host. Otherwise, it falls back to the system's conhost.exe.
    fn load() -> Result<Self> {
        type RawFn = unsafe extern "system" fn() -> isize;
        unsafe {
            let hmodule = LoadLibraryW(w!("conpty.dll"));
            if hmodule.is_null() {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    "conpty.dll not found next to the executable — \
                     ensure the build completed successfully",
                ));
            }

            // Use the Conpty-prefixed exports; these are the primary exports
            // from the DLL. The unprefixed versions (CreatePseudoConsole, etc.)
            // are compatibility aliases pointing to the same implementations.
            let create = GetProcAddress(hmodule, s!("ConptyCreatePseudoConsole"))
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "ConptyCreatePseudoConsole not found in conpty.dll"))?;
            let resize = GetProcAddress(hmodule, s!("ConptyResizePseudoConsole"))
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "ConptyResizePseudoConsole not found in conpty.dll"))?;
            let close = GetProcAddress(hmodule, s!("ConptyClosePseudoConsole"))
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "ConptyClosePseudoConsole not found in conpty.dll"))?;

            // hmodule is intentionally leaked: the DLL must remain loaded for
            // the lifetime of any HPCON created through it.
            Ok(Self {
                create: mem::transmute::<RawFn, CreatePseudoConsoleFn>(create),
                resize: mem::transmute::<RawFn, ResizePseudoConsoleFn>(resize),
                close: mem::transmute::<RawFn, ClosePseudoConsoleFn>(close),
            })
        }
    }
}

/// RAII Pseudoconsole handle backed by the vendored conpty.dll + OpenConsole.exe.
pub struct Conpty {
    handle: Option<HPCON>,
    fns: ConptyFns,
    close_thread: Option<JoinHandle<()>>,
}

impl Conpty {
    pub fn resize(&mut self, window_size: WindowSize) {
        let Some(handle) = self.handle else { return };
        let result = unsafe { (self.fns.resize)(handle, window_size.into()) };
        if result != S_OK {
            log::error!("ConptyResizePseudoConsole failed: HRESULT 0x{:08X}", result);
        }
    }

    /// Close HPCON on a background thread, sending `CTRL_CLOSE_EVENT` to
    /// attached processes. `ConptyClosePseudoConsole` blocks until the conout
    /// pipe is fully read, so the worker loop must keep draining reads.
    pub fn close_async(&mut self) {
        if self.close_thread.is_some() {
            return;
        }

        if let Some(handle) = self.handle.take() {
            let close = self.fns.close;
            self.close_thread = std::thread::Builder::new()
                .name("pty-hpcon-close".into())
                .spawn(move || unsafe { close(handle) })
                .ok();
        }
    }
}

impl Drop for Conpty {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // Blocks until conout pipe is drained. Skipped if close_async
            // already took the handle.
            unsafe { (self.fns.close)(handle) }
        }

        if let Some(close_thread) = self.close_thread.take() {
            let _ = close_thread.join();
        }
    }
}

// The ConPTY handle can be sent between threads.
unsafe impl Send for Conpty {}

pub fn new(config: &Options, window_size: WindowSize) -> Result<Pty> {
    let fns = ConptyFns::load()?;
    let mut pty_handle: HPCON = 0;

    let (conout, conout_for_pty) = create_overlapped_pipe_pair(PIPE_ACCESS_INBOUND, PIPE_CAPACITY)?;
    let (conin, conin_for_pty) = create_overlapped_pipe_pair(PIPE_ACCESS_OUTBOUND, PIPE_CAPACITY)?;

    // Create the Pseudo Console, using the pipes.
    let result = unsafe {
        (fns.create)(
            window_size.into(),
            conin_for_pty.raw(),
            conout_for_pty.raw(),
            0,
            &mut pty_handle as *mut _,
        )
    };

    if result != S_OK {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "CreatePseudoConsole failed (HRESULT 0x{:08X}). \
                 Requires Windows 10 1809+.",
                result,
            ),
        ));
    }

    let mut success;

    // Prepare child process startup info.
    let mut size: usize = 0;

    let mut startup_info_ex: STARTUPINFOEXW = unsafe { mem::zeroed() };
    startup_info_ex.StartupInfo.lpTitle = std::ptr::null_mut() as PWSTR;
    startup_info_ex.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;

    // Prevents the PTY process from inheriting any handles.
    startup_info_ex.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;

    // Create the appropriately sized thread attribute list.
    unsafe {
        let failure =
            InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size as *mut usize) > 0;

        // This call was expected to return false.
        if failure {
            return Err(Error::last_os_error());
        }
    }

    let mut attr_list: Box<[u8]> = vec![0; size].into_boxed_slice();

    // Set startup info's attribute list & initialize it
    //
    // Lint failure is spurious; it's because winapi's definition of PROC_THREAD_ATTRIBUTE_LIST
    // implies it is one pointer in size (32 or 64 bits) but really this is just a dummy value.
    // Casting a *mut u8 (pointer to 8 bit type) might therefore not be aligned correctly in
    // the compiler's eyes.
    #[allow(clippy::cast_ptr_alignment)]
    {
        startup_info_ex.lpAttributeList = attr_list.as_mut_ptr() as _;
    }
    let _attr_list_guard = AttrListGuard(startup_info_ex.lpAttributeList.cast());

    unsafe {
        success = InitializeProcThreadAttributeList(
            startup_info_ex.lpAttributeList,
            1,
            0,
            &mut size as *mut usize,
        ) > 0;

        if !success {
            return Err(Error::last_os_error());
        }
    }

    // Set thread attribute list's Pseudo Console to the specified ConPTY.
    unsafe {
        success = UpdateProcThreadAttribute(
            startup_info_ex.lpAttributeList,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            pty_handle as *mut std::ffi::c_void,
            mem::size_of::<HPCON>(),
            ptr::null_mut(),
            ptr::null_mut(),
        ) > 0;

        if !success {
            return Err(Error::last_os_error());
        }
    }

    // Prepare child process creation arguments.
    let cmdline = win32_string(&cmdline(config));
    let cwd = config.working_directory.as_ref().map(win32_string);
    let mut creation_flags = EXTENDED_STARTUPINFO_PRESENT;
    let custom_env_block = convert_custom_env(&config.env);
    let custom_env_block_pointer = match &custom_env_block {
        Some(custom_env_block) => {
            creation_flags |= CREATE_UNICODE_ENVIRONMENT;
            custom_env_block.as_ptr() as *mut std::ffi::c_void
        }
        None => ptr::null_mut(),
    };

    let mut proc_info: PROCESS_INFORMATION = unsafe { mem::zeroed() };
    unsafe {
        success = CreateProcessW(
            ptr::null(),
            cmdline.as_ptr() as PWSTR,
            ptr::null_mut(),
            ptr::null_mut(),
            false as i32,
            creation_flags,
            custom_env_block_pointer,
            cwd.as_ref().map_or_else(ptr::null, |s| s.as_ptr()),
            &mut startup_info_ex.StartupInfo as *mut STARTUPINFOW,
            &mut proc_info as *mut PROCESS_INFORMATION,
        ) > 0;

        if !success {
            return Err(Error::last_os_error());
        }
    }
    unsafe {
        CloseHandle(proc_info.hThread);
    }

    let child = ChildProcess::new(proc_info.hProcess)?;
    let conpty = Conpty {
        handle: Some(pty_handle as HPCON),
        fns,
        close_thread: None,
    };

    Ok(Pty::new(
        conpty,
        OwnedHandle::new(conout.into_raw()),
        OwnedHandle::new(conin.into_raw()),
        child,
    ))
}

// Windows environment variables are case-insensitive, and the caller is responsible for
// deduplicating environment variables, so do that here while converting.
//
// https://learn.microsoft.com/en-us/previous-versions/troubleshoot/windows/win32/createprocess-cannot-eliminate-duplicate-variables#environment-variables
fn convert_custom_env(custom_env: &HashMap<String, String>) -> Option<Vec<u16>> {
    // Windows inherits parent's env when no `lpEnvironment` parameter is specified.
    if custom_env.is_empty() {
        return None;
    }

    let mut converted_block = Vec::new();
    let mut all_env_keys = HashSet::new();
    for (custom_key, custom_value) in custom_env {
        let custom_key_os = OsStr::new(custom_key);
        if all_env_keys.insert(custom_key_os.to_ascii_uppercase()) {
            add_windows_env_key_value_to_block(
                &mut converted_block,
                custom_key_os,
                OsStr::new(custom_value),
            );
        } else {
            warn!(
                "Omitting environment variable pair with \
                 duplicate key: '{custom_key}={custom_value}'"
            );
        }
    }

    // Pull the current process environment after, to avoid overwriting the user provided one.
    for (inherited_key, inherited_value) in std::env::vars_os() {
        if all_env_keys.insert(inherited_key.to_ascii_uppercase()) {
            add_windows_env_key_value_to_block(
                &mut converted_block,
                &inherited_key,
                &inherited_value,
            );
        }
    }

    converted_block.push(0);
    Some(converted_block)
}

// According to the `lpEnvironment` parameter description:
// https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessa#parameters
//
// > An environment block consists of a null-terminated block of null-terminated strings. Each
// string is in the following form:
// >
// > name=value\0
fn add_windows_env_key_value_to_block(block: &mut Vec<u16>, key: &OsStr, value: &OsStr) {
    block.extend(key.encode_wide());
    block.push('=' as u16);
    block.extend(value.encode_wide());
    block.push(0);
}

impl From<WindowSize> for COORD {
    fn from(window_size: WindowSize) -> Self {
        COORD {
            X: window_size.num_cols as i16,
            Y: window_size.num_lines as i16,
        }
    }
}

fn create_overlapped_pipe_pair(open_mode: u32, buffer_size: u32) -> Result<(OwnedHandle, OwnedHandle)> {
    let pipe_name = unique_pipe_name();
    let name_w = win32_string(&pipe_name);

    // CreateNamedPipeW gives us an anonymous-like per-session endpoint with
    // FILE_FLAG_OVERLAPPED, which ConPTY can use directly for async I/O.
    let server_handle = unsafe {
        CreateNamedPipeW(
            name_w.as_ptr(),
            open_mode | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1, // single instance — FILE_FLAG_FIRST_PIPE_INSTANCE ensures creation fails if name is squatted
            buffer_size,
            buffer_size,
            0,
            ptr::null_mut(),
        )
    };
    if server_handle == INVALID_HANDLE_VALUE {
        return Err(Error::last_os_error());
    }
    let server = OwnedHandle::new(server_handle);

    let client_desired_access = if open_mode == PIPE_ACCESS_INBOUND {
        FILE_GENERIC_WRITE
    } else {
        FILE_GENERIC_READ
    };

    let client_handle = unsafe {
        CreateFileW(
            name_w.as_ptr(),
            client_desired_access,
            0,
            ptr::null_mut(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if client_handle == INVALID_HANDLE_VALUE {
        return Err(Error::last_os_error());
    }
    let client = OwnedHandle::new(client_handle);

    let connected = unsafe { ConnectNamedPipe(server.raw(), ptr::null_mut()) };
    if connected == 0 {
        let last_error = unsafe { GetLastError() };
        if last_error != ERROR_PIPE_CONNECTED {
            return Err(Error::last_os_error());
        }
    }

    Ok((server, client))
}

fn unique_pipe_name() -> String {
    let pid = unsafe { GetCurrentProcessId() };
    let seq = PIPE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0u128, |d| d.as_nanos());
    format!(r"\\.\pipe\rustty-conpty-{pid}-{seq}-{nonce:x}")
}

struct AttrListGuard(*mut std::ffi::c_void);

impl Drop for AttrListGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                DeleteProcThreadAttributeList(self.0.cast());
            }
        }
    }
}
