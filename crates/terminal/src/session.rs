use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use crossbeam_queue::ArrayQueue;
use gpui::{AsyncApp, Context, Entity, Keystroke, Modifiers, Task, WeakEntity};

use ghostty::{ColorRGB, Terminal};
use pty::{Options, Shell, WindowSize};

use crate::config::{RenderConfig, SpawnConfig};
use crate::input::{
    ENCODE_BUF_SIZE, encode_focus_change, encode_key_event, encode_mouse_event, encode_paste,
    map_key,
};
use crate::platform::windows::thread::PlatformThread;
use crate::surface::{AppAction, TerminalSurface};
use crate::types::{
    GridSize, IoEvent, IoMsg, IoThreadNotify, ProcessState, ReadThreadNotify, RendererMessage,
    ScrollOp, SessionId, SessionMetadata,
};
use crate::{io_thread, read_thread};

/// Capacity for the IoMsg channel (GPUI + read thread → IO thread).
/// Matches Ghostty's BlockingQueue capacity (64).
const IO_MSG_CHANNEL_CAPACITY: usize = 64;

/// Capacity for the IoEvent channel (read thread → GPUI event task).
const IO_EVENT_CHANNEL_CAPACITY: usize = 64;

#[allow(dead_code)] // tabs not yet implemented
pub struct TerminalSession {
    pub id: SessionId,
    terminal: Arc<Mutex<Terminal>>,
    size: GridSize,
    spawn_config: SpawnConfig,
    render_config: Entity<RenderConfig>,
    /// Sender for user input and resize commands to the IO thread.
    io_notify: Arc<IoThreadNotify>,
    metadata: SessionMetadata,
    process_state: ProcessState,
    surface: TerminalSurface,

    read_thread: Option<PlatformThread>,
    io_thread: Option<PlatformThread>,
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

        let default_fg = ColorRGB::new(0xDD, 0xDD, 0xDD);
        let default_bg = ColorRGB::new(0x1E, 0x1E, 0x2E);
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

        let notify = Arc::new(ReadThreadNotify::new());

        // Channels:
        //   io_notify.queue: bounded 64 lock-free — GPUI + read thread → IO thread
        //   signal_tx/rx:    async_channel bounded 1  — read/IO thread → GPUI signal task
        //   event_tx/rx:     async_channel bounded 64 — read thread → GPUI event task
        let io_queue = Arc::new(ArrayQueue::new(IO_MSG_CHANNEL_CAPACITY));
        let io_notify = Arc::new(IoThreadNotify::new(io_queue));
        let (signal_tx, signal_rx) = async_channel::bounded::<()>(1);
        let (event_tx, event_rx) = async_channel::bounded::<IoEvent>(IO_EVENT_CHANNEL_CAPACITY);

        let read_thread = read_thread::spawn_suspended(
            reader,
            terminal.clone(),
            notify.clone(),
            io_notify.clone(),
            signal_tx.clone(),
            event_tx,
        )
        .expect("failed to spawn read thread");

        let io_thread = io_thread::spawn_suspended(
            writer,
            terminal.clone(),
            notify.clone(),
            io_notify.clone(),
            signal_tx,
        )
        .expect("failed to spawn IO thread");

        notify.set_read_thread(read_thread.handle());
        io_notify.set_io_thread(io_thread.handle());

        log::debug!(
            "terminal session threads spawned: read_tid={}, io_tid={}",
            read_thread.id(),
            io_thread.id()
        );

        read_thread.resume().expect("failed to resume read thread");
        io_thread.resume().expect("failed to resume IO thread");

