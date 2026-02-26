# ffi-001: FFI Bindings + Safe Rust Wrapper

**Goal:** Build a safe `ghostty_vt::Terminal` Rust wrapper with `RenderFrame<'_>` borrow guard, internal event queue (replacing raw callback exposure), and rich Rust types — making the raw FFI internal-only.

**Architecture:** The safe wrapper owns the opaque Zig handle (`*mut c_void`) with single-thread ownership (`!Send + !Sync` via `PhantomData<Rc<()>>`). `Terminal` methods are split into mutating (`&mut self`: feed, resize, render_update, scroll, selection mutation) and non-mutating (`&self`: begin_frame, mode queries, key encoding). `RenderFrame<'_>` borrows `&Terminal`, preventing mutation while render data is accessed. C callbacks are registered internally at construction; they push `VtEvent`s to a `Vec<VtEvent>` that the consumer drains after `feed()`.

**Tech Stack:** Rust, C ABI FFI (`unsafe extern "C"`), Ghostty 1.3.x via Zig shim

**Refs:**

- `docs/architecture/02-data-model.md` § Render Data Access and Frame Lifetime Safety
- `docs/architecture/06-ghostty-shim.md` § FFI Lifetime and Thread Contract
- `docs/architecture/08-ghostty-alignment-decisions.md` § Decision 4: FFI Lifetime Contract
- `docs/architecture/04-pty-threading.md` § Reentrancy Prevention

---

## Conventions

- Build + test: `cargo test -p ghostty_vt`
- All raw FFI functions become `pub(crate)` — only the safe wrapper is public
- `Terminal` is `!Send + !Sync` (via `PhantomData<Rc<()>>`)
- Method split: `&mut self` for mutating operations, `&self` for reads
- Each part is independently implementable and testable
- Parts must be done in order (each builds on the previous)

---

## Part 1: Foundation — Types, `pub(crate)` FFI, Terminal struct, VtEvent, RenderFrame

**Goal:** Define rich Rust types, make raw FFI internal-only, create the `Terminal` struct with lifecycle + feed/resize + internal event queue + `RenderFrame` borrow guard with dirty/cursor/colors/cells access. Write tests for all of the above.

**Files:**

- Modify: `crates/ghostty-vt/src/types.rs`
- Modify: `crates/ghostty-vt/src/lib.rs`
- Create: `crates/ghostty-vt/src/terminal.rs`
- Create: `crates/ghostty-vt/src/tests/safe_terminal.rs`

### Step 1: Add rich Rust types to `types.rs`

Append after the existing `FlatCell` comptime assertions:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyState {
    Clean,
    Partial,
    Full,
}

impl DirtyState {
    pub(crate) fn from_raw(value: u8) -> Self {
        match value {
            0 => DirtyState::Clean,
            1 => DirtyState::Partial,
            _ => DirtyState::Full,
        }
    }

