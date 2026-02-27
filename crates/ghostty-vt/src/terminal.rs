use std::cell::Cell;
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
/// Use `begin_frame()` to access render state — while the returned
/// `RenderFrame` exists, no mutation can occur (enforced by borrow checker).
pub struct Terminal {
    handle: *mut c_void,
    events: Box<Vec<VtEvent>>,
    /// Prevents calling `begin_frame()` twice in the same scope.
    /// The first frame's Drop clears dirty flags, silently corrupting
    /// the second frame's view. debug_assert catches this during dev.
    frame_active: Cell<bool>,
}

// Safety: Terminal wraps a Zig-allocated handle that is not thread-safe.
// However, all access is protected by an external Mutex<Terminal>
// Only one thread accesses the handle at a time.
// The Box<Vec<VtEvent>> callback target is heap-stable across moves.
// Cell<bool> is Send. The raw *mut c_void is not Send by default, but
// the Mutex guarantees no concurrent access.
unsafe impl Send for Terminal {}

impl Terminal {
    /// Create a new terminal with the given dimensions.
    /// Returns `None` if allocation fails.
    pub fn new(cols: u16, rows: u16) -> Option<Self> {
        let handle = unsafe { ghostty_vt_terminal_new(cols, rows) };
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

        Some(Terminal {
            handle,
            events,
            frame_active: Cell::new(false),
        })
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
    /// Returns the events and clears the internal queue.
    ///
    /// Note: cannot be called while a `RenderFrame` is alive —
    /// the frame borrows `&self` and `drain_events` needs `&mut self`.
    /// This is enforced statically by the borrow checker.
    pub fn drain_events(&mut self) -> Vec<VtEvent> {
        std::mem::take(&mut *self.events)
    }

    /// Update the persistent render state from current terminal state.
    /// Call this after feeding bytes and before `begin_frame()`.
    pub fn render_update(&mut self) {
        unsafe {
            ghostty_vt_terminal_render_update(self.handle);
        }
    }

    /// Borrow render state for the current frame.
    /// While the returned `RenderFrame` exists, mutating methods
    /// (`feed`, `resize`, `render_update`, scroll, selection mutation)
    /// cannot be called — enforced by the borrow checker.
    ///
    /// When the `RenderFrame` is dropped, dirty flags are cleared
    /// automatically.
    ///
    /// # Panics (debug only)
    ///
    /// Debug-asserts if a `RenderFrame` is already active. Calling
    /// `begin_frame()` twice in the same scope would cause the first
    /// frame's Drop to clear dirty flags, silently corrupting the
    /// second frame's view.
    pub fn begin_frame(&self) -> RenderFrame<'_> {
        debug_assert!(
            !self.frame_active.get(),
            "begin_frame() called while a RenderFrame is already active"
        );
        self.frame_active.set(true);
        RenderFrame { terminal: self }
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

    // --- Key encoding (&self — reads mode state, no mutation) ---

    /// Encode a key event using the terminal's current mode state.
    /// Writes VT bytes into `buf` and returns the number of bytes written.
    /// Returns 0 if the key event produces no output.
    ///
    /// `key`: Ghostty Key enum integer value (see `ghostty/src/input/key.zig`).
    /// `mods`: modifier bitfield (Ghostty Mods packed u16).
    /// `action`: 0=release, 1=press, 2=repeat.
    /// `text`: UTF-8 text from the key event (for kitty protocol); empty if none.
    pub fn encode_key(
        &self,
        key: i32,
        mods: u16,
        action: u8,
        text: &[u8],
        buf: &mut [u8],
    ) -> usize {
        let text_ptr = if text.is_empty() {
            std::ptr::null()
        } else {
            text.as_ptr()
        };
        unsafe {
            ghostty_vt_terminal_encode_key(
                self.handle,
                key as c_int,
                mods,
                action,
                text_ptr,
                text.len(),
                buf.as_mut_ptr(),
                buf.len(),
            )
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe {
            ghostty_vt_terminal_free(self.handle);
        }
    }
}

/// Borrow guard for render state access.
///
/// Holds `&Terminal`, preventing mutation while render data is read.
/// When dropped, dirty flags are cleared automatically (calls
/// `render_clear_dirty` on the underlying handle) and the
/// `frame_active` guard is released.
///
/// Cell data returned by `row_cells()` is owned (`Vec<FlatCell>`) because
/// the Zig side uses a single shared flat_cells buffer that gets
/// overwritten on each FFI call. Owned copies are safe (~2KB per row memcpy).
pub struct RenderFrame<'a> {
    terminal: &'a Terminal,
}

impl<'a> RenderFrame<'a> {
    /// Current dirty state of the render data.
    pub fn dirty(&self) -> DirtyState {
        let raw = unsafe { ghostty_vt_terminal_render_dirty(self.terminal.handle) };
        DirtyState::from_raw(raw)
    }

