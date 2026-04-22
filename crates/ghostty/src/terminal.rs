use std::{
    fmt::{Debug, Formatter, Result},
    ops::Deref,
};

use core::ffi::c_void;

use crate::*;

/// Events produced by the terminal during `feed()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Bell,
    TitleChanged(String),
}

struct CallbackSink {
    event_tx: async_channel::Sender<Event>,
    wake: Box<dyn Fn() + Send + Sync>,
}

pub struct CallbackHandle {
    terminal: NonNull<c_void>,
    _sink: Box<CallbackSink>,
}

unsafe extern "C" fn bell_trampoline(userdata: *mut c_void) {
    let _ = std::panic::catch_unwind(|| {
        let sink = unsafe { &*(userdata as *const CallbackSink) };
        let _ = sink.event_tx.try_send(Event::Bell);
    });
}

unsafe extern "C" fn title_trampoline(userdata: *mut c_void, ptr: *const u8, len: usize) {
    let _ = std::panic::catch_unwind(|| {
        let sink = unsafe { &*(userdata as *const CallbackSink) };
        // Guard: from_raw_parts requires non-null ptr even when len == 0.
        // Zig slices always have non-null .ptr, but defend against edge cases.
        let title = if ptr.is_null() || len == 0 {
            String::new()
        } else {
            let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
            String::from_utf8_lossy(bytes).into_owned()
        };
        let _ = sink.event_tx.try_send(Event::TitleChanged(title));
    });
}

unsafe extern "C" fn output_trampoline(userdata: *mut c_void) {
    let _ = std::panic::catch_unwind(|| {
        let sink = unsafe { &*(userdata as *const CallbackSink) };
        (sink.wake)();
    });
}

/// Safe wrapper around the Ghostty VT terminal handle.
///
/// Thread-safe handle wrapper around the shared Zig terminal.
/// Zig owns the terminal mutex, the feed callback path, and device-response routing.
pub struct Terminal {
    handle: NonNull<c_void>,
}

// Safety: Zig owns the terminal mutex and locks internally on
// mutating operations, so the raw handle can be shared.
unsafe impl Send for Terminal {}
unsafe impl Sync for Terminal {}

impl Terminal {
    /// Create a new terminal with the given dimensions and default colors.
    /// Returns `None` if allocation fails.
    pub fn new(cols: u16, rows: u16, fg: ColorRGB, bg: ColorRGB) -> Option<Self> {
        let handle = unsafe {
            ghostty_terminal_new(cols, rows, fg.r(), fg.g(), fg.b(), bg.r(), bg.g(), bg.b())
        };
        if handle.is_null() {
            return None;
        }

        let h = unsafe { NonNull::new_unchecked(handle) };
        Some(Terminal { handle: h })
    }

    /// Borrow the underlying Ghostty handle for integration layers such as zconpty.
    pub fn handle(&self) -> *mut c_void {
        self.handle.as_ptr()
    }

    /// Acquire the Zig-owned terminal mutex.
    ///
    /// SAFETY: The caller must pair this with [`Terminal::unlock`] on the same
    /// terminal and must not call methods that lock internally while the mutex is held.
    pub unsafe fn lock(&self) {
        unsafe {
            ghostty_terminal_lock(self.handle);
        }
    }

    /// Release the Zig-owned terminal mutex.
    ///
    /// SAFETY: The caller must currently hold the terminal mutex for this terminal.
    pub unsafe fn unlock(&self) {
        unsafe {
            ghostty_terminal_unlock(self.handle);
        }
    }

