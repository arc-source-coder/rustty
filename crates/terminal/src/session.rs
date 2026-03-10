use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use bytes::Bytes;
use gpui::{AsyncApp, Context, Entity, Keystroke, Modifiers, Task, WeakEntity};

use ghostty_vt::{ColorRGB, MouseMode, RawCell, ScrollbarInfo, Terminal};
use pty::{Options, Shell, WindowSize};

use crate::config::{RenderConfig, SpawnConfig};
use crate::input::{encode_focus_change, encode_key_event, encode_mouse_event, encode_paste};
use crate::types::{
    GridSize, IoEvent, IoMsg, ProcessState, ReadThreadNotify, SessionId, SessionMetadata,
};
use crate::{io_thread, read_thread};

/// Capacity for the IoMsg channel (GPUI + read thread → IO thread).
/// Matches Ghostty's BlockingQueue capacity (64).
const IO_MSG_CHANNEL_CAPACITY: usize = 64;

/// Capacity for the IoEvent channel (read thread → GPUI event task).
const IO_EVENT_CHANNEL_CAPACITY: usize = 64;

pub struct TerminalSession {
    pub id: SessionId,
    terminal: Arc<Mutex<Terminal>>,
    size: GridSize,
    spawn_config: SpawnConfig,
    render_config: Entity<RenderConfig>,
    /// Sender for user input and resize commands to the IO thread.
    io_tx: crossbeam_channel::Sender<IoMsg>,
    metadata: SessionMetadata,
    process_state: ProcessState,
    /// Cached from the render lock in prepaint; avoids a separate mutex acquisition
    /// in `try_handle_scroll_key`. Updated every frame via `set_render_state`.
    is_alternate_screen: bool,
    /// Cached from the render lock in prepaint; avoids a separate mutex acquisition
    /// in `render`. Updated every frame via `set_render_state`.
    last_scrollbar_info: ScrollbarInfo,

    read_thread: Option<JoinHandle<()>>,
    io_thread: Option<JoinHandle<()>>,
    _signal_task: Task<()>,
    _event_task: Task<()>,
}

