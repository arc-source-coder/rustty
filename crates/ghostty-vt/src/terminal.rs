use std::{
    fmt::{Debug, Formatter, Result},
    ops::Deref,
};

use core::ffi::{c_int, c_void};

use crate::*;

/// Events produced by the terminal during `feed()`.
/// Consumed via `Terminal::drain_events()` after each feed cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VtEvent {
    Bell,
    TitleChanged(String),
    DeviceResponse(Vec<u8>),
}

// C callback trampolines — push to the Vec<VtEvent> via userdata pointer.
// Safety: these are only called during `feed()`, which holds `&mut self`
// on the single UI thread — no concurrent access to the queue.
// Wrapped in catch_unwind to prevent unwinding across the FFI boundary.

unsafe extern "C" fn bell_trampoline(userdata: *mut c_void) {
    let _ = std::panic::catch_unwind(|| {
        let events = unsafe { &mut *(userdata as *mut Vec<VtEvent>) };
        events.push(VtEvent::Bell);
    });
}

unsafe extern "C" fn title_trampoline(userdata: *mut c_void, ptr: *const u8, len: usize) {
    let _ = std::panic::catch_unwind(|| {
        let events = unsafe { &mut *(userdata as *mut Vec<VtEvent>) };
        // Guard: from_raw_parts requires non-null ptr even when len == 0.
        // Zig slices always have non-null .ptr, but defend against edge cases.
        let title = if len == 0 || ptr.is_null() {
            String::new()
        } else {
            let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
            String::from_utf8_lossy(bytes).into_owned()
        };
        events.push(VtEvent::TitleChanged(title));
    });
}

unsafe extern "C" fn response_trampoline(userdata: *mut c_void, ptr: *const u8, len: usize) {
    let _ = std::panic::catch_unwind(|| {
        let events = unsafe { &mut *(userdata as *mut Vec<VtEvent>) };
        if len == 0 || ptr.is_null() {
            return;
        }
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        events.push(VtEvent::DeviceResponse(bytes.to_vec()));
    });
}

/// Safe wrapper around the Ghostty VT terminal handle.
///
/// `Send` but not `Sync` — all access must go through an external
/// `Mutex<Terminal>` when shared between threads.
/// All mutating operations take `&mut self`; read operations take `&self`.
/// Use `render_frame()` to access render state.
pub struct Terminal {
    handle: *mut c_void,
    events: Box<Vec<VtEvent>>,
}

// Safety: Terminal wraps a Zig-allocated handle that is not thread-safe.
// However, all access is protected by an external Mutex<Terminal>
// Only one thread accesses the handle at a time.
// The Box<Vec<VtEvent>> callback target is heap-stable across moves.
// Cell<bool> is Send. The raw *mut c_void is not Send by default, but
// the Mutex guarantees no concurrent access.
unsafe impl Send for Terminal {}

impl Terminal {
    /// Create a new terminal with the given dimensions and default colors.
    /// Returns `None` if allocation fails.
    pub fn new(cols: u16, rows: u16, fg: ColorRGB, bg: ColorRGB) -> Option<Self> {
        let handle =
            unsafe { ghostty_vt_terminal_new(cols, rows, fg.r, fg.g, fg.b, bg.r, bg.g, bg.b) };
        if handle.is_null() {
            return None;
        }

        let mut events = Box::new(Vec::new());

        // Register callbacks with a pointer to the heap-stable Vec.
        // The Box indirection ensures the Vec's address is stable
        // even if Terminal is moved.
        let userdata = &mut *events as *mut Vec<VtEvent> as *mut c_void;
        unsafe {
            ghostty_vt_terminal_set_callbacks(
                handle,
                userdata,
                Some(bell_trampoline as BellCallback),
                Some(title_trampoline as TitleCallback),
                Some(response_trampoline as ResponseCallback),
            );
        }

        Some(Terminal { handle, events })
    }

