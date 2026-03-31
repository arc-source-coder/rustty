use crate::*;
use core::option::Option::None;
use std::sync::atomic::{AtomicU32, Ordering};

const DEFAULT_FG: (u8, u8, u8) = (0xDD, 0xDD, 0xDD);
const DEFAULT_BG: (u8, u8, u8) = (0x1E, 0x1E, 0x2E);

#[test]
fn test_new_free() {
    let ptr = unsafe {
        ghostty_terminal_new(
            80,
            24,
            DEFAULT_FG.0,
            DEFAULT_FG.1,
            DEFAULT_FG.2,
            DEFAULT_BG.0,
            DEFAULT_BG.1,
            DEFAULT_BG.2,
        )
    };
    assert!(!ptr.is_null());
    unsafe { ghostty_terminal_free(ptr) };
}

#[test]
fn test_feed_ascii() {
    let ptr = unsafe {
        ghostty_terminal_new(
            80,
            24,
            DEFAULT_FG.0,
            DEFAULT_FG.1,
            DEFAULT_FG.2,
            DEFAULT_BG.0,
            DEFAULT_BG.1,
            DEFAULT_BG.2,
        )
    };
    let text = b"Hello, world!";
    let rc = unsafe { ghostty_terminal_feed(ptr, text.as_ptr(), text.len()) };
    assert_eq!(rc, 0);
    unsafe { ghostty_terminal_free(ptr) };
}

#[test]
fn test_resize() {
    let ptr = unsafe {
        ghostty_terminal_new(
            80,
            24,
            DEFAULT_FG.0,
            DEFAULT_FG.1,
            DEFAULT_FG.2,
            DEFAULT_BG.0,
            DEFAULT_BG.1,
            DEFAULT_BG.2,
        )
    };
    let rc = unsafe { ghostty_terminal_resize(ptr, 120, 40) };
    assert_eq!(rc, 0);
    unsafe { ghostty_terminal_free(ptr) };
}

static BELL_COUNT: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" fn bell_handler(_: *mut c_void) {
    BELL_COUNT.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn test_bell_callback() {
    BELL_COUNT.store(0, Ordering::SeqCst);
    let ptr = unsafe {
        ghostty_terminal_new(
            80,
            24,
            DEFAULT_FG.0,
            DEFAULT_FG.1,
            DEFAULT_FG.2,
            DEFAULT_BG.0,
            DEFAULT_BG.1,
            DEFAULT_BG.2,
        )
    };
    unsafe {
        ghostty_terminal_set_callbacks(ptr, std::ptr::null_mut(), Some(bell_handler), None, None);
    }
    // BEL character (0x07)
    let bel = [0x07u8];
    let rc = unsafe { ghostty_terminal_feed(ptr, bel.as_ptr(), bel.len()) };
    assert_eq!(rc, 0);
    assert_eq!(BELL_COUNT.load(Ordering::SeqCst), 1);
    unsafe { ghostty_terminal_free(ptr) };
}
