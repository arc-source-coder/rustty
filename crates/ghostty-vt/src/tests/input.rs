use crate::*;

const DEFAULT_FG: (u8, u8, u8) = (0xDD, 0xDD, 0xDD);
const DEFAULT_BG: (u8, u8, u8) = (0x1E, 0x1E, 0x2E);

#[test]
fn test_encode_key_basic() {
    let ptr = unsafe {
        ghostty_vt_terminal_new(
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
    let opts = unsafe { ghostty_vt_terminal_get_input_opts(ptr) };
    
    let mut buf = [0u8; 128];
    // Encode enter press.
    // Key.enter = 58 in Ghostty's Key enum (unidentified=0 through enter=58)
    let n = unsafe {
        ghostty_vt_encode_key(
            opts,
            58, // Key.enter
            0,  // no mods
            1,  // press
            std::ptr::null(),
            0,
            0,
            buf.as_mut_ptr(),
            buf.len(),
        )
    };
    // Enter should produce \r (0x0D)
    assert!(n > 0, "expected n > 0, got n = {}", n);
    // Verify it's \r
    assert_eq!(buf[0], 0x0D);
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_encode_mouse_sgr() {
    let ptr = unsafe {
        ghostty_vt_terminal_new(
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
    // Enable SGR mouse: CSI ?1003h (any-event) + CSI ?1006h (SGR format)
    let seq = b"\x1b[?1003h\x1b[?1006h";
    unsafe { ghostty_vt_terminal_feed(ptr, seq.as_ptr(), seq.len()) };
    
    let opts = unsafe { ghostty_vt_terminal_get_input_opts(ptr) };

    let mut buf = [0u8; 64];
    // Left button press at (5, 10)
    let n = unsafe {
        ghostty_vt_encode_mouse(opts, 0, 0, 0, 5, 10, buf.as_mut_ptr(), buf.len())
    };
    assert!(n > 0);
    let output = std::str::from_utf8(&buf[..n]).unwrap();
    // SGR format: \x1b[<0;6;11M (button 0, 1-indexed coords)
    assert_eq!(output, "\x1b[<0;6;11M");

    // Left button release at (5, 10) — SGR uses 'm' for release
    let n = unsafe {
        ghostty_vt_encode_mouse(opts, 0, 1, 0, 5, 10, buf.as_mut_ptr(), buf.len())
    };
    let output = std::str::from_utf8(&buf[..n]).unwrap();
    assert_eq!(output, "\x1b[<0;6;11m");

    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_encode_mouse_x10() {
    let ptr = unsafe {
        ghostty_vt_terminal_new(
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
    // Enable X10 mouse: CSI ?9h
    let seq = b"\x1b[?9h";
    unsafe { ghostty_vt_terminal_feed(ptr, seq.as_ptr(), seq.len()) };
    
    let opts = unsafe { ghostty_vt_terminal_get_input_opts(ptr) };

    let mut buf = [0u8; 64];
    // Left button press at (0, 0)
    let n = unsafe {
        ghostty_vt_encode_mouse(opts, 0, 0, 0, 0, 0, buf.as_mut_ptr(), buf.len())
    };
    assert!(n > 0);
    assert_eq!(n, 6); // \x1b[M + button + x + y
    assert_eq!(buf[0], 0x1b);
    assert_eq!(buf[1], b'[');
    assert_eq!(buf[2], b'M');
    assert_eq!(buf[3], 32); // button 0 + 32
    assert_eq!(buf[4], 33); // x=0 + 32 + 1
    assert_eq!(buf[5], 33); // y=0 + 32 + 1

    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_key_from_w3c() {
    // Letters
    assert_eq!(key_from_w3c("KeyA"), Some(20)); // key_a
    assert_eq!(key_from_w3c("KeyZ"), Some(45)); // key_z

    // Named keys
    assert!(key_from_w3c("Enter").is_some());
    assert!(key_from_w3c("Escape").is_some());
    assert!(key_from_w3c("ArrowLeft").is_some());
    assert!(key_from_w3c("F1").is_some());

    // Unknown
    assert_eq!(key_from_w3c("FooBar"), None);
    assert_eq!(key_from_w3c(""), None);
}