    /// Feed raw bytes (PTY output) to the terminal emulator.
    /// Side-effect events (bell, title change) are queued internally;
    /// call `drain_events()` after feeding to process them.
    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        unsafe {
            ghostty_vt_terminal_feed(self.handle, bytes.as_ptr(), bytes.len());
        }
    }

    /// Resize the terminal grid.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        unsafe {
            ghostty_vt_terminal_resize(self.handle, cols, rows);
        }
    }

    /// Set cell pixel dimensions. Called by the renderer whenever
    /// font metrics change. Needed for size report responses (CSI 14t, 16t).
    pub fn set_cell_size(&mut self, width_px: u16, height_px: u16) {
        unsafe {
            ghostty_vt_terminal_set_cell_size(self.handle, width_px, height_px);
        }
    }

    /// Drain all events produced during the last `feed()` call(s).
    /// Writes events to the provided bufer and clears the internal queue.
    ///
    /// Note: cannot be called while a `RenderFrame` is alive —
    /// the frame borrows `&self` and `drain_events` needs `&mut self`.
    /// This is enforced statically by the borrow checker.
    pub fn drain_events(&mut self, buf: &mut Vec<VtEvent>) {
        buf.clear();
        std::mem::swap(&mut *self.events, buf);
    }

    /// Update the persistent render state and return a detached frame accessor.
    ///
    /// Calls `render_update()` then constructs a `RenderFrame` holding the
    /// raw handle pointer. The frame can be used after the `MutexGuard<Terminal>`
    /// that gave us `&mut self` is dropped — the caller controls the lock scope.
    ///
    /// The caller MUST query any non-RenderState fields (`scrollbar_info`,
    /// `is_alternate_screen`, etc.) before the guard drops.
    /// `frame.*()` calls are safe after the lock drops because RenderState
    /// is only mutated by render_update() on the UI thread.
    ///
    /// SAFETY: `render_frame()` takes `&mut self`, so the borrow checker prevents
    /// creating a second frame while the `MutexGuard<Terminal>` is still held.
    /// However, once the guard drops the frame becomes detached, and the borrow
    /// checker can no longer prevent a second `render_frame()` call on the same
    /// guard (in a subsequent lock scope) before the first frame is dropped.
    ///
    /// Callers MUST ensure the previous `RenderFrame` is dropped before calling
    /// `render_frame()` again. Violating this causes the first frame's `Drop` to clear
    /// dirty flags that the second frame still needs, producing incorrect rendering.
    ///
    /// The architectural invariant (only prepaint creates frames, and prepaint drops
    /// the frame before its next invocation) satisfies this requirement.
    ///
    /// When `RenderFrame` is dropped, dirty flags are cleared automatically.
    pub fn render_frame(&mut self) -> RenderFrame {
        // Ignore return: a failed update (allocation error inside Ghostty)
        // leaves RenderState in its previous valid state. We hand out a
        // frame over stale-but-consistent data rather than crashing or
        // skipping the frame. The dirty flags are unchanged, so the next
        // successful update will re-render the affected rows.
        unsafe {
            ghostty_vt_terminal_render_update(self.handle);
        }
        RenderFrame {
            handle: self.handle,
        }
    }

    // --- Mode flag queries (read-only, &self) ---

    /// Current mouse event reporting mode.
    pub fn mouse_mode(&self) -> MouseMode {
        MouseMode::from_raw(unsafe { ghostty_vt_terminal_get_mouse_mode(self.handle) })
    }

    /// Current mouse coordinate format.
    pub fn mouse_format(&self) -> MouseFormat {
        MouseFormat::from_raw(unsafe { ghostty_vt_terminal_get_mouse_format(self.handle) })
    }

    /// Whether bracketed paste mode is active.
    pub fn is_bracketed_paste(&self) -> bool {
        unsafe { ghostty_vt_terminal_is_bracketed_paste(self.handle) != 0 }
    }

    /// Kitty keyboard protocol flags (5-bit bitfield).
    pub fn kitty_keyboard_flags(&self) -> u8 {
        unsafe { ghostty_vt_terminal_get_kitty_keyboard_flags(self.handle) }
    }

    /// Whether synchronized output mode (DEC 2026) is active.
    pub fn is_synchronized_output(&self) -> bool {
        unsafe { ghostty_vt_terminal_is_synchronized_output(self.handle) != 0 }
    }

    /// Reset synchronized output mode (DEC 2026).
    /// Used by the sync-output safety timer to unfreeze misbehaving programs.
    pub fn reset_synchronized_output(&mut self) {
        unsafe { ghostty_vt_terminal_reset_synchronized_output(self.handle) }
    }

    /// Whether focus event mode (DEC 1004) is active.
    pub fn is_focus_event_mode(&self) -> bool {
        unsafe { ghostty_vt_terminal_is_focus_event_mode(self.handle) != 0 }
    }

    /// Whether the alternate screen is active.
    pub fn is_alternate_screen(&self) -> bool {
        unsafe { ghostty_vt_terminal_is_alternate_screen(self.handle) != 0 }
    }

    /// Snapshot all input-relevant mode flags into an [`InputOpts`].
    ///
    /// Must be called under the terminal mutex. The returned value is
    /// plain data — no terminal reference is retained. Callers can drop
    /// the lock immediately after this call and encode outside the lock
    /// using [`encode_key`] / [`encode_mouse`].
    pub fn input_opts(&self) -> InputOpts {
        let raw = unsafe { ghostty_vt_terminal_get_input_opts(self.handle) };
        InputOpts {
            cursor_key_application: raw.cursor_key_application != 0,
            keypad_key_application: raw.keypad_key_application != 0,
            ignore_keypad_with_numlock: raw.ignore_keypad_with_numlock != 0,
            alt_esc_prefix: raw.alt_esc_prefix != 0,
            modify_other_keys_state_2: raw.modify_other_keys_state_2 != 0,
            kitty_flags: raw.kitty_flags,
            mouse_event: MouseMode::from_raw(raw.mouse_event),
            mouse_format: MouseFormat::from_raw(raw.mouse_format),
            bracketed_paste: raw.bracketed_paste != 0,
            focus_event_mode: raw.focus_event_mode != 0,
        }
    }

    // --- Viewport scroll (mutating, &mut self) ---

    /// Scroll the viewport by delta rows.
    /// Negative = up (towards history), positive = down.
    pub fn scroll_viewport(&mut self, delta: i32) {
        unsafe { ghostty_vt_terminal_scroll_viewport(self.handle, delta) }
    }

    /// Scroll the viewport to the top of scrollback.
    pub fn scroll_to_top(&mut self) {
        unsafe { ghostty_vt_terminal_scroll_viewport_top(self.handle) }
    }

    /// Scroll the viewport to the bottom (active area).
    pub fn scroll_to_bottom(&mut self) {
        unsafe { ghostty_vt_terminal_scroll_viewport_bottom(self.handle) }
    }

    /// Scroll viewport to an absolute row offset from the top of scrollback.
    pub fn scroll_to_row(&mut self, row: u64) {
        unsafe { ghostty_vt_terminal_scroll_to_row(self.handle, row) }
    }

    /// Read-only - Whether the viewport is at the bottom (active area).
    pub fn viewport_is_bottom(&self) -> bool {
        unsafe { ghostty_vt_terminal_viewport_is_bottom(self.handle) != 0 }
    }

    /// Query scrollbar positioning info (total rows, viewport offset, viewport size).
    /// Read-only - Can be called under the mutex for snapshot coherence.
    pub fn scrollbar_info(&self) -> ScrollbarInfo {
        let mut out = ScrollbarInfo::default();
        unsafe { ghostty_vt_terminal_scrollbar_info(self.handle, &mut out) };
        out
    }

    // --- Selection (set/clear mutate, text read is &self) ---

    /// Set a selection in viewport coordinates (0-indexed).
    /// Returns `true` on success.
    pub fn set_selection(
        &mut self,
        start_x: u16,
        start_y: u32,
        end_x: u16,
        end_y: u32,
        rectangular: bool,
    ) -> bool {
        let rc = unsafe {
            ghostty_vt_terminal_set_selection(
                self.handle,
                start_x,
                start_y,
                end_x,
                end_y,
                rectangular as u8,
            )
        };
        rc == 0
    }

    /// Clear any active selection.
    pub fn clear_selection(&mut self) {
        unsafe { ghostty_vt_terminal_clear_selection(self.handle) }
    }

    /// Get the currently selected text. Returns `None` if no selection.
    /// The returned `SelectionText` frees its memory on drop.
    pub fn selection_text(&self) -> Option<SelectionText> {
        let mut len: usize = 0;
        let ptr = unsafe { ghostty_vt_terminal_get_selection_text(self.handle, &mut len) };
        if ptr.is_null() {
            return None;
        }
        Some(SelectionText { ptr, len })
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe {
            ghostty_vt_terminal_free(self.handle);
        }
    }
}