    /// Number of rows in the current render state.
    pub fn rows(&self) -> u16 {
        unsafe { ghostty_vt_terminal_render_rows(self.terminal.handle) }
    }

    /// Number of columns in the current render state.
    pub fn cols(&self) -> u16 {
        unsafe { ghostty_vt_terminal_render_cols(self.terminal.handle) }
    }

    /// Whether a specific row has changed since last clear.
    pub fn row_dirty(&self, y: u16) -> bool {
        unsafe { ghostty_vt_terminal_render_row_dirty(self.terminal.handle, y) != 0 }
    }

    /// Current cursor state.
    pub fn cursor(&self) -> CursorState {
        let mut out = CursorState::default();
        unsafe { ghostty_vt_terminal_render_cursor(self.terminal.handle, &mut out) };
        out
    }

    /// Current terminal colors (foreground, background, cursor).
    pub fn colors(&self) -> ColorState {
        let mut out = ColorState::default();
        unsafe { ghostty_vt_terminal_render_colors(self.terminal.handle, &mut out) };
        out
    }

    /// Get a palette color by index (0–255).
    pub fn palette_color(&self, index: u8) -> ColorRGB {
        let mut out = ColorRGB::default();
        unsafe { ghostty_vt_terminal_render_palette_color(self.terminal.handle, index, &mut out) };
        out
    }

    /// Get flattened cell data for a row. Returns an owned `Vec<FlatCell>`.
    /// Returns `None` if the row is out of bounds.
    pub fn row_cells(&self, y: u16) -> Option<Vec<FlatCell>> {
        let mut len: u16 = 0;
        let ptr =
            unsafe { ghostty_vt_terminal_render_row_cells(self.terminal.handle, y, &mut len) };
        if ptr.is_null() || len == 0 {
            return None;
        }
        let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        Some(slice.to_vec())
    }

    /// Get selection range for a row. Returns `Some((start_x, end_x))` if
    /// the row has an active selection, `None` otherwise.
    pub fn row_selection(&self, y: u16) -> Option<(u16, u16)> {
        let mut start_x: u16 = 0;
        let mut end_x: u16 = 0;
        let has = unsafe {
            ghostty_vt_terminal_render_row_selection(
                self.terminal.handle,
                y,
                &mut start_x,
                &mut end_x,
            )
        };
        if has != 0 {
            Some((start_x, end_x))
        } else {
            None
        }
    }

    /// Get grapheme codepoints for a cell with a multi-codepoint cluster.
    /// Returns `None` if the cell has no grapheme (`grapheme_len == 0`) or
    /// is out of bounds.
    ///
    /// Returns owned data — the Zig side uses a shared buffer, so
    /// returning a borrowed slice would be unsound across multiple calls.
    pub fn cell_grapheme(&self, row: u16, col: u16) -> Option<Vec<u32>> {
        let mut len: u8 = 0;
        let ptr = unsafe {
            ghostty_vt_terminal_render_cell_grapheme(self.terminal.handle, row, col, &mut len)
        };
        if ptr.is_null() || len == 0 {
            return None;
        }
        let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        Some(slice.to_vec())
    }
}

impl<'a> Drop for RenderFrame<'a> {
    fn drop(&mut self) {
        unsafe {
            ghostty_vt_terminal_render_clear_dirty(self.terminal.handle);
        }
        self.terminal.frame_active.set(false);
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
