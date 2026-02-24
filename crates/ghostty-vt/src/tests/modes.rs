use crate::*;

#[test]
fn test_mode_flags_default() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    // Defaults: no mouse, no bracketed paste, no kitty flags
    assert_eq!(unsafe { ghostty_vt_terminal_get_mouse_mode(ptr) }, 0);
    assert_eq!(unsafe { ghostty_vt_terminal_get_mouse_format(ptr) }, 0);
    assert_eq!(unsafe { ghostty_vt_terminal_is_bracketed_paste(ptr) }, 0);
    assert_eq!(
        unsafe { ghostty_vt_terminal_get_kitty_keyboard_flags(ptr) },
        0
    );
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_bracketed_paste_enabled() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    // CSI ?2004h enables bracketed paste
    let seq = b"\x1b[?2004h";
    unsafe { ghostty_vt_terminal_feed(ptr, seq.as_ptr(), seq.len()) };
    assert_eq!(unsafe { ghostty_vt_terminal_is_bracketed_paste(ptr) }, 1);
    // CSI ?2004l disables it
    let seq_off = b"\x1b[?2004l";
    unsafe { ghostty_vt_terminal_feed(ptr, seq_off.as_ptr(), seq_off.len()) };
    assert_eq!(unsafe { ghostty_vt_terminal_is_bracketed_paste(ptr) }, 0);
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_mouse_mode_enabled() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    // CSI ?1003h enables any-event mouse tracking
    let seq = b"\x1b[?1003h";
    unsafe { ghostty_vt_terminal_feed(ptr, seq.as_ptr(), seq.len()) };
    assert_eq!(unsafe { ghostty_vt_terminal_get_mouse_mode(ptr) }, 4); // any=4
    // CSI ?1006h enables SGR format
    let seq2 = b"\x1b[?1006h";
    unsafe { ghostty_vt_terminal_feed(ptr, seq2.as_ptr(), seq2.len()) };
    assert_eq!(unsafe { ghostty_vt_terminal_get_mouse_format(ptr) }, 2); // sgr=2
    unsafe { ghostty_vt_terminal_free(ptr) };
}