/// Detached render state accessor — holds a raw Zig handle pointer.
///
/// Created by `Terminal::render_frame()` which calls `render_update()` and
/// returns this with the mutex already dropped. The handle pointer is
/// Zig-allocated and heap-stable.
///
/// Cell and style data returned by `row_raw()` and `row_styles()` are
/// zero-copy slices into RenderState memory. These pointers are stable
/// from the moment `render_frame()` returns until the next `render_update()`.
/// Thus, it is stable for the entire frame (when frame drops, dirty flags clear).
///
/// This detached design allows the mutex to be dropped immediately after
/// `render_update()`, so the read thread can call `feed()` without blocking
/// on text run building or cursor generation.
pub struct RenderFrame {
    handle: *mut c_void,
}

impl RenderFrame {
    /// Get raw cell data for a row (zero-copy pointer into Zig memory).
    /// Returns `None` if row is out of bounds.
    ///
    /// Safety: The returned slice borrows from RenderState memory
    /// that is stable for the lifetime of this RenderFrame.
    pub fn row_raw(&self, y: u16) -> Option<&[RawCell]> {
        let mut len: u16 = 0;
        let ptr = unsafe { ghostty_vt_terminal_render_row_raw(self.handle, y, &mut len) };
        if ptr.is_null() || len == 0 {
            return None;
        }
        // Safety: RawCell is #[repr(transparent)] over u64.
        // The pointer comes from RenderState memory which is stable
        // for the frame's lifetime. The len is verified by the Zig side.
        Some(unsafe { std::slice::from_raw_parts(ptr as *const RawCell, len as usize) })
    }