    /// Register bell/title/output callbacks.
    /// Locks internally.
    pub fn set_event_sender(
        &self,
        event_tx: async_channel::Sender<Event>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> CallbackHandle {
        let mut sink = Box::new(CallbackSink {
            event_tx,
            wake: Box::new(wake),
        });
        let userdata = (&mut *sink as *mut CallbackSink).cast::<c_void>();

        unsafe {
            ghostty_terminal_set_callbacks(
                self.handle,
                userdata,
                Some(bell_trampoline),
                Some(title_trampoline),
                Some(output_trampoline),
            );
        }

        CallbackHandle {
            terminal: self.handle,
            _sink: sink,
        }
    }

    /// Resize the terminal grid.
    /// Locks internally.
    pub fn resize(&self, cols: u16, rows: u16) {
        unsafe {
            ghostty_terminal_resize(self.handle, cols, rows);
        }
    }

    /// Set dimensions for rendering. Called by the renderer whenever
    /// font metrics change. Needed for size report responses (CSI 14t, 16t),
    /// Kitty Graphics Protocol, and mouse event encoding.
    /// Locks internally.
    pub fn set_dimensions(&self, dimensions: TerminalDimensions) {
        let screen_width_px = dimensions.screen_width_px.max(1);
        let screen_height_px = dimensions.screen_height_px.max(1);
        let cell_width_px = dimensions.cell_width_px.max(1);
        let cell_height_px = dimensions.cell_height_px.max(1);

        unsafe {
            ghostty_terminal_set_render_dimensions(
                self.handle,
                screen_width_px,
                screen_height_px,
                cell_width_px,
                cell_height_px,
            );
        }
    }

    /// Update the persistent render state and return a detached frame accessor.
    ///
    /// This does not lock internally. Callers must hold the terminal mutex so
    /// they can gather any other frame-coherent terminal data in the same critical
    /// section before unlocking.
    ///
    /// SAFETY: The caller must hold the terminal mutex via [`Terminal::lock`].
    /// Callers must still avoid overlapping frames on the same terminal
    /// because `RenderFrame::drop()` clears shared dirty flags.
    pub unsafe fn render_frame(&self) -> RenderFrame {
        // Ignore return: a failed update (allocation error inside Ghostty)
        // leaves RenderState in its previous valid state. We hand out a
        // frame over stale-but-consistent data rather than crashing or
        // skipping the frame. The dirty flags are unchanged, so the next
        // successful update will re-render the affected rows.
        unsafe {
            ghostty_terminal_render_update(self.handle);
        }
        RenderFrame {
            handle: self.handle,
        }
    }

    // --- Mode flag queries (read-only, &self) ---

    /// Whether synchronized output mode (DEC 2026) is active.
    /// Locks internally.
    pub fn is_synchronized_output(&self) -> bool {
        unsafe { ghostty_terminal_is_synchronized_output(self.handle) }
    }

    /// Reset synchronized output mode (DEC 2026).
    /// Used by the sync-output safety timer to unfreeze misbehaving programs.
    /// Locks internally.
    pub fn reset_synchronized_output(&self) {
        unsafe { ghostty_terminal_reset_synchronized_output(self.handle) }
    }

    /// Whether focus event mode (DEC 1004) is active.
    /// Locks internally.
    pub fn is_focus_event_mode(&self) -> bool {
        unsafe { ghostty_terminal_is_focus_event_mode(self.handle) }
    }

    /// Whether the alternate screen is active.
    /// Locks internally.
    pub fn is_alternate_screen(&self) -> bool {
        unsafe { ghostty_terminal_is_alternate_screen(self.handle) }
    }

    /// Whether mouse reporting is enabled
    /// Locks internally.
    pub fn is_mouse_reporting(&self) -> bool {
        unsafe { ghostty_terminal_is_mouse_reporting(self.handle) }
    }

    /// Whether alternate scroll mode (DEC 1007) is active.
    /// Locks internally.
    pub fn mouse_alternate_scroll_enabled(&self) -> bool {
        unsafe { ghostty_terminal_is_mouse_alternate_scroll(self.handle) }
    }

    // --- Viewport scroll (mutating terminal state, `&self`) ---

    /// Scroll the viewport by delta rows.
    /// Negative = up (towards history), positive = down.
    /// Locks internally.
    pub fn scroll_viewport(&self, delta: i32) {
        unsafe { ghostty_terminal_scroll_viewport(self.handle, delta) }
    }

    /// Scroll the viewport to the top of scrollback.
    /// Locks internally.
    pub fn scroll_to_top(&self) {
        unsafe { ghostty_terminal_scroll_viewport_top(self.handle) }
    }

    /// Scroll the viewport to the bottom (active area).
    /// Locks internally.
    pub fn scroll_to_bottom(&self) {
        unsafe { ghostty_terminal_scroll_viewport_bottom(self.handle) }
    }

    /// Scroll viewport to an absolute row offset from the top of scrollback.
    /// Locks internally.
    pub fn scroll_to_row(&self, row: u64) {
        unsafe { ghostty_terminal_scroll_to_row(self.handle, row) }
    }

    /// Read-only - Whether the viewport is at the bottom (active area).
    /// Locks internally.
    pub fn viewport_is_bottom(&self) -> bool {
        unsafe { ghostty_terminal_viewport_is_bottom(self.handle) }
    }

    /// Query scrollbar positioning info (total rows, viewport offset, viewport size).
    /// This does not lock internally.
    ///
    /// SAFETY: The caller must hold the terminal mutex via [`Terminal::lock`].
    pub unsafe fn scrollbar_info(&self) -> ScrollbarInfo {
        let mut out = ScrollbarInfo::default();
        let out_ptr = unsafe { NonNull::new_unchecked(&mut out) };
        unsafe { ghostty_terminal_scrollbar_info(self.handle, out_ptr) };
        out
    }

    // --- Selection (set/clear mutate, text read is &self) ---

    /// Set a selection in viewport coordinates (0-indexed).
    /// Returns `true` on success.
    /// Locks internally.
    pub fn set_selection(
        &self,
        start_x: u16,
        start_y: u32,
        end_x: u16,
        end_y: u32,
        rectangular: bool,
    ) -> bool {
        let rc = unsafe {
            ghostty_terminal_set_selection(
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

    /// Select the word at viewport coordinates (0-indexed) using Ghostty's
    /// terminal-side word selection semantics.
    /// Returns `true` if a selection was created.
    /// Locks internally.
    pub fn select_word_at(&self, x: u16, y: u32) -> bool {
        let rc = unsafe { ghostty_terminal_select_word_at(self.handle, x, y) };
        rc == 0
    }

    /// Select the (soft-wrapped) line at viewport coordinates (0-indexed)
    /// using Ghostty's terminal-side line selection semantics.
    /// Returns `true` if a selection was created.
    /// Locks internally.
    pub fn select_line_at(&self, x: u16, y: u32) -> bool {
        let rc = unsafe { ghostty_terminal_select_line_at(self.handle, x, y) };
        rc == 0
    }

    /// Select shell output at viewport coordinates (0-indexed) using
    /// Ghostty's semantic prompt integration.
    /// Returns `true` if a selection was created.
    /// Locks internally.
    pub fn select_output_at(&self, x: u16, y: u32) -> bool {
        let rc = unsafe { ghostty_terminal_select_output_at(self.handle, x, y) };
        rc == 0
    }

    /// Update selection during a double-click drag using Ghostty terminal
    /// semantics (`selectWordBetween`).
    /// Returns `true` if a selection was produced.
    /// Locks internally.
    pub fn select_word_drag(&self, click_x: u16, click_y: u32, drag_x: u16, drag_y: u32) -> bool {
        let rc = unsafe {
            ghostty_terminal_select_word_drag(self.handle, click_x, click_y, drag_x, drag_y)
        };
        rc == 0
    }

    /// Update selection during a triple-click drag using Ghostty terminal
    /// semantics (line-wise expansion).
    /// Returns `true` if a selection was produced.
    /// Locks internally.
    pub fn select_line_drag(&self, click_x: u16, click_y: u32, drag_x: u16, drag_y: u32) -> bool {
        let rc = unsafe {
            ghostty_terminal_select_line_drag(self.handle, click_x, click_y, drag_x, drag_y)
        };
        rc == 0
    }

    /// Clear any active selection.
    /// Locks internally.
    pub fn clear_selection(&self) {
        unsafe { ghostty_terminal_clear_selection(self.handle) }
    }

    /// Get the currently selected text. Returns `None` if no selection.
    /// The returned `SelectionText` frees its memory on drop.
    /// Locks internally.
    pub fn selection_text(&self) -> Option<SelectionText> {
        let mut len: usize = 0;
        let len_ptr = unsafe { NonNull::new_unchecked(&mut len) };
        let ptr = unsafe { ghostty_terminal_get_selection_text(self.handle, len_ptr) };
        if ptr.is_null() {
            return None;
        }
        let p = unsafe { NonNull::new_unchecked(ptr.cast_mut()) };
        Some(SelectionText {
            terminal: self.handle,
            ptr: p,
            len,
        })
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe {
            ghostty_terminal_free(self.handle);
        }
    }
}

/// Detached render state accessor — holds a raw Zig handle pointer.
///
/// Created by `Terminal::render_frame()` while the caller holds the terminal
/// mutex. The handle pointer is Zig-allocated and heap-stable.
///
/// Cell and style data returned by `row_raw()` and `row_styles()` are
/// zero-copy slices into RenderState memory. These pointers are stable
/// from the moment `render_frame()` returns until the next `render_update()`.
/// Thus, it is stable for the entire frame (when frame drops, dirty flags clear).
///
/// The intended pattern is: lock → `render_frame()` + any other coherent
/// queries → unlock. This detached design keeps text run building and
/// present work outside the terminal critical section.
pub struct RenderFrame {
    handle: NonNull<c_void>,
}

impl RenderFrame {
    /// Get raw cell data for a row (zero-copy pointer into Zig memory).
    /// Returns `None` if row is out of bounds.
    ///
    /// Safety: The returned slice borrows from RenderState memory
    /// that is stable for the lifetime of this RenderFrame.
    pub fn row_raw(&self, y: u16) -> Option<&[RawCell]> {
        let mut len: u16 = 0;
        let len_ptr = unsafe { NonNull::new_unchecked(&mut len) };
        let ptr = unsafe { ghostty_terminal_render_row_raw(self.handle, y, len_ptr) };
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
        let len_ptr = unsafe { NonNull::new_unchecked(&mut len) };
        let ptr = unsafe { ghostty_terminal_render_row_styles(self.handle, y, len_ptr) };
        if ptr.is_null() || len == 0 {
            return None;
        }
        // Safety: The Zig FFI returns *const CellStyle (the Zig extern struct).
        // Rust's CellStyle is #[repr(C)] with the same layout, verified by
        // Zig comptime assertions. Pointer is into RenderState memory,
        // stable for the frame's lifetime.
        Some(unsafe { std::slice::from_raw_parts(ptr, len as usize) })
    }

    /// Get grapheme codepoints for all cells of a row.
    /// Returns a zero-copy slice into the grapheme SoA column for a row (slice of slices).
    ///
    /// Each element is a Zig slice []const u21 = { ptr: [*]const u21, len: usize }.
    /// GraphemeSlice mirrors the layout of a Zig slice, verified by comptime assertions.
    ///
    /// Only the low 21 bits of each `u32` are meaningful. Zig stores these values as
    /// `u21`; the FFI exposes them as `u32` with Zig comptime assertions guaranteeing
    /// size/alignment compatibility.
    ///
    /// For cells without graphemes, slices are undefined - caller must check the raw
    /// cell's content_tag before accessing a slice.
    ///
    /// Returns `None` for out-of-bounds rows (or if the row has zero cells).
    ///
    /// SAFETY: The returned slice points into RenderState memory and
    /// is stable for the frame lifetime (until the next `render_update()`).
    pub fn row_graphemes(&self, row: u16) -> Option<&[GraphemeSlice]> {
        let mut len: u16 = 0;
        let len_ptr = unsafe { NonNull::new_unchecked(&mut len) };
        let ptr = unsafe { ghostty_terminal_render_row_graphemes(self.handle, row, len_ptr) };
        if ptr.is_null() || len == 0 {
            return None;
        }
        // SAFETY: The returned slice points into RenderState memory and
        // is stable for the frame lifetime (until the next `render_update()`).
        Some(unsafe { std::slice::from_raw_parts(ptr, len as usize) })
    }

    /// Current dirty state of the render data.
    #[inline(always)]
    pub fn dirty(&self) -> DirtyState {
        let raw = unsafe { ghostty_terminal_render_dirty(self.handle) };
        DirtyState::from_raw(raw)
    }

    /// Number of rows in the current render state.
    #[inline(always)]
    pub fn rows(&self) -> u16 {
        unsafe { ghostty_terminal_render_rows(self.handle) }
    }

    /// Number of columns in the current render state.
    #[inline(always)]
    pub fn cols(&self) -> u16 {
        unsafe { ghostty_terminal_render_cols(self.handle) }
    }

    /// Whether a specific row has changed since last clear.
    #[inline(always)]
    pub fn row_dirty(&self, y: u16) -> bool {
        unsafe { ghostty_terminal_render_row_dirty(self.handle, y) }
    }

    /// Get selection range for a row. Returns `Some((start_x, end_x))` if
    /// the row has an active selection, `None` otherwise.
    pub fn row_selection(&self, y: u16) -> Option<(u16, u16)> {
        let mut start_x: u16 = 0;
        let mut end_x: u16 = 0;

        let start_ptr = unsafe { NonNull::new_unchecked(&mut start_x) };
        let end_ptr = unsafe { NonNull::new_unchecked(&mut end_x) };
        let has =
            unsafe { ghostty_terminal_render_row_selection(self.handle, y, start_ptr, end_ptr) };
        if has { Some((start_x, end_x)) } else { None }
    }

    /// Current cursor state.
    pub fn cursor(&self) -> CursorState {
        let mut out = CursorState::default();
        let out_ptr = unsafe { NonNull::new_unchecked(&mut out) };
        unsafe { ghostty_terminal_render_cursor(self.handle, out_ptr) };
        out
    }

    /// Current terminal colors (foreground, background, cursor).
    pub fn colors(&self) -> &RenderColors {
        let ptr = unsafe { ghostty_terminal_render_colors(self.handle) };
        // Safety: pointer is into RenderState memory, stable until next render_update().
        unsafe { &*ptr }
    }
}

impl Drop for RenderFrame {
    fn drop(&mut self) {
        unsafe {
            ghostty_terminal_render_clear_dirty(self.handle);
        }
    }
}

impl Drop for CallbackHandle {
    fn drop(&mut self) {
        unsafe {
            ghostty_terminal_set_callbacks(self.terminal, std::ptr::null_mut(), None, None, None);
        }
    }
}

/// Owned selection text. Frees the underlying Zig-allocated buffer on drop.
pub struct SelectionText {
    terminal: NonNull<c_void>,
    ptr: NonNull<u8>,
    len: usize,
}

impl SelectionText {
    /// View the selection text as a `&str`.
    pub fn as_str(&self) -> &str {
        // Safety: Ghostty produces valid UTF-8 selection text (stores
        // codepoints internally and serializes via std.unicode.utf8Encode).
        unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(self.ptr.as_ptr(), self.len))
        }
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
        unsafe { ghostty_terminal_bytes_free(self.terminal, self.ptr, self.len) }
    }
}
