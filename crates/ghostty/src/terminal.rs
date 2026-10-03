use utils::asserts::unreachable;

use crate::zig::RowView;
use crate::*;
use core::ffi::c_void;

/// Events produced by the terminal during `feed()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    Bell,
    TitleChanged(String),
}

struct CallbackSink {
    event_tx: async_channel::Sender<TerminalEvent>,
    wake: Box<dyn Fn() + Send + Sync>,
}

pub struct CallbackHandle {
    terminal: NonNull<c_void>,
    _sink: Box<CallbackSink>,
}

unsafe extern "C" fn bell_trampoline(userdata: *mut c_void) {
    let _ = std::panic::catch_unwind(|| {
        let sink = unsafe { &*(userdata as *const CallbackSink) };
        let _ = sink.event_tx.try_send(TerminalEvent::Bell);
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
        let _ = sink.event_tx.try_send(TerminalEvent::TitleChanged(title));
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
    pub fn new(dimensions: TerminalDimensions, fg: ColorRGB, bg: ColorRGB) -> Option<Self> {
        let handle = unsafe { ghostty_terminal_new(dimensions, fg.to_u32(), bg.to_u32()) };
        let h = NonNull::new(handle)?;
        Some(Terminal { handle: h })
    }

    /// Borrow the underlying Ghostty handle for integration layers such as zconpty.
    #[inline]
    pub const fn handle(&self) -> *mut c_void {
        self.handle.as_ptr()
    }

    /// Acquire the Zig-owned terminal mutex.
    ///
    /// # Safety
    /// The caller must pair this with [`Terminal::unlock`] on the same
    /// terminal and must not call methods that lock internally while the mutex is held.
    #[inline]
    pub unsafe fn lock(&self) {
        unsafe { ghostty_terminal_lock(self.handle) };
    }

    /// Release the Zig-owned terminal mutex.
    ///
    /// # Safety
    /// The caller must currently hold the terminal mutex for this terminal.
    #[inline]
    pub unsafe fn unlock(&self) {
        unsafe { ghostty_terminal_unlock(self.handle) };
    }

    /// Register bell/title/output callbacks.
    /// Locks internally.
    pub fn set_event_sender(
        &self,
        event_tx: async_channel::Sender<TerminalEvent>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> CallbackHandle {
        let mut sink = Box::new(CallbackSink {
            event_tx,
            wake: Box::new(wake),
        });
        let userdata = (&raw mut *sink).cast::<c_void>();

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

    /// Feed bytes from the pseudoterminal into Ghostty's VT parser.
    /// Locks internally and wakes the registered output callback once for
    /// every non-empty feed.
    #[inline]
    pub fn feed(&self, bytes: &[u8]) {
        unsafe { ghostty_terminal_feed(self.handle, bytes.as_ptr(), bytes.len()) };
    }

    /// Resize the terminal grid.
    /// Locks internally.
    #[inline]
    pub fn resize(&self, dimensions: TerminalDimensions) {
        unsafe { ghostty_terminal_resize(self.handle, dimensions) };
    }

    /// Update the persistent render state and return a detached frame accessor.
    ///
    /// This does not lock internally. Callers must hold the terminal mutex so
    /// they can gather any other frame-coherent terminal data in the same critical
    /// section before unlocking.
    ///
    /// # Safety
    /// The caller must hold the terminal mutex via [`Terminal::lock`].
    /// Callers must still avoid overlapping frames on the same terminal
    /// because `RenderFrame::drop()` clears shared dirty flags.
    ///
    /// The caller must also ensure the terminal outlives the returned frame.
    #[inline]
    pub unsafe fn render_frame(&self) -> RenderFrame {
        // Ignore return: a failed update (allocation error inside Ghostty)
        // leaves RenderState in its previous valid state. We hand out a
        // frame over stale-but-consistent data rather than crashing or
        // skipping the frame. The dirty flags are unchanged, so the next
        // successful update will re-render the affected rows.
        unsafe { ghostty_terminal_render_update(self.handle) };
        RenderFrame {
            handle: self.handle,
        }
    }

    // --- Mode flag queries (read-only, &self) ---

    /// Whether synchronized output mode (DEC 2026) is active.
    /// Locks internally.
    #[inline]
    pub fn is_synchronized_output(&self) -> bool {
        unsafe { ghostty_terminal_is_synchronized_output(self.handle) }
    }

    /// Reset synchronized output mode (DEC 2026).
    /// Used by the sync-output safety timer to unfreeze misbehaving programs.
    /// Locks internally.
    #[inline]
    pub fn reset_synchronized_output(&self) {
        unsafe { ghostty_terminal_reset_synchronized_output(self.handle) }
    }

    /// Whether focus event mode (DEC 1004) is active.
    /// Locks internally.
    #[inline]
    pub fn is_focus_event_mode(&self) -> bool {
        unsafe { ghostty_terminal_is_focus_event_mode(self.handle) }
    }

    // TODO: Doc comment
    /// Locks internally.
    #[inline]
    pub fn mouse_mode(&self) -> MouseMode {
        unsafe { ghostty_terminal_get_mouse_mode(self.handle) }
    }

    // --- Viewport scroll (mutating terminal state, `&self`) ---

    /// Scroll the viewport by delta rows.
    /// Negative = up (towards history), positive = down.
    /// Locks internally.
    #[inline]
    pub fn scroll_viewport(&self, delta: i32) {
        unsafe { ghostty_terminal_scroll_viewport(self.handle, delta) }
    }

    /// Scroll the viewport to the top of scrollback.
    /// Locks internally.
    #[inline]
    pub fn scroll_to_top(&self) {
        unsafe { ghostty_terminal_scroll_viewport_top(self.handle) }
    }

    /// Scroll the viewport to the bottom (active area).
    /// Locks internally.
    #[inline]
    pub fn scroll_to_bottom(&self) {
        unsafe { ghostty_terminal_scroll_viewport_bottom(self.handle) }
    }

    /// Scroll viewport to an absolute row offset from the top of scrollback.
    /// Locks internally.
    #[inline]
    pub fn scroll_to_row(&self, row: u64) {
        unsafe { ghostty_terminal_scroll_to_row(self.handle, row) }
    }

    /// Read-only - Whether the viewport is at the bottom (active area).
    /// Locks internally.
    #[inline]
    pub fn viewport_is_bottom(&self) -> bool {
        unsafe { ghostty_terminal_viewport_is_bottom(self.handle) }
    }

    /// Query scrollbar positioning info (total rows, viewport offset, viewport size).
    /// This does not lock internally.
    ///
    /// # Safety
    /// The caller must hold the terminal mutex via [`Terminal::lock`].
    #[inline]
    pub unsafe fn scrollbar_info(&self) -> ScrollbarInfo {
        unsafe { ghostty_terminal_scrollbar_info(self.handle) }
    }

    // --- Selection (send/reset mutate, text read is &self) ---

    // TODO: Doc comments

    #[inline]
    pub fn send_gesture_press(
        &self,
        x: f32,
        y: f32,
        ctrl_or_super: bool,
        shift: bool,
        rectangular: bool,
    ) -> SelectionUpdate {
        unsafe {
            ghostty_terminal_gesture_press(self.handle, x, y, ctrl_or_super, shift, rectangular)
        }
    }

    #[inline]
    pub fn send_gesture_release(&self, x: f32, y: f32) {
        unsafe { ghostty_terminal_gesture_release(self.handle, x, y) };
    }

    #[inline]
    pub fn send_gesture_drag(&self, x: f32, y: f32, rectangular: bool) -> SelectionUpdate {
        unsafe { ghostty_terminal_gesture_drag(self.handle, x, y, rectangular) }
    }

    #[inline]
    pub fn send_gesture_autoscroll_tick(
        &self,
        x: f32,
        y: f32,
        rectangular: bool,
    ) -> SelectionUpdate {
        unsafe { ghostty_terminal_gesture_autoscroll_tick(self.handle, x, y, rectangular) }
    }

    #[inline]
    pub fn reset_gesture(&self) {
        unsafe { ghostty_terminal_gesture_reset(self.handle) };
    }

    /// Clear any active selection.
    /// Locks internally.
    #[inline]
    pub fn clear_selection(&self) -> bool {
        unsafe { ghostty_terminal_clear_selection(self.handle) }
    }

    // TODO: Doc comment
    /// Locks internally.
    #[inline]
    pub fn take_selection_text(&self) -> Option<String> {
        let mut len: usize = 0;
        let ptr = unsafe { ghostty_terminal_take_selection_text(self.handle, &mut len) };
        if ptr.is_null() {
            return None;
        }

        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        // Safety: Ghostty produces valid UTF-8 selection text (stores
        // codepoints internally and serializes via std.unicode.utf8Encode).
        let text = unsafe { std::str::from_utf8_unchecked(bytes) }.to_owned();

        unsafe { ghostty_terminal_bytes_free(self.handle, ptr, len) };

        Some(text)
    }
}

impl Drop for Terminal {
    #[inline]
    fn drop(&mut self) {
        unsafe { ghostty_terminal_free(self.handle) };
    }
}

/// Detached render state accessor — holds a raw Zig handle pointer.
///
/// Created by `Terminal::render_frame()` while the caller holds the terminal
/// mutex. The handle pointer is Zig-allocated and heap-stable.
///
/// `render_rows()` exposes a borrowed `RowView`; its cell lists provide
/// `CellView` access to zero-copy cell and style columns in `RenderState`.
/// These pointers are stable from the moment `render_frame()` returns
/// until the next `render_update()`. Thus, it is stable for the entire
/// frame (when frame drops, dirty flags clear).
///
/// The intended pattern is: lock → `render_frame()` + any other coherent
/// queries → unlock. This detached design keeps text run building and
/// present work outside the terminal critical section.
// NOTE: This will be reworked soon to tie the frame to Terminal, encode the
// lifetime/update protocol in the type system, and return Result on update failure.
pub struct RenderFrame {
    handle: NonNull<c_void>,
}

impl RenderFrame {
    /// Returns a view into the `std.MultiArrayList(terminal.RenderState.Row)`
    /// in Ghostty's `RenderState`, representing all rows in the viewport.
    /// The view is valid until the next `render_update()` call.
    #[inline]
    pub fn render_rows(&self) -> RowView<'_> {
        let rows = unsafe { ghostty_terminal_render_row_data(self.handle) };
        // SAFETY: The header belongs to this frame's RenderState and the frame
        // protocol prohibits another render update while the view is alive.
        RowView::from(unsafe { &*rows })
    }

    /// Current dirty state of the render data.
    #[inline]
    pub fn dirty(&self) -> Dirty {
        match unsafe { ghostty_terminal_render_dirty(self.handle) } {
            0 => Dirty::Clean,
            1 => Dirty::Partial,
            2 => Dirty::Full,
            _ => unreachable(),
        }
    }

    /// Number of rows and columns in the current render state.
    #[inline]
    pub fn dimensions(&self) -> (u16, u16) {
        let (mut rows, mut cols) = (0, 0);
        unsafe { ghostty_terminal_get_dimensions(self.handle, &mut rows, &mut cols) };
        (rows, cols)
    }

    /// Current cursor state from `RenderState`.
    /// The view is valid until the next `render_update()` call.
    #[inline]
    pub fn render_cursor(&self) -> &RenderCursor {
        let ptr = unsafe { ghostty_terminal_render_cursor(self.handle) };
        // Safety: Pointer is into `RenderState` memory, stable until next render_update().
        unsafe { &*ptr }
    }

    /// Current terminal colors (foreground, background, cursor).
    #[inline]
    pub fn colors(&self) -> &RenderColors {
        let ptr = unsafe { ghostty_terminal_render_colors(self.handle) };
        // Safety: pointer is into `RenderState` memory, stable until next render_update().
        unsafe { &*ptr }
    }
}

impl Drop for RenderFrame {
    #[inline]
    fn drop(&mut self) {
        unsafe { ghostty_terminal_render_clear_dirty(self.handle) };
    }
}

impl Drop for CallbackHandle {
    fn drop(&mut self) {
        unsafe {
            ghostty_terminal_set_callbacks(self.terminal, std::ptr::null_mut(), None, None, None);
        }
    }
}