    /// Get style data for a row (zero-copy pointer into Zig memory).
    /// Returns `None` if row is out of bounds.
    ///
    /// Individual entries are only valid when the corresponding
    /// RawCell's style_id != 0 or content_tag is bg_color_*.
    pub fn row_styles(&self, y: u16) -> Option<&[CellStyle]> {
        let mut len: u16 = 0;
        let ptr = unsafe { ghostty_vt_terminal_render_row_styles(self.handle, y, &mut len) };
        if ptr.is_null() || len == 0 {
            return None;
        }
        // Safety: The Zig FFI returns *const CellStyle (the Zig extern struct).
        // Rust's CellStyle is #[repr(C)] with the same layout, verified by
        // Zig comptime assertions. Pointer is into RenderState memory,
        // stable for the frame's lifetime.
        Some(unsafe { std::slice::from_raw_parts(ptr, len as usize) })
    }

    /// Get grapheme codepoints for a multi-codepoint cluster cell (on-demand).
    /// Returns a slice into the Zig-side scratch buffer (widened u21→u32).
    ///
    /// Returns `None` if the cell is not a grapheme cluster (`has_grapheme() == false`).
    ///
    /// SAFETY: The returned slice points into a single per-handle scratch buffer
    /// that is overwritten on every call. This means:
    ///   - The slice is **only valid until the next `cell_grapheme()` call**.
    ///   - Holding two slices from two calls simultaneously is **undefined behaviour**.
    ///
    /// This is safe within `build_row_runs` because the loop consumes
    /// each slice (pushes chars to `cell_text`) before advancing to
    /// the next cell, which is the only call site.
    /// Do not refactor the call site without re-checking this invariant.
    pub fn cell_grapheme(&self, row: u16, col: u16) -> Option<&[u32]> {
        let mut len: u8 = 0;
        let ptr =
            unsafe { ghostty_vt_terminal_render_cell_grapheme(self.handle, row, col, &mut len) };
        if ptr.is_null() || len == 0 {
            return None;
        }
        // SAFETY: Zig widens u21→u32 into `handle.grapheme_buf`, a heap-stable
        // scratch buffer that is only mutated by this function. The caller
        // (build_row_runs) consumes the slice in the same iteration step before
        // issuing any further cell_grapheme() call, so no aliasing occurs.
        Some(unsafe { std::slice::from_raw_parts(ptr, len as usize) })
    }

