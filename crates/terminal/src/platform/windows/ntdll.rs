//! Thin, local ntdll ABI surface for terminal IO/threading.
//! Keeps raw Windows-native types contained to one module.
use std::ffi::c_void;

pub type NtStatus = i32;
pub type Handle = *mut c_void;
pub type Boolean = u8;
pub type AccessMask = u32;

pub const TRUE: Boolean = 1;
pub const FALSE: Boolean = 0;

pub const STATUS_SUCCESS: NtStatus = 0x00000000;
pub const STATUS_TIMEOUT: NtStatus = 0x00000102;
pub const STATUS_ALERTED: NtStatus = 0x00000101;
pub const STATUS_USER_APC: NtStatus = 0x000000C0;

pub const MAXIMUM_ALLOWED: AccessMask = 0x0200_0000;
pub const THREAD_CREATE_FLAGS_CREATE_SUSPENDED: u32 = 0x0000_0001;
pub const THREAD_INFORMATION_CLASS_NAME_INFORMATION: u32 = 38;

const PS_ATTRIBUTE_NUMBER_TEB_ADDRESS: usize = 4;
const PS_ATTRIBUTE_THREAD: usize = 0x0001_0000;
pub const PS_ATTRIBUTE_TEB_ADDRESS: usize = PS_ATTRIBUTE_NUMBER_TEB_ADDRESS | PS_ATTRIBUTE_THREAD;

#[repr(C)]
pub struct ObjectAttributes {
    pub length: u32,
    pub root_directory: Handle,
    pub object_name: *mut c_void,
    pub attributes: u32,
    pub security_descriptor: *mut c_void,
    pub security_quality_of_service: *mut c_void,
}

#[repr(C)]
pub struct UnicodeString {
    pub length: u16,
    pub maximum_length: u16,
    pub buffer: *mut u16,
}

impl ObjectAttributes {
    pub const fn empty() -> Self {
        Self {
            length: std::mem::size_of::<Self>() as u32,
            root_directory: std::ptr::null_mut(),
            object_name: std::ptr::null_mut(),
            attributes: 0,
            security_descriptor: std::ptr::null_mut(),
            security_quality_of_service: std::ptr::null_mut(),
        }
    }
}

#[repr(C)]
pub struct ClientId {
    pub unique_process: Handle,
    pub unique_thread: Handle,
}

#[repr(C)]
pub struct NtTib {
    pub exception_list: *mut c_void,
    pub stack_base: *mut c_void,
    pub stack_limit: *mut c_void,
    pub sub_system_tib: *mut c_void,
    pub fiber_data_or_version: *mut c_void,
    pub arbitrary_user_pointer: *mut c_void,
    pub self_ptr: *mut c_void,
}

#[repr(C)]
pub struct Teb {
    pub nt_tib: NtTib,
    pub environment_pointer: *mut c_void,
    pub client_id: ClientId,
}

#[repr(C)]
pub union PsAttributeValue {
    pub value: usize,
    pub value_ptr: *mut c_void,
}

#[repr(C)]
pub struct PsAttribute {
    pub attribute: usize,
    pub size: usize,
    pub value: PsAttributeValue,
    pub return_length: *mut usize,
}

#[repr(C)]
pub struct PsAttributeList {
    pub total_length: usize,
    pub attributes: [PsAttribute; 1],
}

#[link(name = "ntdll")]
unsafe extern "system" {
    pub fn NtDelayExecution(alertable: Boolean, delay_interval: *const i64) -> NtStatus;

    pub fn NtAlertThread(thread_handle: Handle) -> NtStatus;

    pub fn NtCreateThreadEx(
        thread_handle: *mut Handle,
        desired_access: AccessMask,
        object_attributes: *const ObjectAttributes,
        process_handle: Handle,
        start_routine: unsafe extern "system" fn(*mut c_void) -> u32,
        argument: *mut c_void,
        create_flags: u32,
        zero_bits: usize,
        stack_size: usize,
        maximum_stack_size: usize,
        attribute_list: *mut c_void,
    ) -> NtStatus;

    pub fn NtResumeThread(thread_handle: Handle, previous_suspend_count: *mut u32) -> NtStatus;

    pub fn NtWaitForSingleObject(
        handle: Handle,
        alertable: Boolean,
        timeout: *const i64,
    ) -> NtStatus;

    pub fn NtClose(handle: Handle) -> NtStatus;

    pub fn NtSetInformationThread(
        thread_handle: Handle,
        thread_information_class: u32,
        thread_information: *const c_void,
        thread_information_length: u32,
    ) -> NtStatus;
}
