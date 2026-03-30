//! APC-based async IO helpers over ntdll.
use std::ffi::c_void;

use super::ntdll::{
    FALSE, Handle, IoStatusBlock, NtCancelIoFileEx, NtDelayExecution, NtReadFile, NtStatus,
    NtWriteFile, TRUE,
};

pub struct AsyncIo {
    pub iosb: IoStatusBlock,
    pub done: bool,
}

impl AsyncIo {
    pub const fn new() -> Self {
        Self {
            iosb: IoStatusBlock::zeroed(),
            done: false,
        }
    }

    pub fn reset(&mut self) {
        self.iosb = IoStatusBlock::zeroed();
        self.done = false;
    }
}

/// APC callback: sets a `bool` to wake the owning thread's harvest loop.
///
/// Both read and write paths use this. The APC fires on the thread that
/// issued the IO, during the next alertable wait.
pub unsafe extern "system" fn flag_apc(
    context: *mut c_void,
    _iosb: *mut IoStatusBlock,
    _reserved: u32,
) {
    let done = unsafe { &mut *(context as *mut bool) };
    *done = true;
}

pub unsafe fn async_read(handle: Handle, io: &mut AsyncIo, buf: *mut u8, len: u32) -> NtStatus {
    io.reset();
    unsafe {
        NtReadFile(
            handle,
            std::ptr::null_mut(),
            Some(flag_apc),
            &mut io.done as *mut bool as *mut c_void,
            &mut io.iosb,
            buf as *mut c_void,
            len,
            std::ptr::null(),
            std::ptr::null(),
        )
    }
}

pub unsafe fn async_write(handle: Handle, io: &mut AsyncIo, buf: *const u8, len: u32) -> NtStatus {
    io.reset();
    unsafe {
        NtWriteFile(
            handle,
            std::ptr::null_mut(),
            Some(flag_apc),
            &mut io.done as *mut bool as *mut c_void,
            &mut io.iosb,
            buf as *const c_void,
            len,
            std::ptr::null(),
            std::ptr::null(),
        )
    }
}

pub unsafe fn cancel_io(handle: Handle, iosb: &IoStatusBlock) -> NtStatus {
    let mut cancel_iosb = IoStatusBlock::zeroed();
    unsafe { NtCancelIoFileEx(handle, iosb as *const _, &mut cancel_iosb) }
}

pub fn alertable_wait(timeout_100ns: i64) -> NtStatus {
    unsafe { NtDelayExecution(TRUE, &timeout_100ns) }
}

pub fn sleep_100ns(timeout_100ns: i64) {
    let _ = unsafe { NtDelayExecution(FALSE, &timeout_100ns) };
}