impl TerminalSession {
    pub fn new(
        spawn_config: SpawnConfig,
        render_config: Entity<RenderConfig>,
        cx: &mut Context<Self>,
    ) -> Self {
        let size = GridSize::new(spawn_config.initial_cols, spawn_config.initial_rows);

        let default_fg = ColorRGB {
            r: 0xDD,
            g: 0xDD,
            b: 0xDD,
        };
        let default_bg = ColorRGB {
            r: 0x1E,
            g: 0x1E,
            b: 0x2E,
        };
        let terminal = Terminal::new(size.cols, size.rows, default_fg, default_bg)
            .expect("failed to allocate ghostty terminal");
        let terminal = Arc::new(Mutex::new(terminal));

        let mut env = HashMap::new();
        env.insert("TERM".into(), spawn_config.term.clone());
        env.insert("COLORTERM".into(), spawn_config.color_term.clone());

        let pty_options = Options {
            shell: Some(Shell::new(
                spawn_config.shell_program.clone(),
                spawn_config.shell_args.clone(),
            )),
            env,
            ..Default::default()
        };

        let window_size = WindowSize {
            num_cols: size.cols,
            num_lines: size.rows,
            // Placeholders - renderer will update these with real font metrics
            cell_width: 8,
            cell_height: 16,
        };

        let pty = pty::new(&pty_options, window_size).expect("failed to spawn PTY");
        let (reader, writer) = pty.split();

        // ReadThreadNotify: shared signal from IO thread → read thread.
        // Create the IOCP handle here so session owns the lifetime.
        let notify_iocp = unsafe {
            windows_sys::Win32::System::IO::CreateIoCompletionPort(
                windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE,
                std::ptr::null_mut(),
                0,
                0,
            )
        };
        assert!(
            !notify_iocp.is_null(),
            "CreateIoCompletionPort failed for ReadThreadNotify"
        );
        let notify = Arc::new(ReadThreadNotify::new(notify_iocp));

        // Channels:
        //   io_tx/io_rx:     crossbeam bounded 64  — GPUI + read thread → IO thread
        //   signal_tx/rx:    async_channel bounded 1  — read/IO thread → GPUI signal task
        //   event_tx/rx:     async_channel bounded 64 — read thread → GPUI event task
        let (io_tx, io_rx) = crossbeam_channel::bounded::<IoMsg>(IO_MSG_CHANNEL_CAPACITY);
        let (signal_tx, signal_rx) = async_channel::bounded::<()>(1);
        let (event_tx, event_rx) = async_channel::bounded::<IoEvent>(IO_EVENT_CHANNEL_CAPACITY);

        // Read thread gets its own io_tx clone for device replies.
        let io_tx_for_read = io_tx.clone();

        // Spawn read thread (hot path: conout reads + terminal.feed()).
        let read_thread = read_thread::spawn(
            reader,
            terminal.clone(),
            notify.clone(),
            io_tx_for_read,
            signal_tx.clone(),
            event_tx,
        );

        // Spawn IO thread (cold path: writes, resize coalescing, sync-output timer).
        let io_thread = io_thread::spawn(writer, terminal.clone(), notify, io_rx, signal_tx);

        // Signal task: awaits render wakeup from read/IO thread, calls cx.notify().
        let signal_task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                match signal_rx.recv().await {
                    Ok(()) => {
                        if this.update(cx, |_, cx| cx.notify()).is_err() {
                            break; // Entity dropped
                        }
                    }
                    Err(_) => break, // Channel closed
                }
            }
        });

        // Event task: processes IoEvents (Bell, TitleChanged, Exited, Error).
        let event_task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                match event_rx.recv().await {
                    Ok(event) => {
                        let result = this.update(cx, |session, cx| {
                            session.handle_io_event(event, cx);
                        });
                        if result.is_err() {
                            break; // Entity dropped
                        }
                    }
                    Err(_) => break, // Channel closed
                }
            }
        });

        Self {
            id: SessionId::new(),
            terminal,
            size,
            spawn_config,
            render_config,
            io_tx,
            metadata: SessionMetadata::default(),
            process_state: ProcessState::Running,
            is_alternate_screen: false,
            last_scrollbar_info: ScrollbarInfo::default(),
            read_thread: Some(read_thread),
            io_thread: Some(io_thread),
            _signal_task: signal_task,
            _event_task: event_task,
        }
    }

    /// Handle an IoEvent from the read thread.
    fn handle_io_event(&mut self, event: IoEvent, cx: &mut Context<Self>) {
        match event {
            IoEvent::Bell => {
                self.metadata.bell_count += 1;
            }
            IoEvent::TitleChanged(title) => {
                self.metadata.title = if title.is_empty() { None } else { Some(title) };
            }
            IoEvent::Exited(status) => {
                self.process_state = ProcessState::Exited(status);
            }
            IoEvent::Error(err) => {
                self.process_state = ProcessState::Error(err);
            }
        }
        cx.notify();
    }

    // --- Public API ---

    /// Access the shared terminal (for renderer snapshot + resize).
    pub fn terminal_mutex(&self) -> &Arc<Mutex<Terminal>> {
        &self.terminal
    }

    /// Request a resize from the renderer.
    /// Sends IoMsg::Resize to the IO thread, which coalesces (25ms) then signals
    /// the read thread to call ResizePseudoConsole + terminal.resize().
    /// Includes cell dimensions for CSI size reports.
    pub fn request_resize(&mut self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) {
        let new_size = GridSize::new(cols, rows);
        if new_size == self.size {
            return;
        }
        self.size = new_size;

        let window_size = WindowSize {
            num_cols: cols,
            num_lines: rows,
            cell_width,
            cell_height,
        };
        self.io_tx.try_send(IoMsg::Resize(window_size)).ok();
    }

    /// Current grid size.
    pub fn current_size(&self) -> GridSize {
        self.size
    }

    /// Write user input bytes to the PTY.
    pub fn write_to_pty(&self, data: Bytes) {
        self.io_tx.try_send(IoMsg::Input(data)).ok();
    }

    /// Current process state.
    pub fn process_state(&self) -> &ProcessState {
        &self.process_state
    }

    /// Session metadata (title, cwd, bell count).
    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    /// Mark output as read (e.g., when the tab becomes active).
    pub fn mark_output_read(&mut self) {
        self.metadata.has_unread_output = false;
    }

    /// Encode and send a key event to the PTY.
    ///
    /// Locks the terminal mutex only to snapshot mode flags (`input_opts()`).
    /// Encoding and the PTY write both happen outside the lock.
    pub fn send_key_event(&self, keystroke: &Keystroke, is_held: bool) {
        let bytes = {
            let mut term = self.terminal.lock().expect("terminal mutex poisoned");
            let opts = term.input_opts();
            let bytes = encode_key_event(opts, keystroke, is_held);
            // Scroll to bottom on user input (like Windows Terminal / Ghostty).
            if bytes.is_some() && !term.viewport_is_bottom() {
                term.scroll_to_bottom();
            }
            bytes
        };
        if let Some(bytes) = bytes {
            self.write_to_pty(Bytes::from(bytes));
        }
    }

    /// Encode and send a paste to the PTY.
    ///
    /// Locks the terminal mutex only to snapshot mode flags (`input_opts()`).
    pub fn send_paste(&self, text: &str) {
        let bytes = {
            let mut term = self.terminal.lock().expect("terminal mutex poisoned");
            let opts = term.input_opts();
            if !term.viewport_is_bottom() {
                term.scroll_to_bottom();
            }
            encode_paste(opts, text)
        };
        self.write_to_pty(Bytes::from(bytes));
    }

    /// Encode and send a focus change to the PTY.
    ///
    /// Locks the terminal mutex only to snapshot mode flags (`input_opts()`).
    pub fn send_focus_change(&self, focused: bool) {
        let opts = self
            .terminal
            .lock()
            .expect("terminal mutex poisoned")
            .input_opts();
        if let Some(bytes) = encode_focus_change(opts, focused) {
            self.write_to_pty(Bytes::from(bytes));
        }
    }

    /// Encode and send a mouse event to the PTY.
    ///
    /// Locks the terminal mutex only to snapshot mode flags (`input_opts()`).
    /// Encoding and the PTY write both happen outside the lock.
    ///
    /// Returns `true` if the terminal consumed the event (mouse reporting
    /// is active), `false` if the caller should handle it (e.g. scroll viewport).
    pub fn send_mouse_event(
        &self,
        button: u8,
        action: u8,
        shift: bool,
        alt: bool,
        ctrl: bool,
        x: u16,
        y: u16,
    ) -> bool {
        let opts = self
            .terminal
            .lock()
            .expect("terminal mutex poisoned")
            .input_opts();
        if let Some(bytes) = encode_mouse_event(opts, button, action, shift, alt, ctrl, x, y) {
            self.write_to_pty(Bytes::from(bytes));
            true
        } else {
            false
        }
    }

    /// Set terminal selection in viewport coordinates (0-indexed).
    /// start and end are (col, row) pairs where row is u32 for scrollback-aware coords.
    /// rectangular = true for Alt+drag block selection.
    pub fn set_selection(&self, start: (u16, u32), end: (u16, u32), rectangular: bool) {
        self.terminal
            .lock()
            .expect("terminal mutex poisoned")
            .set_selection(start.0, start.1, end.0, end.1, rectangular);
    }

    /// Clear any active terminal selection.
    pub fn clear_selection(&self) {
        self.terminal
            .lock()
            .expect("terminal mutex poisoned")
            .clear_selection();
    }

    /// Copy the currently selected text. Returns None if no selection is active.
    /// Caller is responsible for writing to the clipboard.
    pub fn copy_selection(&self) -> Option<String> {
        self.terminal
            .lock()
            .expect("terminal mutex poisoned")
            .selection_text()
            .map(|s| s.as_str().to_owned())
    }

    /// Access the render config.
    pub fn render_config(&self) -> &Entity<RenderConfig> {
        &self.render_config
    }

    /// Scroll the viewport by delta rows. Negative = up (towards history).
    pub fn scroll_viewport(&self, delta: i32) {
        self.terminal
            .lock()
            .expect("terminal mutex poisoned")
            .scroll_viewport(delta);
    }

    /// Scroll to the top of scrollback.
    pub fn scroll_to_top(&self) {
        self.terminal
            .lock()
            .expect("terminal mutex poisoned")
            .scroll_to_top();
    }

    /// Scroll to the bottom (active area).
    pub fn scroll_to_bottom(&self) {
        self.terminal
            .lock()
            .expect("terminal mutex poisoned")
            .scroll_to_bottom();
    }

    /// Scroll to an absolute row offset (for scrollbar thumb drag).
    pub fn scroll_to_row(&self, row: u64) {
        self.terminal
            .lock()
            .expect("terminal mutex poisoned")
            .scroll_to_row(row);
    }

    /// Whether the alternate screen is currently active.
    pub fn is_alternate_screen(&self) -> bool {
        self.is_alternate_screen
    }

    /// Last scrollbar info captured during prepaint.
    pub fn last_scrollbar_info(&self) -> ScrollbarInfo {
        self.last_scrollbar_info
    }

    /// Update render-derived state captured under the render mutex in prepaint.
    pub fn set_render_state(&mut self, is_alternate_screen: bool, scrollbar_info: ScrollbarInfo) {
        self.is_alternate_screen = is_alternate_screen;
        self.last_scrollbar_info = scrollbar_info;
    }

    // --- Selection gesture handling ---

    /// Handle mouse down for selection. Returns None to send to PTY, Some(anchor) if handled.
    pub fn handle_mouse_down(
        &self,
        pos: (u16, u16),
        click_count: u8,
        mods: &Modifiers,
    ) -> Option<Option<(u16, u16)>> {
        let mut term = self.terminal.lock().expect("terminal mutex poisoned");

        if term.input_opts().mouse_event != MouseMode::None && !mods.shift {
            return None; // Let PTY handle it
        }

        match click_count {
            1 => {
                term.clear_selection();
                Some(Some(pos)) // Return anchor for drag-to-select
            }
            2 => {
                let (start, end) = Self::expand_word(&mut term, pos);
                term.set_selection(start.0, start.1, end.0, end.1, false);
                Some(None)
            }
            3 => {
                let (start, end) = Self::expand_line(&mut term, pos);
                term.set_selection(start.0, start.1, end.0, end.1, false);
                Some(None)
            }
            _ => Some(None),
        }
    }

    const WORD_DELIMITERS: &str = "/\\()\"'-.,:;<>~!@#$%^&*|+=[]{}~?\u{2502}";

    fn classify_codepoint(cp: u32) -> DelimClass {
        if cp == 0 || cp <= 0x20 {
            return DelimClass::Control;
        }
        char::from_u32(cp)
            .filter(|c| Self::WORD_DELIMITERS.contains(*c))
            .map_or(DelimClass::Regular, |_| DelimClass::Delimiter)
    }

    fn effective_codepoint(cells: &[RawCell], col: u16) -> u32 {
        let idx = col as usize;
        cells.get(idx).map_or(0, |cell| {
            if cell.wide() == 2 && idx > 0 {
                cells[idx - 1].codepoint()
            } else {
                cell.codepoint()
            }
        })
    }

    fn expand_word(term: &mut Terminal, pos: (u16, u16)) -> ((u16, u32), (u16, u32)) {
        let (col, row) = pos;
        let frame = term.render_frame();

        let cells = match frame.row_raw(row) {
            Some(c) if !c.is_empty() => c,
            _ => return ((col, row as u32), (col, row as u32)),
        };

        let col = col.min(cells.len().saturating_sub(1) as u16);
        let target = Self::classify_codepoint(Self::effective_codepoint(cells, col));

        // Walk left to find start
        let mut start = col;
        while start > 0
            && Self::classify_codepoint(Self::effective_codepoint(cells, start - 1)) == target
        {
            start -= 1;
        }

        // Walk right to find end
        let mut end = col;
        while (end as usize) + 1 < cells.len()
            && Self::classify_codepoint(Self::effective_codepoint(cells, end + 1)) == target
        {
            end += 1;
        }

        ((start, row as u32), (end, row as u32))
    }

    fn expand_line(term: &mut Terminal, pos: (u16, u16)) -> ((u16, u32), (u16, u32)) {
        let (_, row) = pos;
        let frame = term.render_frame();
        let last_col = frame.cols().saturating_sub(1);
        ((0, row as u32), (last_col, row as u32))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // 1. Send Close (blocking — must not be lost).
        //    IO thread receives Close, signals read thread, drains input, calls
        //    close_async on HPCON, then exits.
        let _ = self.io_tx.send(IoMsg::Close);

        // 2. Join read thread FIRST — it may still be draining conout or sending
        //    device replies via io_tx. After this join, no more reads will arrive.
        if let Some(handle) = self.read_thread.take() {
            let _ = handle.join();
        }

        // 3. Join IO thread. io_tx (our copy) is still live here, so io_rx won't
        //    disconnect prematurely. IO thread already exited from Close handler.
        if let Some(handle) = self.io_thread.take() {
            let _ = handle.join();
        }

        // io_tx drops here → io_rx disconnects (IO thread already gone).
        // PtyWriter drops inside IO thread → Conpty::drop() joins close_thread → conin drops.
    }
}

