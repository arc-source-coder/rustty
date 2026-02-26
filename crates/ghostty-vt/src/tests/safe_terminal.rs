use crate::*;

#[test]
fn new_and_drop() {
    let term = Terminal::new(80, 24).expect("failed to create terminal");
    drop(term);
}

#[test]
fn feed_ascii() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.feed(b"Hello, world!");
}

#[test]
fn feed_empty_is_noop() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.feed(b"");
}

#[test]
fn resize() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.resize(120, 40);
}

#[test]
fn bell_event() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.feed(b"\x07");
    let events = term.drain_events();
    assert_eq!(events, vec![VtEvent::Bell]);
}

#[test]
fn title_event() {
    let mut term = Terminal::new(80, 24).unwrap();
    // OSC 0 (set title): ESC ] 0 ; title ST
    term.feed(b"\x1b]0;My Title\x1b\\");
    let events = term.drain_events();
    assert_eq!(events, vec![VtEvent::TitleChanged("My Title".to_string())]);
}

#[test]
fn drain_events_clears_queue() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.feed(b"\x07");
    let _ = term.drain_events();
    let events = term.drain_events();
    assert!(events.is_empty());
}

#[test]
fn multiple_events_in_one_feed() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.feed(b"\x07\x07\x1b]0;Title\x1b\\");
    let events = term.drain_events();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0], VtEvent::Bell);
    assert_eq!(events[1], VtEvent::Bell);
    assert_eq!(events[2], VtEvent::TitleChanged("Title".to_string()));
}

#[test]
fn render_frame_dirty_lifecycle() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.render_update();
    {
        let frame = term.begin_frame();
        // First update is always full dirty
        assert_eq!(frame.dirty(), DirtyState::Full);
        assert_eq!(frame.rows(), 24);
        assert_eq!(frame.cols(), 80);
    } // frame dropped — dirty cleared
    {
        let frame = term.begin_frame();
        assert_eq!(frame.dirty(), DirtyState::Clean);
    }
}

#[test]
fn render_frame_partial_dirty() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.render_update();
    drop(term.begin_frame()); // clear initial full dirty
    term.feed(b"Hello");
    term.render_update();
    let frame = term.begin_frame();
    assert!(frame.dirty().is_dirty());
    assert!(frame.row_dirty(0));
}

#[test]
fn render_frame_cursor() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.render_update();
    let frame = term.begin_frame();
    let cursor = frame.cursor();
    assert_eq!(cursor.x, 0);
    assert_eq!(cursor.y, 0);
    assert_eq!(cursor.in_viewport, 1);
    assert_eq!(cursor.visible, 1);
    assert_eq!(cursor.style, 1); // block
}

#[test]
fn render_frame_colors_and_palette() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.render_update();
    let frame = term.begin_frame();
    let _colors = frame.colors();
    // Palette index 1 is red (#cc6666 in Ghostty defaults)
    let red = frame.palette_color(1);
    assert_eq!(red.r, 204);
    assert_eq!(red.g, 102);
    assert_eq!(red.b, 102);
}

#[test]
fn row_cells_ascii() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.feed(b"ABC");
    term.render_update();
    let frame = term.begin_frame();
    let cells = frame.row_cells(0).expect("row 0 should exist");
    assert_eq!(cells.len(), 80);
    assert_eq!(cells[0].codepoint, b'A' as u32);
    assert_eq!(cells[1].codepoint, b'B' as u32);
    assert_eq!(cells[2].codepoint, b'C' as u32);
    assert_eq!(cells[0].wide, 0); // narrow
    assert_eq!(cells[0].grapheme_len, 0);
}

