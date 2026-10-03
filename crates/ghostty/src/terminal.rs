use utils::asserts::unreachable;

use crate::zig::RowView;
use crate::*;
use core::ffi::c_void;
use std::{marker::PhantomData, rc::Rc};

/// Events produced by the terminal during `feed()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    Bell,
    TitleChanged(String),
    ClipboardWrite(ClipboardWrite),
}

/// A terminal application's request to update the host clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardWrite {
    /// Clear all clipboard formats, including non-text data.
    Clear,
    /// Replace all clipboard contents with UTF-8 plain text.
    /// An empty string publishes empty text rather than clearing the clipboard.
    Text(String),
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

unsafe extern "C" fn clipboard_write_trampoline(
    userdata: *mut c_void,
    ptr: *const u8,
    len: usize,
) -> bool {
    std::panic::catch_unwind(|| {
        let sink = unsafe { &*(userdata as *const CallbackSink) };
        if sink.event_tx.is_closed() || sink.event_tx.is_full() {
            return false;
        }
        let write = match ptr.is_null() {
            true => ClipboardWrite::Clear,
            false => {
                let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
                let Ok(text) = str::from_utf8(bytes) else {
                    return false;
                };
                ClipboardWrite::Text(text.to_owned())
            }
        };
        sink.event_tx.try_send(TerminalEvent::ClipboardWrite(write)).is_ok()
    })
    .unwrap_or(false)
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

    /// Run an operation while holding the terminal mutex.
    ///
    /// The [`LockedTerminal`] passed to `f` exposes operations that require a
    /// coherent view of terminal state. It cannot escape the closure, and the
    /// mutex is released when the closure returns or unwinds.
    ///
    /// Caller must not call methods that lock internally inside the closure.
    #[inline]
    pub fn with_lock<R>(&self, f: impl for<'lock> FnOnce(&mut LockedTerminal<'lock>) -> R) -> R {
        unsafe { ghostty_terminal_lock(self.handle) };
        let mut terminal = LockedTerminal { terminal: self, _not_send_or_sync: PhantomData };
        f(&mut terminal)
    }

    /// Register terminal event and output callbacks.
    /// Locks internally.
    pub fn set_event_sender(
        &self,
        event_tx: async_channel::Sender<TerminalEvent>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> CallbackHandle {
        let mut sink = Box::new(CallbackSink { event_tx, wake: Box::new(wake) });
        let userdata = (&raw mut *sink).cast::<c_void>();

        unsafe {
            ghostty_terminal_set_callbacks(
                self.handle,
                userdata,
                Some(bell_trampoline),
                Some(title_trampoline),
                Some(clipboard_write_trampoline),
                Some(output_trampoline),
            );
        }

        CallbackHandle { terminal: self.handle, _sink: sink }
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

/// Scoped access to terminal operations that require its mutex to be held.
///
/// Created only by [`Terminal::with_lock`]. The guard is neither sendable nor
/// shareable, so the terminal is always unlocked on the thread that locked it.
pub struct LockedTerminal<'a> {
    terminal: &'a Terminal,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl LockedTerminal<'_> {
    /// Copy terminal-dependent data into `state` and begin a frame update.
    ///
    /// The returned update no longer accesses the terminal, so it may be
    /// completed after [`Terminal::with_lock`] releases the mutex.
    #[inline]
    pub fn begin_update<'a>(
        &mut self,
        state: &'a mut RenderState,
    ) -> Result<PendingUpdate<'a>, RenderUpdateError> {
        match unsafe { ghostty_render_state_begin_update(state.handle, self.terminal.handle) } {
            0 => Ok(PendingUpdate { state: Some(state) }),
            1 => Err(RenderUpdateError::OutOfMemory),
            _ => unreachable(),
        }
    }

    /// Query scrollbar positioning coherently with the frame update.
    #[inline]
    pub fn scrollbar_info(&mut self) -> ScrollbarInfo {
        unsafe { ghostty_terminal_scrollbar_info(self.terminal.handle) }
    }
}

impl Drop for LockedTerminal<'_> {
    fn drop(&mut self) {
        unsafe { ghostty_terminal_unlock(self.terminal.handle) };
    }
}