/// Delimiter classification for word selection.
#[derive(PartialEq, Eq, Clone, Copy)]
enum DelimClass {
    Control,   // space / empty / wide-continuation spacer
    Delimiter, // punctuation delimiter
    Regular,   // word character
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RenderConfig, SpawnConfig};

    // TerminalSession requires a GPUI App context to construct.
    // Full integration tests will be added when the GPUI test
    // harness is established. For now, verify supporting types.

    #[test]
    fn session_id_uniqueness() {
        let a = SessionId::new();
        let b = SessionId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn grid_size_equality() {
        let a = GridSize::new(80, 24);
        let b = GridSize::new(80, 24);
        assert_eq!(a, b);
        assert_ne!(a, GridSize::new(120, 40));
    }

    #[test]
    fn default_spawn_config() {
        let config = SpawnConfig::default();
        assert_eq!(config.initial_cols, 80);
        assert_eq!(config.initial_rows, 24);
        assert_eq!(config.term, "xterm-256color");
    }

    #[test]
    fn default_render_config() {
        let config = RenderConfig::default();
        assert_eq!(config.font_size, 14.0);
    }

    #[test]
    fn process_state_variants() {
        assert!(matches!(ProcessState::Running, ProcessState::Running));
        assert!(matches!(
            ProcessState::Error("test".into()),
            ProcessState::Error(_)
        ));
    }

    #[test]
    fn metadata_defaults() {
        let meta = SessionMetadata::default();
        assert!(meta.title.is_none());
        assert!(meta.cwd.is_none());
        assert_eq!(meta.bell_count, 0);
        assert!(!meta.has_unread_output);
    }
}