    /// Get the full 256-entry palette (zero-copy pointer into sidecar).
    /// The sidecar is refreshed during render_update().
    pub fn palette(&self) -> &[ColorRGB; 256] {
        let ptr = unsafe { ghostty_vt_terminal_render_palette(self.handle) };
        // Safety: palette_cache is always initialized after render_update().
        // 256 × ColorRGB (3 bytes each) = 768 bytes.
        unsafe { &*(ptr as *const [ColorRGB; 256]) }
    }

    /// Current dirty state of the render data.
    #[inline(always)]
    pub fn dirty(&self) -> DirtyState {
        let raw = unsafe { ghostty_vt_terminal_render_dirty(self.handle) };
        DirtyState::from_raw(raw)
    }

    /// Number of rows in the current render state.
    #[inline(always)]
    pub fn rows(&self) -> u16 {
        unsafe { ghostty_vt_terminal_render_rows(self.handle) }
    }

    /// Number of columns in the current render state.
    #[inline(always)]
    pub fn cols(&self) -> u16 {
        unsafe { ghostty_vt_terminal_render_cols(self.handle) }
    }

    /// Whether a specific row has changed since last clear.
    #[inline(always)]
    pub fn row_dirty(&self, y: u16) -> bool {
        unsafe { ghostty_vt_terminal_render_row_dirty(self.handle, y) != 0 }
    }

    /// Get selection range for a row. Returns `Some((start_x, end_x))` if
    /// the row has an active selection, `None` otherwise.
    pub fn row_selection(&self, y: u16) -> Option<(u16, u16)> {
        let mut start_x: u16 = 0;
        let mut end_x: u16 = 0;
        let has = unsafe {
            ghostty_vt_terminal_render_row_selection(self.handle, y, &mut start_x, &mut end_x)
        };
        if has != 0 {
            Some((start_x, end_x))
        } else {
            None
        }
    }

    /// Current cursor state.
    pub fn cursor(&self) -> CursorState {
        let mut out = CursorState::default();
        unsafe { ghostty_vt_terminal_render_cursor(self.handle, &mut out) };
        out
    }

    /// Current terminal colors (foreground, background, cursor).
    pub fn colors(&self) -> ColorState {
        let mut out = ColorState::default();
        unsafe { ghostty_vt_terminal_render_colors(self.handle, &mut out) };
        out
    }
}

impl Drop for RenderFrame {
    fn drop(&mut self) {
        unsafe {
            ghostty_vt_terminal_render_clear_dirty(self.handle);
        }
    }
}

/// Owned selection text. Frees the underlying Zig-allocated buffer on drop.
pub struct SelectionText {
    ptr: *const u8,
    len: usize,
}

impl SelectionText {
    /// View the selection text as a `&str`.
    pub fn as_str(&self) -> &str {
        // Safety: Ghostty produces valid UTF-8 selection text (stores
        // codepoints internally and serializes via std.unicode.utf8Encode).
        unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(self.ptr, self.len)) }
    }
}