    pub fn is_dirty(self) -> bool {
        self != DirtyState::Clean
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    None,
    X10,
    Normal,
    Button,
    Any,
}

impl MouseMode {
    pub(crate) fn from_raw(value: u8) -> Self {
        match value {
            0 => MouseMode::None,
            1 => MouseMode::X10,
            2 => MouseMode::Normal,
            3 => MouseMode::Button,
            _ => MouseMode::Any,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseFormat {
    X10,
    Utf8,
    Sgr,
    Urxvt,
    SgrPixels,
}

impl MouseFormat {
    pub(crate) fn from_raw(value: u8) -> Self {
        match value {
            0 => MouseFormat::X10,
            1 => MouseFormat::Utf8,
            2 => MouseFormat::Sgr,
            3 => MouseFormat::Urxvt,
            _ => MouseFormat::SgrPixels,
        }
    }
}
```

Also change callback type visibility from `pub` to `pub(crate)`:

```rust
pub(crate) type BellCallback = unsafe extern "C" fn(userdata: *mut c_void);
pub(crate) type TitleCallback = unsafe extern "C" fn(userdata: *mut c_void, ptr: *const u8, len: usize);
```

### Step 2: Make raw FFI `pub(crate)` in `lib.rs`

Change every `pub fn` inside the `unsafe extern "C"` block to `pub(crate) fn`.

Replace the module declarations and re-exports at the top of the file:

```rust
mod types;
mod terminal;

use core::ffi::{c_int, c_void};

// Raw FFI — crate-internal only
pub(crate) use types::{BellCallback, TitleCallback};

// Public safe API
pub use terminal::{RenderFrame, SelectionText, Terminal, VtEvent};
pub use types::{ColorRGB, ColorState, CursorState, DirtyState, FlatCell, MouseFormat, MouseMode};

unsafe extern "C" {
    pub(crate) fn ghostty_vt_terminal_new(cols: u16, rows: u16) -> *mut c_void;
    pub(crate) fn ghostty_vt_terminal_free(terminal: *mut c_void);
    // ... all other declarations, each changed to pub(crate) ...
}
```

### Step 3: Create `terminal.rs`

```rust
// crates/ghostty-vt/src/terminal.rs

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;

use core::ffi::{c_int, c_void};

use crate::*;

// ---------------------------------------------------------------------------
// VtEvent
// ---------------------------------------------------------------------------

/// Events produced by the terminal during `feed()`.
/// Consumed via `Terminal::drain_events()` after each feed cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VtEvent {
    Bell,
    TitleChanged(String),
}

// ---------------------------------------------------------------------------
// Callback trampolines
// ---------------------------------------------------------------------------

// C callback trampolines — push to the Vec<VtEvent> via userdata pointer.
// Safety: these are only called during `feed()`, which holds `&mut self`
// on the single UI thread — no concurrent access to the queue.
// Wrapped in catch_unwind to prevent unwinding across the FFI boundary.

unsafe extern "C" fn bell_trampoline(userdata: *mut c_void) {
    let _ = std::panic::catch_unwind(|| {
        let events = &mut *(userdata as *mut Vec<VtEvent>);
        events.push(VtEvent::Bell);
    });
}

unsafe extern "C" fn title_trampoline(userdata: *mut c_void, ptr: *const u8, len: usize) {
    let _ = std::panic::catch_unwind(|| {
        let events = &mut *(userdata as *mut Vec<VtEvent>);
        // Guard: from_raw_parts requires non-null ptr even when len == 0.
        // Zig slices always have non-null .ptr, but defend against edge cases.
        let title = if len == 0 || ptr.is_null() {
            String::new()
        } else {
            let bytes = std::slice::from_raw_parts(ptr, len);
            String::from_utf8_lossy(bytes).into_owned()
        };
        events.push(VtEvent::TitleChanged(title));
    });
}

// ---------------------------------------------------------------------------
// Terminal
// ---------------------------------------------------------------------------

/// Safe wrapper around the Ghostty VT terminal handle.
///
/// Single-thread owned (`!Send + !Sync` via `PhantomData<Rc<()>>`).
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
    /// Makes Terminal !Send + !Sync. Raw pointers alone don't reliably
    /// prevent auto-trait inference in all compiler versions.
    _not_send_sync: PhantomData<Rc<()>>,
}

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
        let userdata = &mut **events as *mut Vec<VtEvent> as *mut c_void;
        unsafe {
            ghostty_vt_terminal_set_callbacks(
                handle,
                userdata,
                Some(bell_trampoline as BellCallback),
                Some(title_trampoline as TitleCallback),
            );
        }

