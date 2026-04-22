use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io::{Error, ErrorKind, Result};
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::{ffi::c_void, ptr};

use log::warn;
use windows_sys::Win32::Foundation::{GetLastError, HANDLE, INVALID_HANDLE_VALUE, S_OK};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
    PIPE_ACCESS_OUTBOUND,
};
use windows_sys::Win32::System::Console::{COORD, HPCON};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::CREATE_UNICODE_ENVIRONMENT;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::core::HRESULT;

use super::child::ChildProcess;
use super::{OwnedHandle, Pty, cmdline, win32_string};
use crate::{Options, WindowSize};

const PIPE_CAPACITY: u32 = 128 * 1024;
const ERROR_PIPE_CONNECTED: u32 = 535;
static PIPE_COUNTER: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" {
    fn ptyCreate(
        size: COORD,
        h_input: HANDLE,
        h_output: HANDLE,
        dw_flags: u32,
        command_line: *mut u16,
        current_directory: *const u16,
        environment: *mut c_void,
        creation_flags: u32,
        out_hpcon: *mut HPCON,
        out_child_process: *mut HANDLE,
    ) -> HRESULT;
    fn ptyRelease(hpcon: HPCON) -> HRESULT;
    fn ptyResize(hpcon: HPCON, size: COORD) -> HRESULT;
    fn ptyClose(hpcon: HPCON);
}

pub(crate) fn resize(hpcon: HPCON, size: COORD) -> HRESULT {
    unsafe { ptyResize(hpcon, size) }
}

fn close_failed_pty(
    pty_handle: HPCON,
    conout: OwnedHandle,
    conout_for_pty: OwnedHandle,
    conin: OwnedHandle,
    conin_for_pty: OwnedHandle,
) {
    // ptyClose waits on the PTY pipes. Close our local endpoints first so
    // error cleanup cannot deadlock before the read thread exists.
    drop(conout_for_pty);
    drop(conin_for_pty);
    drop(conout);
    drop(conin);

    unsafe { ptyClose(pty_handle) }
}

/// RAII PTY session handle backed by the statically linked Zig conpty shim.
pub struct PtySession {
    handle: Option<HPCON>,
    close_thread: Option<JoinHandle<()>>,
}

impl PtySession {
    /// Raw HPCON value for the read thread (resize operations).
    /// Returns 0 if HPCON has been taken by close_async.
    pub fn raw_hpcon(&self) -> HPCON {
        self.handle.unwrap_or(0)
    }

    /// Close HPCON on a background thread, sending `CTRL_CLOSE_EVENT` to
    /// attached processes. `ptyClose` blocks until the conout
    /// pipe is fully read, so the worker loop must keep draining reads.
    pub fn close_async(&mut self) {
        if self.close_thread.is_some() {
            return;
        }

        if let Some(handle) = self.handle.take() {
            self.close_thread = std::thread::Builder::new()
                .name("pty-hpcon-close".into())
                .spawn(move || unsafe { ptyClose(handle) })
                .ok();
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // Blocks until conout pipe is drained. Skipped if close_async
            // already took the handle.
            unsafe { ptyClose(handle) }
        }

        if let Some(close_thread) = self.close_thread.take() {
            let _ = close_thread.join();
        }
    }
}

// The PTY session handle can be sent between threads.
unsafe impl Send for PtySession {}

pub fn new(config: &Options, window_size: WindowSize) -> Result<Pty> {
    let mut pty_handle: HPCON = 0;
    let mut child_process: HANDLE = ptr::null_mut();

    let (conout, conout_for_pty) = create_overlapped_pipe_pair(PIPE_ACCESS_INBOUND, PIPE_CAPACITY)?;
    let (conin, conin_for_pty) = create_overlapped_pipe_pair(PIPE_ACCESS_OUTBOUND, PIPE_CAPACITY)?;

    let cmdline = win32_string(&cmdline(config));
    let cwd = config.working_directory.as_ref().map(win32_string);
    let mut creation_flags = 0;
    let custom_env_block = convert_custom_env(&config.env);
    let custom_env_block_pointer = match &custom_env_block {
        Some(custom_env_block) => {
            creation_flags |= CREATE_UNICODE_ENVIRONMENT;
            custom_env_block.as_ptr() as *mut std::ffi::c_void
        }
        None => ptr::null_mut(),
    };

    // Create PTY backend and spawn child process in Zig.
    let result = unsafe {
        ptyCreate(
            window_size.into(),
            conin_for_pty.raw(),
            conout_for_pty.raw(),
            0,
            cmdline.as_ptr() as *mut u16,
            cwd.as_ref().map_or_else(ptr::null, |s| s.as_ptr()),
            custom_env_block_pointer,
            creation_flags,
            &mut pty_handle as *mut _,
            &mut child_process as *mut _,
        )
    };

    if result != S_OK {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "ptyCreate failed (HRESULT 0x{:08X}). \
                 Requires Windows 10 1809+.",
                result,
            ),
        ));
    }

    let child = match ChildProcess::new(child_process) {
        Ok(child) => child,
        Err(err) => {
            close_failed_pty(pty_handle, conout, conout_for_pty, conin, conin_for_pty);
            return Err(err);
        }
    };

    let release_result = unsafe { ptyRelease(pty_handle) };
    if release_result != S_OK {
        close_failed_pty(pty_handle, conout, conout_for_pty, conin, conin_for_pty);
        return Err(Error::other(format!(
            "ptyRelease failed (HRESULT 0x{:08X})",
            release_result,
        )));
    }

    let session = PtySession {
        handle: Some(pty_handle as HPCON),
        close_thread: None,
    };

    Ok(Pty::new(
        session,
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

fn create_overlapped_pipe_pair(
    open_mode: u32,
    buffer_size: u32,
) -> Result<(OwnedHandle, OwnedHandle)> {
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