        // Signal task: awaits render wakeup from read/IO thread, calls cx.notify().
        let signal_task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            while let Ok(()) = signal_rx.recv().await {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break; // Entity dropped
                }
            }
        });

        // Event task: processes IoEvents (Bell, TitleChanged, Exited, Error).
        let event_task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            while let Ok(event) = event_rx.recv().await {
                let result = this.update(cx, |session, cx| {
                    session.handle_io_event(event, cx);
                });
                if result.is_err() {
                    break; // Entity dropped
                }
            }
        });

        Self {
            id: SessionId::new(),
            terminal,
            size,
            spawn_config,
            render_config,
            io_notify,
            metadata: SessionMetadata::default(),
            process_state: ProcessState::Running,
            surface: TerminalSurface::new(),
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

    /// Attach a renderer-thread sender so the IO thread can wake the renderer
    /// after committed resize events. Called by `TerminalView` after startup.
    ///
    /// Only the sender is stored; no renderer-owned state enters `terminal`.
    pub fn attach_renderer_sender(&self, sender: crossbeam_channel::Sender<RendererMessage>) {
        let _ = self.io_notify.try_send(IoMsg::AttachRenderer(sender));
    }

    /// Remove the renderer sender, for example when the view is torn down.
    /// Send failures after detach are treated as normal shutdown races.
    pub fn detach_renderer_sender(&self) {
        let _ = self.io_notify.try_send(IoMsg::DetachRenderer);
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
        let _ = self.io_notify.try_send(IoMsg::Resize(window_size));
    }

    /// Current grid size.
    pub fn current_size(&self) -> GridSize {
        self.size
    }

    /// Write user input bytes to the PTY.
    pub fn write_to_pty(&self, data: Bytes) {
        let _ = self.io_notify.try_send(IoMsg::Input(data));
    }

    fn write_small_to_pty(&self, data: &[u8]) {
        self.io_notify.try_send_input_small(data);
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
        // Resolve key first — pure hash-map lookup, no lock needed.
        // Returns early for modifier-only keys and other unrecognized inputs.
        if map_key(&keystroke.key).is_none() {
            return;
        }

        let opts = {
            let mut term = self.terminal.lock().expect("terminal mutex poisoned");
            let opts = term.input_opts();
            // Scroll to bottom on user input
            if !term.viewport_is_bottom() {
                term.scroll_to_bottom();
            }
            opts
        };

        let mut buf = [0u8; ENCODE_BUF_SIZE];
        if let Some(bytes) = encode_key_event(opts, keystroke, is_held, &mut buf) {
            self.write_small_to_pty(bytes);
        }
    }

    /// Encode and send a paste to the PTY.
    ///
    /// Locks the terminal mutex only to snapshot mode flags (`input_opts()`).
    pub fn send_paste(&self, text: &str) {
        let opts = {
            let mut term = self.terminal.lock().expect("terminal mutex poisoned");
            let opts = term.input_opts();
            if !term.viewport_is_bottom() {
                term.scroll_to_bottom();
            }
            opts
        };

        // Paste output is at most input + bracket fenceposts.
        let max_len = text.len() + 12;
        let mut out = Vec::<u8>::with_capacity(max_len);
        let n = {
            let spare = out.spare_capacity_mut();
            // SAFETY: we expose exactly the spare capacity as a temporary
            // byte slice. `encode_paste` writes initialized bytes only in
            // the returned prefix `n`, then we set_len(n) below.
            let out_slice = unsafe {
                std::slice::from_raw_parts_mut(spare.as_mut_ptr() as *mut u8, spare.len())
            };
            encode_paste(opts, text, out_slice)
        };

        if n != 0 {
            // SAFETY: `n` bytes were initialized by `encode_paste`.
            unsafe { out.set_len(n) };
            self.write_to_pty(Bytes::from(out));
        }
    }

    /// Encode and send a focus change to the PTY.
    ///
    /// Locks the terminal mutex only to snapshot mode flags (`input_opts()`).
    pub fn send_focus_change(&self, focused: bool) {
        let opts = {
            let term = self.terminal.lock().expect("terminal mutex poisoned");
            term.input_opts()
        };
        if let Some(bytes) = encode_focus_change(opts, focused) {
            self.write_small_to_pty(bytes);
        }
    }

    /// Encode and send a mouse event to the PTY.
    ///
    /// Locks the terminal mutex only to snapshot mode flags (`input_opts()`).
    /// Encoding and the PTY write both happen outside the lock.
    ///
    /// Returns `true` if the terminal consumed the event (mouse reporting
    /// is active), `false` if the caller should handle it (e.g. scroll viewport).
    #[allow(clippy::too_many_arguments)]
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
        let opts = {
            let term = self.terminal.lock().expect("terminal mutex poisoned");
            term.input_opts()
        };
        let mut buf = [0u8; ENCODE_BUF_SIZE];
        if let Some(bytes) =
            encode_mouse_event(opts, button, action, shift, alt, ctrl, x, y, &mut buf)
        {
            self.write_small_to_pty(bytes);
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
        let _ = self
            .io_notify
            .try_send(IoMsg::Scroll(ScrollOp::Delta(delta)));
    }

    /// Scroll to the top of scrollback.
    pub fn scroll_to_top(&self) {
        let _ = self.io_notify.try_send(IoMsg::Scroll(ScrollOp::Top));
    }

    /// Scroll to the bottom (active area).
    pub fn scroll_to_bottom(&self) {
        let _ = self.io_notify.try_send(IoMsg::Scroll(ScrollOp::Bottom));
    }

    /// Scroll to an absolute row offset (for scrollbar thumb drag).
    ///
    /// This cannot be forwarded to the IO thread like other scroll events
    /// because the absolute row index must be resolved against live
    /// `total_rows`, which changes as output arrives. Forwarding would
    /// introduce a TOCTOU race (stale scrollback geometry).
    pub fn scroll_to_row(&self, row: u64) {
        self.terminal
            .lock()
            .expect("terminal mutex poisoned")
            .scroll_to_row(row);
    }

    pub fn surface_focus_out(&mut self) {
        self.surface.focus_out();
    }

    pub fn handle_left_mouse_down(
        &mut self,
        pos: (u16, u16),
        click_count: u8,
        mods: &Modifiers,
    ) -> bool {
        self.surface
            .handle_left_mouse_down(&self.terminal, &self.io_notify, pos, click_count, mods)
    }

    pub fn handle_right_mouse_down(&mut self, pos: (u16, u16), mods: &Modifiers) -> bool {
        self.surface
            .handle_right_mouse_down(&self.terminal, &self.io_notify, pos, mods)
    }

    pub fn handle_middle_mouse_down(&mut self, pos: (u16, u16), mods: &Modifiers) -> bool {
        self.surface
            .handle_middle_mouse_down(&self.terminal, &self.io_notify, pos, mods)
    }

    pub fn handle_left_mouse_up(&mut self, pos: Option<(u16, u16)>, mods: &Modifiers) -> bool {
        self.surface
            .handle_left_mouse_up(&self.terminal, &self.io_notify, pos, mods)
    }

    pub fn handle_right_mouse_up(
        &mut self,
        pos: Option<(u16, u16)>,
        mods: &Modifiers,
        emit: &mut dyn FnMut(AppAction),
    ) -> bool {
        self.surface
            .handle_right_mouse_up(&self.terminal, &self.io_notify, pos, mods, emit)
    }

    pub fn handle_middle_mouse_up(&mut self, pos: (u16, u16), mods: &Modifiers) -> bool {
        self.surface
            .handle_middle_mouse_up(&self.terminal, &self.io_notify, pos, mods)
    }

    pub fn handle_mouse_move(
        &mut self,
        pos: (u16, u16),
        pressed_button: u8,
        mods: &Modifiers,
    ) -> bool {
        self.surface
            .handle_mouse_move(&self.terminal, &self.io_notify, pos, pressed_button, mods)
    }

    pub fn handle_scroll_wheel(
        &mut self,
        pos: (u16, u16),
        delta: gpui::ScrollDelta,
        cell_height: f32,
        mods: &Modifiers,
        emit: &mut dyn FnMut(AppAction),
    ) -> bool {
        self.surface.handle_scroll_wheel(
            &self.terminal,
            &self.io_notify,
            pos,
            delta,
            cell_height,
            mods,
            emit,
        )
    }

    pub fn handle_scroll_key(
        &mut self,
        keystroke: &Keystroke,
        emit: &mut dyn FnMut(AppAction),
    ) -> bool {
        self.surface.handle_scroll_key(
            &self.terminal,
            &self.io_notify,
            &keystroke.key,
            &keystroke.modifiers,
            self.size.rows,
            emit,
        )
    }

    pub fn has_selection(&self) -> bool {
        self.terminal
            .lock()
            .expect("terminal mutex poisoned")
            .selection_text()
            .is_some()
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // 1. Send Close (blocking — must not be lost).
        //    IO thread receives Close, signals read thread, drains input, calls
        //    close_async on HPCON, then exits.
        self.io_notify.send_lossless(IoMsg::Close);

        // 2. Join read thread FIRST — it may still be draining conout or sending
        //    device replies through io_notify. After this join, no more reads arrive.
        if let Some(handle) = self.read_thread.take() {
            let _ = handle.alert();
            handle.join();
        }

        // 3. Join IO thread. IO thread already exited from Close handling.
        if let Some(handle) = self.io_thread.take() {
            let _ = handle.alert();
            handle.join();
        }

        // io_notify drops here (IO thread already gone).
        // PtyWriter drops inside IO thread → Conpty::drop() joins close_thread → conin drops.
    }
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
