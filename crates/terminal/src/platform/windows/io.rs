//! APC-based async IO helpers over ntdll.
use super::ntdll::{FALSE, NtDelayExecution, NtStatus, TRUE};

pub fn alertable_wait(timeout_100ns: i64) -> NtStatus {
    unsafe { NtDelayExecution(TRUE, &timeout_100ns) }
}

pub fn sleep_100ns(timeout_100ns: i64) {
    let _ = unsafe { NtDelayExecution(FALSE, &timeout_100ns) };
}