        Some(Terminal {
            handle,
            events,
            frame_active: Cell::new(false),
            _not_send_sync: PhantomData,
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

// ---------------------------------------------------------------------------
// RenderFrame
// ---------------------------------------------------------------------------

/// Borrow guard for render state access.
///
/// Holds `&Terminal`, preventing mutation while render data is read.
/// When dropped, dirty flags are cleared automatically (calls
/// `render_clear_dirty` on the underlying handle) and the
/// `frame_active` guard is released.
///
/// Cell data returned by `row_cells()` is owned (`Vec<FlatCell>`) because
/// the Zig side uses a single shared flat_cells buffer that gets
/// overwritten on each FFI call. Owned copies are safe (~2KB per row
/// memcpy, negligible overhead).
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
        unsafe {
            ghostty_vt_terminal_render_palette_color(self.terminal.handle, index, &mut out)
        };
        out
    }

    /// Get flattened cell data for a row. Returns an owned `Vec<FlatCell>`.
    ///
    /// Returns owned data because the Zig side uses a single shared
    /// `flat_cells` buffer that gets overwritten on each call.
    /// Returning a borrowed slice would be unsound.
    /// The memcpy overhead is ~2KB per row (80 cols × 28 bytes), negligible.
    ///
    /// Returns `None` if the row is out of bounds.
    pub fn row_cells(&self, y: u16) -> Option<Vec<FlatCell>> {
        let mut len: u16 = 0;
        let ptr = unsafe {
            ghostty_vt_terminal_render_row_cells(self.terminal.handle, y, &mut len)
        };
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

// ---------------------------------------------------------------------------
// SelectionText
// ---------------------------------------------------------------------------

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
        unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(self.ptr, self.len))
        }
    }
}

