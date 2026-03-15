use crate::*;

const DEFAULT_FG: ColorRGB = ColorRGB {
    r: 0xDD,
    g: 0xDD,
    b: 0xDD,
};
const DEFAULT_BG: ColorRGB = ColorRGB {
    r: 0x1E,
    g: 0x1E,
    b: 0x2E,
};

#[test]
fn new_and_drop() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).expect("failed to create terminal");
    drop(term);
}

#[test]
fn feed_ascii() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"Hello, world!");
}

#[test]
fn feed_empty_is_noop() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"");
}

#[test]
fn resize() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.resize(120, 40);
}

#[test]
fn bell_event() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"\x07");
    let mut events: Vec<VtEvent> = Vec::new();
    term.drain_events(&mut events);
    assert_eq!(events, vec![VtEvent::Bell]);
}

#[test]
fn title_event() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    // OSC 0 (set title): ESC ] 0 ; title ST
    term.feed(b"\x1b]0;My Title\x1b\\");
    let mut events: Vec<VtEvent> = Vec::new();
    term.drain_events(&mut events);
    assert_eq!(events, vec![VtEvent::TitleChanged("My Title".to_string())]);
}

#[test]
fn drain_events_clears_queue() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"\x07");
    let mut events: Vec<VtEvent> = Vec::new();
    term.drain_events(&mut events);
    events.clear();
    term.drain_events(&mut events);
    assert!(events.is_empty());
}

#[test]
fn multiple_events_in_one_feed() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"\x07\x07\x1b]0;Title\x1b\\");
    let mut events: Vec<VtEvent> = Vec::new();
    term.drain_events(&mut events);
    assert_eq!(events.len(), 3);
    assert_eq!(events[0], VtEvent::Bell);
    assert_eq!(events[1], VtEvent::Bell);
    assert_eq!(events[2], VtEvent::TitleChanged("Title".to_string()));
}

#[test]
fn render_frame_dirty_lifecycle() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    {
        let frame = term.render_frame();
        // First update is always full dirty
        assert_eq!(frame.dirty(), DirtyState::Full);
        assert_eq!(frame.rows(), 24);
        assert_eq!(frame.cols(), 80);
    } // frame dropped — dirty cleared
    {
        let frame = term.render_frame();
        assert_eq!(frame.dirty(), DirtyState::Clean);
    }
}

#[test]
fn render_frame_partial_dirty() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    drop(term.render_frame()); // clear initial full dirty
    term.feed(b"Hello");
    let frame = term.render_frame();
    assert!(frame.dirty().is_dirty());
    assert!(frame.row_dirty(0));
}

#[test]
fn render_frame_cursor() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    let frame = term.render_frame();
    let cursor = frame.cursor();
    assert_eq!(cursor.x, 0);
    assert_eq!(cursor.y, 0);
    assert_eq!(cursor.in_viewport, 1);
    assert_eq!(cursor.visible, 1);
    assert_eq!(cursor.style, 1); // block
}

#[test]
fn render_frame_colors_and_palette() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    let frame = term.render_frame();
    let _colors = frame.colors();
    // Palette index 1 is red (#cc6666 in Ghostty defaults)
    let red = frame.palette()[1];
    assert_eq!(red.r, 204);
    assert_eq!(red.g, 102);
    assert_eq!(red.b, 102);
}

#[test]
fn row_raw_ascii() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"ABC");
    let frame = term.render_frame();
    let cells = frame.row_raw(0).expect("row 0 should exist");
    assert_eq!(cells.len(), 80);
    assert_eq!(cells[0].codepoint(), b'A' as u32);
    assert_eq!(cells[1].codepoint(), b'B' as u32);
    assert_eq!(cells[2].codepoint(), b'C' as u32);
    assert_eq!(cells[0].wide(), 0); // narrow
    assert!(!cells[0].has_grapheme()); // grapheme_len 0
}

#[test]
fn row_graphemes_zero_copy_stable_within_frame() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed("x\u{0301}".as_bytes());
    let frame = term.render_frame();
    let cells = frame.row_raw(0).unwrap();
    assert!(cells[0].has_grapheme());

    let row_g1 = frame.row_graphemes(0).expect("expected row grapheme data");
    let row_g2 = frame.row_graphemes(0).expect("expected row grapheme data");
    assert_eq!(row_g1.len(), cells.len());

    let g1 = unsafe { row_g1[0].as_slice() }.expect("expected grapheme data");
    let g2 = unsafe { row_g2[0].as_slice() }.expect("expected grapheme data");
    assert_eq!(g1, g2);
    assert!(g1.iter().all(|cp| (*cp >> 21) == 0));
}

#[test]
fn row_raw_styled() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    // SGR 1 (bold) + SGR 31 (red fg) + "X"
    term.feed(b"\x1b[1;31mX");
    let frame = term.render_frame();
    let cells = frame.row_raw(0).unwrap();
    let styles = frame.row_styles(0).unwrap();
    assert_eq!(cells[0].codepoint(), b'X' as u32);
    // Bold flag (bit 0)
    assert!(styles[0].is_bold());
    // Foreground: palette color, index 1 (red)
    assert_eq!(styles[0].fg.tag, 1); // palette
    assert_eq!(styles[0].fg.r, 1); // palette index
}