/// Reusable storage for a renderer's snapshot of terminal state.
///
/// A render state is allocated independently from [`Terminal`] and retains its
/// buffers between updates. [`LockedTerminal::begin_update`] refreshes it from
/// a terminal, and [`PendingUpdate::finish`] makes the completed snapshot
/// available as a [`RenderFrame`]. Keeping the storage separate lets frame
/// processing continue after the terminal mutex has been released.
///
/// The intended pattern is: lock → `begin_update()` + any other coherent
/// queries → unlock. This detached design keeps text run building and
/// present work outside the terminal critical section.
pub struct RenderState {
    handle: NonNull<c_void>,
}

impl RenderState {
    /// Allocate an empty render state for reuse across frames.
    pub fn new() -> Option<Self> {
        NonNull::new(unsafe { ghostty_render_state_new() }).map(|handle| Self { handle })
    }
}

impl Drop for RenderState {
    fn drop(&mut self) {
        unsafe { ghostty_render_state_free(self.handle) };
    }
}

/// An update whose terminal-dependent phase has completed.
///
/// The render state is not readable until [`PendingUpdate::finish`] completes
/// Ghostty's deferred work. Dropping this value also completes that work, so a
/// successful begin is always paired with an end update during unwinding.
pub struct PendingUpdate<'a> {
    state: Option<&'a mut RenderState>,
}

impl<'a> PendingUpdate<'a> {
    /// Complete deferred work and return a readable frame.
    #[inline]
    pub fn finish(mut self) -> RenderFrame<'a> {
        let state = self.state.take().unwrap();
        unsafe { ghostty_render_state_end_update(state.handle) };
        RenderFrame { state }
    }
}

impl Drop for PendingUpdate<'_> {
    #[inline]
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            unsafe { ghostty_render_state_end_update(state.handle) };
        }
    }
}

/// A read-only view of one completed render-state update.
///
/// Exposes a borrowed `RowView` and `CellView` for zero-copy access into the
/// columns in `RenderState`. The frame's mutable borrow of [`RenderState`]
/// keeps its zero-copy views stable and prevents overlapping updates.
///
/// Dropping a frame leaves its dirty flags intact; call [`RenderFrame::mark_clean`]
/// only after successfully consuming them.
pub struct RenderFrame<'a> {
    state: &'a mut RenderState,
}

impl RenderFrame<'_> {
    /// Returns a view into the `std.MultiArrayList(terminal.RenderState.Row)`
    /// in Ghostty's `RenderState`, representing all rows in the viewport.
    /// The view is valid for this frame's borrow (until the next `begin_update` call).
    #[inline]
    pub fn render_rows(&self) -> RowView<'_> {
        let rows = unsafe { ghostty_render_state_row_data(self.state.handle) };
        // SAFETY: The header belongs to this frame's RenderState and the frame
        // protocol prohibits another render update while the view is alive.
        RowView::from(unsafe { &*rows })
    }

    /// Current dirty state of the render data.
    #[inline]
    pub fn dirty(&self) -> Dirty {
        match unsafe { ghostty_render_state_dirty(self.state.handle) } {
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
        unsafe { ghostty_render_state_get_dimensions(self.state.handle, &mut rows, &mut cols) };
        (rows, cols)
    }

    /// Current cursor state from `RenderState`.
    /// The view is valid for this frame's borrow.
    #[inline]
    pub fn render_cursor(&self) -> &RenderCursor {
        let ptr = unsafe { ghostty_render_state_cursor(self.state.handle) };
        // Safety: The frame's mutable borrow prevents another render state update.
        unsafe { &*ptr }
    }

    /// Current terminal colors (foreground, background, cursor).
    #[inline]
    pub fn colors(&self) -> &RenderColors {
        let ptr = unsafe { ghostty_render_state_colors(self.state.handle) };
        // Safety: The frame's mutable borrow prevents another render state update.
        unsafe { &*ptr }
    }

    /// Mark this frame's global and per-row dirty state as consumed.
    #[inline]
    pub fn mark_clean(self) {
        unsafe { ghostty_render_state_clear_dirty(self.state.handle) };
    }
}

/// An error encountered while copying terminal data into a render state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderUpdateError {
    /// Ghostty could not grow one of the render-state buffers.
    OutOfMemory,
}

impl std::fmt::Display for RenderUpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ghostty could not allocate render state")
    }
}

impl std::error::Error for RenderUpdateError {}

impl Drop for CallbackHandle {
    fn drop(&mut self) {
        unsafe {
            ghostty_terminal_set_callbacks(
                self.terminal,
                std::ptr::null_mut(),
                None,
                None,
                None,
                None,
            );
        }
    }
}
