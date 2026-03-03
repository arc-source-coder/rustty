use std::io::{Error, Result};
use std::num::NonZeroU32;
use std::os::windows::process::ExitStatusExt;
use std::process::ExitStatus;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, GetProcessId, TerminateProcess};

use crate::ChildEvent;
const STILL_ACTIVE: u32 = 259;

pub struct ChildProcess {
    handle: HANDLE,
    pid: Option<NonZeroU32>,
}

impl ChildProcess {
    pub fn new(handle: HANDLE) -> Result<Self> {
        if handle.is_null() {
            return Err(Error::other("invalid child process handle"));
        }

        let pid = unsafe { NonZeroU32::new(GetProcessId(handle)) };
        Ok(Self { handle, pid })
    }

    pub fn try_wait_event(&self) -> Option<ChildEvent> {
        let mut exit_code = 0_u32;
        let ok = unsafe { GetExitCodeProcess(self.handle, &mut exit_code) };
        if ok == 0 || exit_code == STILL_ACTIVE {
            return None;
        }

        Some(ChildEvent::Exited(Some(ExitStatus::from_raw(exit_code))))
    }

    pub fn handle(&self) -> HANDLE {
        self.handle
    }

    pub fn pid(&self) -> Option<NonZeroU32> {
        self.pid
    }

    pub fn terminate(&self) {
        if !self.handle.is_null() {
            unsafe {
                TerminateProcess(self.handle, 1);
            }
        }
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                CloseHandle(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

unsafe impl Send for ChildProcess {}