impl std::ops::Deref for SelectionText {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Debug for SelectionText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
```

### Step 4: Register test module in `lib.rs`

Add to the existing `#[cfg(test)] mod tests` block:

```rust
mod safe_terminal;
```

### Step 5: Write tests

Create `crates/ghostty-vt/src/tests/safe_terminal.rs`:

```rust
use crate::*;

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// VtEvent queue
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// RenderFrame lifecycle
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// RenderFrame data access
// ---------------------------------------------------------------------------

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
    term.feed(b"Row0\nRow1");
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

// ---------------------------------------------------------------------------
// Frame active guard
// ---------------------------------------------------------------------------

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "begin_frame() called while a RenderFrame is already active")]
fn double_begin_frame_panics_in_debug() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.render_update();
    let _frame1 = term.begin_frame();
    let _frame2 = term.begin_frame(); // should panic
}

// ---------------------------------------------------------------------------
// Mode flags
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Scroll
// ---------------------------------------------------------------------------

#[test]
fn scroll_no_crash() {
    let mut term = Terminal::new(80, 24).unwrap();
    let newlines = "\n".repeat(50);
    term.feed(newlines.as_bytes());
    term.scroll_viewport(-5);
    term.scroll_to_top();
    term.scroll_to_bottom();
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Key encoding
// ---------------------------------------------------------------------------

#[test]
fn encode_enter_key() {
    let term = Terminal::new(80, 24).unwrap();
    let mut buf = [0u8; 128];
    // Key.enter = 58 in Ghostty's Key enum
    let n = term.encode_key(58, 0, 1, b"", &mut buf);
    assert!(n > 0, "expected output bytes for enter");
    assert_eq!(buf[0], 0x0D); // \r
}
```

### Step 6: Build and test

```bash
cargo test -p ghostty_vt
```

All existing raw FFI tests still pass (they're in-crate, `pub(crate)` is visible). All new safe API tests validate the wrapper.

---

## Part 2: Cleanup + Close

**Goal:** Verify the complete public API surface is correct, run final checks, close the issue.

**Files:**

- None (verification only)

### Step 1: Verify public API surface

The only public items from `ghostty_vt` should be:

**Types:** `Terminal`, `RenderFrame`, `VtEvent`, `SelectionText`, `DirtyState`, `MouseMode`, `MouseFormat`, `CursorState`, `ColorRGB`, `ColorState`, `FlatCell`

**No** raw FFI functions, callback types, or `Vec<VtEvent>` internals should be accessible from outside the crate.

### Step 2: Run full test suite

```bash
cargo test -p ghostty_vt
```

### Step 3: Close the issue

```bash
dot close ffi-001
```

---

## Design Decisions

### Internal event queue over exposed callbacks

C callbacks (bell, title) are registered internally at `Terminal::new()` time. They push `VtEvent` variants to a heap-stable `Box<Vec<VtEvent>>` via the userdata pointer. The consumer calls `drain_events()` after `feed()`. This:

- Eliminates `unsafe` from the public API entirely
- Matches the architecture's reentrancy prevention model (`04-pty-threading.md`)
- Keeps layering clean: `ghostty_vt` emits `VtEvent`, the `terminal` crate maps to its own `SideEffect` enum

The `Box<Vec<VtEvent>>` pattern (no wrapper struct) keeps the indirection simple. The `Box` ensures the `Vec`'s heap address is stable even if `Terminal` is moved, which is critical because the Zig side holds a raw pointer to it as `userdata`.

### Callback trampolines use `catch_unwind`

If a Rust panic unwinds through the C FFI boundary (Zig → C trampoline → Zig), the behavior is undefined. Wrapping each trampoline body in `std::panic::catch_unwind` absorbs any panic and prevents UB. In practice these trampolines are trivial (push to Vec), so panics are not expected — this is defense in depth.

### `!Send + !Sync` via `PhantomData<Rc<()>>`

A raw pointer `*mut c_void` alone doesn't reliably prevent auto-trait inference for `Send`/`Sync` in all compiler versions. Adding `PhantomData<Rc<()>>` explicitly opts out, since `Rc<()>` is neither `Send` nor `Sync`.

### `&self` vs `&mut self` split

| Method group                | Receiver    | Why                                                |
| --------------------------- | ----------- | -------------------------------------------------- |
| feed, resize, render_update | `&mut self` | Mutates terminal state, invalidates borrowed views |
| scroll, set/clear selection | `&mut self` | Mutates viewport/selection state                   |
| begin_frame, mode queries   | `&self`     | Read-only access                                   |
| encode_key, selection_text  | `&self`     | Reads mode state / selection, no mutation          |
| drain_events                | `&mut self` | Consumes internal queue                            |

### `row_cells` returns `Vec<FlatCell>`, not `&'a [FlatCell]`

**Soundness fix.** The Zig side uses a single shared `flat_cells` buffer per `TerminalHandle`. Each call to `ghostty_vt_terminal_render_row_cells` overwrites this buffer with the requested row's data. Returning `&'a [FlatCell]` would be unsound: calling `row_cells(0)` then `row_cells(1)` would invalidate the first slice while it's still borrowed. Returning `Vec<FlatCell>` (owned copy via `to_vec()`) is safe. The overhead is ~2KB per row (80 cols × 28 bytes/cell), negligible compared to the rendering work that follows.

### `cell_grapheme` returns owned `Vec<u32>`

Same issue as `row_cells`: the Zig side uses a single shared `grapheme_buf` — calling `cell_grapheme` twice overwrites the previous buffer. Returning `&'a [u32]` would be unsound. Returning `Vec<u32>` is safe and the overhead is negligible (grapheme clusters are rare and short, typically 1–4 codepoints).

### `SelectionText` uses `from_utf8_unchecked`

Ghostty produces valid UTF-8 selection text (it stores codepoints internally and serializes via `std.unicode.utf8Encode`). Using `from_utf8_unchecked` avoids a validation pass over potentially large selections. If defensive safety is preferred, switch to `from_utf8_lossy`.

### Frame-active debug guard

Calling `begin_frame()` twice in the same scope is a logic error: the first frame's `Drop` clears dirty flags, silently corrupting the second frame's dirty state view. A `Cell<bool>` flag (`frame_active`) on `Terminal` catches this with a `debug_assert` during development. In release builds, the assert is compiled out — the cost of `Cell<bool>` is negligible regardless.

---

## Unresolved Questions

1. **Key enum constants:** Key integer values come from `ghostty/src/input/key.zig`. Tests use raw integers (e.g., `58` for Enter). A Rust-side `Key` enum with named constants is planned for `terminal-002`.

2. **Event queue growth:** The `Vec<VtEvent>` grows unbounded during a single `feed()` call. In practice, this is bounded by PTY output chunk size (≤64KB, producing at most a few events). No cap needed for v0.