#[test]
fn row_cells_styled() {
    let mut term = Terminal::new(80, 24).unwrap();
    // SGR 1 (bold) + SGR 31 (red fg) + "X"
    term.feed(b"\x1b[1;31mX");
    term.render_update();
    let frame = term.begin_frame();
    let cells = frame.row_cells(0).unwrap();
    assert_eq!(cells[0].codepoint, b'X' as u32);
    // Bold flag (bit 0)
    assert!(cells[0].style_flags & 1 != 0);
    // Foreground: palette color, index 1 (red)
    assert_eq!(cells[0].fg_color_type, 1);
    assert_eq!(cells[0].fg_palette, 1);
}

#[test]
fn row_cells_out_of_bounds_returns_none() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.render_update();
    let frame = term.begin_frame();
    assert!(frame.row_cells(100).is_none());
}

#[test]
fn multiple_row_cells_calls_are_independent() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.feed(b"Row0\r\nRow1");
    term.render_update();
    let frame = term.begin_frame();
    // Each call returns an owned Vec — previous data is not overwritten
    let row0 = frame.row_cells(0).unwrap();
    let row1 = frame.row_cells(1).unwrap();

    assert_eq!(row0[0].codepoint, b'R' as u32);
    assert_eq!(row0[3].codepoint, b'0' as u32);
    assert_eq!(row1[0].codepoint, b'R' as u32);
    assert_eq!(row1[3].codepoint, b'1' as u32);
}

#[test]
fn row_selection_none_by_default() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.render_update();
    let frame = term.begin_frame();
    assert!(frame.row_selection(0).is_none());
}

#[test]
fn default_modes() {
    let term = Terminal::new(80, 24).unwrap();
    assert_eq!(term.mouse_mode(), MouseMode::None);
    assert_eq!(term.mouse_format(), MouseFormat::X10);
    assert!(!term.is_bracketed_paste());
    assert_eq!(term.kitty_keyboard_flags(), 0);
}

#[test]
fn bracketed_paste_toggle() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.feed(b"\x1b[?2004h");
    assert!(term.is_bracketed_paste());
    term.feed(b"\x1b[?2004l");
    assert!(!term.is_bracketed_paste());
}

#[test]
fn mouse_mode_any_with_sgr_format() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.feed(b"\x1b[?1003h");
    assert_eq!(term.mouse_mode(), MouseMode::Any);
    term.feed(b"\x1b[?1006h");
    assert_eq!(term.mouse_format(), MouseFormat::Sgr);
}

#[test]
fn scroll_no_crash() {
    let mut term = Terminal::new(80, 24).unwrap();
    let newlines = "\n".repeat(50);
    term.feed(newlines.as_bytes());
    term.scroll_viewport(-5);
    term.scroll_to_top();
    term.scroll_to_bottom();
}

#[test]
fn selection_lifecycle() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.feed(b"Hello, World!");
    assert!(term.set_selection(0, 0, 4, 0, false));
    term.render_update();
    {
        let frame = term.begin_frame();
        let sel = frame.row_selection(0);
        assert_eq!(sel, Some((0, 4)));
    }
    let text = term.selection_text().expect("should have selection");
    assert_eq!(text.as_str(), "Hello");
    drop(text);
    term.clear_selection();
    term.render_update();
    {
        let frame = term.begin_frame();
        assert!(frame.row_selection(0).is_none());
    }
}

#[test]
fn selection_text_deref() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.feed(b"Hello");
    term.set_selection(0, 0, 4, 0, false);
    let text = term.selection_text().unwrap();
    // SelectionText derefs to &str
    assert!(text.starts_with("Hel"));
    assert_eq!(text.len(), 5);
}

#[test]
fn no_selection_returns_none() {
    let term = Terminal::new(80, 24).unwrap();
    assert!(term.selection_text().is_none());
}

#[test]
fn encode_enter_key() {
    let term = Terminal::new(80, 24).unwrap();
    let mut buf = [0u8; 128];
    // Key.enter = 58 in Ghostty's Key enum
    let n = term.encode_key(58, 0, 1, b"", &mut buf);
    assert!(n > 0, "expected output bytes for enter");
    assert_eq!(buf[0], 0x0D); // \r
}
