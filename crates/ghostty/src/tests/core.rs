use crate::*;

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
    let p = unsafe { NonNull::new_unchecked(ptr) };
    unsafe { ghostty_terminal_free(p) };
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
    assert!(!ptr.is_null());
    let p = unsafe { NonNull::new_unchecked(ptr) };
    let rc = unsafe { ghostty_terminal_resize(p, 120, 40) };
    assert_eq!(rc, 0);
    unsafe { ghostty_terminal_free(p) };
}