impl Deref for SelectionText {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl Debug for SelectionText {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        f.debug_tuple("SelectionText")
            .field(&self.as_str())
            .finish()
    }
}

impl Drop for SelectionText {
    fn drop(&mut self) {
        unsafe { ghostty_vt_bytes_free(self.ptr, self.len) }
    }
}

/// Snapshot of all terminal input mode flags.
///
/// Captured once under the terminal mutex via [`Terminal::input_opts()`].
/// Encoding functions take this by value — all encoding work is lock-free.
///
/// This is a throwaway created fresh per input event, never stored.
#[derive(Clone, Copy, Debug)]
pub struct InputOpts {
    // Key encoding — mirrors key_encode.Options fields
    pub cursor_key_application: bool,
    pub keypad_key_application: bool,
    pub ignore_keypad_with_numlock: bool,
    pub alt_esc_prefix: bool,
    pub modify_other_keys_state_2: bool,
    /// Kitty keyboard protocol flags (packed u5, widened to u8).
    pub kitty_flags: u8,
    // Mouse encoding
    pub mouse_event: MouseMode,
    pub mouse_format: MouseFormat,
    // Other
    pub bracketed_paste: bool,
    pub focus_event_mode: bool,
}

impl InputOpts {
    fn to_c(self) -> InputOptsC {
        InputOptsC {
            cursor_key_application: self.cursor_key_application as u8,
            keypad_key_application: self.keypad_key_application as u8,
            ignore_keypad_with_numlock: self.ignore_keypad_with_numlock as u8,
            alt_esc_prefix: self.alt_esc_prefix as u8,
            modify_other_keys_state_2: self.modify_other_keys_state_2 as u8,
            kitty_flags: self.kitty_flags,
            mouse_event: self.mouse_event.to_raw(),
            mouse_format: self.mouse_format.to_raw(),
            bracketed_paste: self.bracketed_paste as u8,
            focus_event_mode: self.focus_event_mode as u8,
        }
    }
}

/// Encode a key event using a pre-captured [`InputOpts`] snapshot.
/// No terminal handle or lock needed — pure computation.
///
/// Returns the number of bytes written into `buf`, or 0 if the event
/// produces no terminal output.
pub fn encode_key(
    opts: InputOpts,
    key: i32,
    mods: u16,
    action: u8,
    text: &[u8],
    unshifted_codepoint: u32,
    buf: &mut [u8],
) -> usize {
    let text_ptr = if text.is_empty() {
        std::ptr::null()
    } else {
        text.as_ptr()
    };
    unsafe {
        ghostty_vt_encode_key(
            opts.to_c(),
            key as c_int,
            mods,
            action,
            text_ptr,
            text.len(),
            unshifted_codepoint,
            buf.as_mut_ptr(),
            buf.len(),
        )
    }
}

/// Encode a mouse event using a pre-captured [`InputOpts`] snapshot.
/// No terminal handle or lock needed — pure computation.
///
/// Returns the number of bytes written into `buf`, or 0 if mouse
/// reporting is disabled or the event produces no output.
pub fn encode_mouse(
    opts: InputOpts,
    button: u8,
    action: u8,
    mods: u8,
    x: u16,
    y: u16,
    buf: &mut [u8],
) -> usize {
    unsafe {
        ghostty_vt_encode_mouse(
            opts.to_c(),
            button,
            action,
            mods,
            x,
            y,
            buf.as_mut_ptr(),
            buf.len(),
        )
    }
}

/// Resolve a W3C key code string to a Ghostty Key enum value (c_int).
/// Returns `None` if the code is unrecognized.
///
/// This is a pure function — it doesn't need a terminal instance.
/// Thread-safe (no mutable state).
///
/// Example W3C codes: "KeyA", "Enter", "ArrowLeft", "F1", "Digit0"
pub fn key_from_w3c(code: &str) -> Option<i32> {
    let result = unsafe { ghostty_vt_key_from_w3c(code.as_ptr(), code.len()) };
    if result < 0 { None } else { Some(result) }
}