#[test]
fn row_raw_background_styled() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    // SGR 48;5;196 (palette background) + "X"
    term.feed(b"\x1b[48;5;196mX");
    let frame = term.render_frame();
    let cells = frame.row_raw(0).unwrap();
    let styles = frame.row_styles(0).unwrap();
    assert_eq!(cells[0].codepoint(), b'X' as u32);
    assert!(cells[0].style_id() != 0);
    assert_eq!(styles[0].bg.tag, 1); // palette
    assert_eq!(styles[0].bg.r, 196); // palette index
}

#[test]
fn row_raw_out_of_bounds_returns_none() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    let frame = term.render_frame();
    assert!(frame.row_raw(100).is_none());
    assert!(frame.row_graphemes(100).is_none());
}

#[test]
fn multiple_row_raw_calls_are_independent() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"Row0\r\nRow1");

    let frame = term.render_frame();
    // Each call returns an owned Vec — previous data is not overwritten
    let row0 = frame.row_raw(0).unwrap();
    let row1 = frame.row_raw(1).unwrap();

    assert_eq!(row0[0].codepoint(), b'R' as u32);
    assert_eq!(row0[3].codepoint(), b'0' as u32);
    assert_eq!(row1[0].codepoint(), b'R' as u32);
    assert_eq!(row1[3].codepoint(), b'1' as u32);
}

#[test]
fn row_selection_none_by_default() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    let frame = term.render_frame();
    assert!(frame.row_selection(0).is_none());
}

#[test]
fn default_modes() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    assert_eq!(term.mouse_mode(), MouseMode::None);
    assert_eq!(term.mouse_format(), MouseFormat::X10);
    assert!(!term.is_bracketed_paste());
    assert_eq!(term.kitty_keyboard_flags(), 0);
}

#[test]
fn bracketed_paste_toggle() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"\x1b[?2004h");
    assert!(term.is_bracketed_paste());
    term.feed(b"\x1b[?2004l");
    assert!(!term.is_bracketed_paste());
}

#[test]
fn mouse_mode_any_with_sgr_format() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"\x1b[?1003h");
    assert_eq!(term.mouse_mode(), MouseMode::Any);
    term.feed(b"\x1b[?1006h");
    assert_eq!(term.mouse_format(), MouseFormat::Sgr);
}

#[test]
fn scroll_no_crash() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    let newlines = "\n".repeat(50);
    term.feed(newlines.as_bytes());
    term.scroll_viewport(-5);
    term.scroll_to_top();
    term.scroll_to_bottom();
}

#[test]
fn selection_lifecycle() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"Hello, World!");
    assert!(term.set_selection(0, 0, 4, 0, false));
    {
        let frame = term.render_frame();
        let sel = frame.row_selection(0);
        assert_eq!(sel, Some((0, 4)));
    }
    let text = term.selection_text().expect("should have selection");
    assert_eq!(text.as_str(), "Hello");
    drop(text);
    term.clear_selection();
    {
        let frame = term.render_frame();
        assert!(frame.row_selection(0).is_none());
    }
}

#[test]
fn selection_text_deref() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"Hello");
    term.set_selection(0, 0, 4, 0, false);
    let text = term.selection_text().unwrap();
    // SelectionText derefs to &str
    assert!(text.starts_with("Hel"));
    assert_eq!(text.len(), 5);
}

#[test]
fn select_word_at_uses_ghostty_boundaries() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"foo,bar baz");

    assert!(term.select_word_at(1, 0));
    assert_eq!(term.selection_text().unwrap().as_str(), "foo");

    assert!(term.select_word_at(4, 0));
    assert_eq!(term.selection_text().unwrap().as_str(), "bar");

    assert!(term.select_word_at(8, 0));
    assert_eq!(term.selection_text().unwrap().as_str(), "baz");
}

#[test]
fn select_line_at_trims_whitespace_like_ghostty() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"   hello world   \r\n\tsecond line\t");

    assert!(term.select_line_at(0, 0));
    assert_eq!(term.selection_text().unwrap().as_str(), "hello world");

    assert!(term.select_line_at(0, 1));
    assert_eq!(term.selection_text().unwrap().as_str(), "second line");
}

#[test]
fn select_word_at_returns_false_for_unwritten_cells() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"abc");
    assert!(!term.select_word_at(10, 0));
}

#[test]
fn select_word_drag_expands_by_word_boundaries() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"foo bar baz");

    assert!(term.select_word_drag(5, 0, 9, 0));
    assert_eq!(term.selection_text().unwrap().as_str(), "bar baz");

    assert!(term.select_word_drag(5, 0, 1, 0));
    assert_eq!(term.selection_text().unwrap().as_str(), "foo bar");
}

#[test]
fn select_line_drag_expands_by_lines() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"line0\r\nline1\r\nline2");

    assert!(term.select_line_drag(1, 1, 1, 2));
    assert_eq!(term.selection_text().unwrap().as_str(), "line1\nline2");

    assert!(term.select_line_drag(1, 1, 1, 0));
    assert_eq!(term.selection_text().unwrap().as_str(), "line0\nline1");
}

#[test]
fn select_output_at_without_semantic_markers_returns_false() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.feed(b"plain text");
    assert!(!term.select_output_at(2, 0));
}

#[test]
fn no_selection_returns_none() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    assert!(term.selection_text().is_none());
}

#[test]
fn encode_enter_key() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    let opts = term.input_opts();
    let mut buf = [0u8; 128];
    // Key.enter = 58 in Ghostty's Key enum
    let n = encode_key(opts, 58, 0, 1, b"", 0, &mut buf);
    assert!(n > 0, "expected output bytes for enter");
    assert_eq!(buf[0], 0x0D); // \r
}
