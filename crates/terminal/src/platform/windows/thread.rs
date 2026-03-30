//! Thread lifecycle built on NtCreateThreadEx.
use std::ffi::c_void;
use std::io;

use super::ntdll::{
    FALSE, Handle, MAXIMUM_ALLOWED, NtAlertThread, NtClose, NtCreateThreadEx, NtResumeThread,
    NtSetInformationThread, NtStatus, NtWaitForSingleObject, ObjectAttributes,
    PS_ATTRIBUTE_TEB_ADDRESS, PsAttribute, PsAttributeList, PsAttributeValue, STATUS_SUCCESS,
    THREAD_CREATE_FLAGS_CREATE_SUSPENDED, THREAD_INFORMATION_CLASS_NAME_INFORMATION, Teb,
    UnicodeString,
};

pub struct PlatformThread {
    handle: Handle,
    id: u32,
}

unsafe impl Send for PlatformThread {}

impl PlatformThread {
    pub fn spawn_suspended(
        entry: unsafe extern "system" fn(*mut c_void) -> u32,
        context: *mut c_void,
    ) -> io::Result<Self> {
        let mut handle: Handle = std::ptr::null_mut();
        let attrs = ObjectAttributes::empty();

        let mut teb: *mut Teb = std::ptr::null_mut();
        let mut attr_list = PsAttributeList {
            total_length: std::mem::size_of::<PsAttributeList>(),
            attributes: [PsAttribute {
                attribute: PS_ATTRIBUTE_TEB_ADDRESS,
                size: std::mem::size_of::<*mut Teb>(),
                value: PsAttributeValue {
                    value_ptr: &mut teb as *mut _ as *mut c_void,
                },
                return_length: std::ptr::null_mut(),
            }],
        };

        let status = unsafe {
            NtCreateThreadEx(
                &mut handle,
                MAXIMUM_ALLOWED,
                &attrs,
                (-1isize) as usize as Handle,
                entry,
                context,
                THREAD_CREATE_FLAGS_CREATE_SUSPENDED,
                0,
                0,
                0,
                &mut attr_list as *mut _ as *mut c_void,
            )
        };
        if status != STATUS_SUCCESS || handle.is_null() {
            return Err(io::Error::other(format!(
                "NtCreateThreadEx failed: 0x{status:08X}"
            )));
        }

        let id = if teb.is_null() {
            0
        } else {
            // SAFETY: Teb pointer returned by NtCreateThreadEx via PS_ATTRIBUTE_TEB_ADDRESS.
            unsafe { (*teb).client_id.unique_thread as usize as u32 }
        };

        Ok(Self { handle, id })
    }

    pub fn resume(&self) -> io::Result<()> {
        let status = unsafe { NtResumeThread(self.handle, std::ptr::null_mut()) };
        if status != STATUS_SUCCESS {
            return Err(io::Error::other(format!(
                "NtResumeThread failed: 0x{status:08X}"
            )));
        }
        Ok(())
    }

    pub fn alert(&self) -> NtStatus {
        unsafe { NtAlertThread(self.handle) }
    }

    pub fn handle(&self) -> Handle {
        self.handle
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn join(mut self) {
        if !self.handle.is_null() {
            let infinite_timeout: i64 = i64::MIN;
            let _ = unsafe { NtWaitForSingleObject(self.handle, FALSE, &infinite_timeout) };
            let _ = unsafe { NtClose(self.handle) };
            self.handle = std::ptr::null_mut();
        }
    }
}

impl Drop for PlatformThread {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            let _ = unsafe { NtClose(self.handle) };
            self.handle = std::ptr::null_mut();
        }
    }
}

/// Best-effort thread naming for profiler/debugger visibility.
///
/// Uses `NtSetInformationThread(NameInformation)` on current thread.
pub fn set_current_thread_name(name: &str) {
    let mut wide: Vec<u16> = name.encode_utf16().collect();
    let max_len_u16 = (u16::MAX as usize) / 2;
    if wide.len() > max_len_u16 {
        wide.truncate(max_len_u16);
    }
    let mut u = UnicodeString {
        length: (wide.len() * 2) as u16,
        maximum_length: (wide.len() * 2) as u16,
        buffer: wide.as_mut_ptr(),
    };
    let current_thread: Handle = (-2isize) as usize as Handle;
    unsafe {
        let _ = NtSetInformationThread(
            current_thread,
            THREAD_INFORMATION_CLASS_NAME_INFORMATION,
            &mut u as *mut _ as *const c_void,
            std::mem::size_of::<UnicodeString>() as u32,
        );
    }
}
