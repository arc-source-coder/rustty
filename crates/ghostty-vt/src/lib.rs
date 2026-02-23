pub type BellCallback = unsafe extern "C" fn(userdata: *mut core::ffi::c_void);
pub type TitleCallback =
    unsafe extern "C" fn(userdata: *mut core::ffi::c_void, ptr: *const u8, len: usize);

unsafe extern "C" {
    pub fn ghostty_vt_terminal_new(cols: u16, rows: u16) -> *mut core::ffi::c_void;
    pub fn ghostty_vt_terminal_free(terminal: *mut core::ffi::c_void);

    pub fn ghostty_vt_terminal_set_callbacks(
        terminal: *mut core::ffi::c_void,
        userdata: *mut core::ffi::c_void,
        bell: Option<BellCallback>,
        title: Option<TitleCallback>,
    );

    pub fn ghostty_vt_terminal_feed(
        terminal: *mut core::ffi::c_void,
        bytes: *const u8,
        len: usize,
    ) -> core::ffi::c_int;

    pub fn ghostty_vt_terminal_resize(
        terminal: *mut core::ffi::c_void,
        cols: u16,
        rows: u16,
    ) -> core::ffi::c_int;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn test_new_free() {
        let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
        assert!(!ptr.is_null());
        unsafe { ghostty_vt_terminal_free(ptr) };
    }

    #[test]
    fn test_feed_ascii() {
        let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
        let text = b"Hello, world!";
        let rc = unsafe { ghostty_vt_terminal_feed(ptr, text.as_ptr(), text.len()) };
        assert_eq!(rc, 0);
        unsafe { ghostty_vt_terminal_free(ptr) };
    }

    #[test]
    fn test_resize() {
        let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
        let rc = unsafe { ghostty_vt_terminal_resize(ptr, 120, 40) };
        assert_eq!(rc, 0);
        unsafe { ghostty_vt_terminal_free(ptr) };
    }

    static BELL_COUNT: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" fn bell_handler(_: *mut core::ffi::c_void) {
        BELL_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn test_bell_callback() {
        BELL_COUNT.store(0, Ordering::SeqCst);
        let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
        unsafe {
            ghostty_vt_terminal_set_callbacks(
                ptr,
                std::ptr::null_mut(),
                Some(bell_handler),
                None,
            );
        }
        // BEL character (0x07)
        let bel = [0x07u8];
        let rc = unsafe { ghostty_vt_terminal_feed(ptr, bel.as_ptr(), bel.len()) };
        assert_eq!(rc, 0);
        assert_eq!(BELL_COUNT.load(Ordering::SeqCst), 1);
        unsafe { ghostty_vt_terminal_free(ptr) };
    }
}
