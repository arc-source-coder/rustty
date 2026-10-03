use std::sync::Arc;

use crossbeam_queue::ArrayQueue;
use gpui::{
    Bounds, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, ModifiersChangedEvent, MouseMoveEvent,
    Pixels, Point, ScrollDelta, ScrollWheelEvent,
};

use crate::input::{ToKeyEvent as _, ToZconptyMods as _};
use crate::platform::windows::thread::PlatformThread;
use crate::types::{IoEvent, IoInput, IoMsg, IoThreadNotify};

use crate::io_thread;
use crate::types::{ProcessState, RendererWake, ScrollOp, SessionMetadata};

use config::{Config, SpawnConfig};
use ghostty::{
    CallbackHandle, CellSize, ColorRGB, GridSize, ScreenSize, Terminal, TerminalDimensions,
    TerminalEvent,
};
use zconpty::{
    ConPTY, KeyAction, KeyEvent, MouseAction, MouseButton, MouseEvent, MousePosition, W3cCode,
};

pub const DEFAULT_FG: ColorRGB = ColorRGB::new(0xDD, 0xDD, 0xDD);
pub const DEFAULT_BG: ColorRGB = ColorRGB::new(0x1E, 0x1E, 0x2E);

/// Capacity for the IO thread mailbox.
/// Input now flows through this queue as typed events, so keep some headroom for
/// short bursts of key, mouse, resize, and paste traffic.
const IO_MSG_CHANNEL_CAPACITY: usize = 256;
const TERMINAL_EVENT_CHANNEL_CAPACITY: usize = 32;

/// A terminal's UI-side session, holding its shared Ghostty terminal, ConPTY
/// connection, IO thread/mailbox, callbacks, and renderer wake binding.
pub struct TerminalSession {
    pub config: Config,

    terminal: Arc<Terminal>,
    /// Holds the ConPTY session alive until after the IO thread exits.
    _console_session: Arc<ConPTY>,

    metadata: SessionMetadata,
    process_state: ProcessState,

    /// Written by `TerminalElement::prepaint` each frame.
    /// Used to convert window-space mouse positions to element-local positions.
    pub surface_bounds: Bounds<Pixels>,

    pending_scroll_y: f32,
    dimensions: TerminalDimensions,

    // TODO(courier): Replace the 4 below
    _callback_handle: CallbackHandle,
    io_notify: Arc<IoThreadNotify>,
    renderer_wake: Arc<RendererWake>,
    io_thread: Option<PlatformThread>,
}

/// Notification for the caller after a session operation.
#[derive(Eq, PartialEq)]
pub enum SessionEffect {
    /// Session title metadata changed.
    TitleChanged,
    /// A bell was recorded in the session metadata.
    Bell,
    /// No notification for the caller, even if session state or input changed.
    None,
    /// Viewport scrolling was requested; queued scrolling may not yet be applied.
    ViewportScrolled,
}

#[derive(Debug, Clone)]
pub enum AppAction {
    WriteClipboard(String),
    Autoscroll(bool),
    None,
}

pub struct Options<'a> {
    pub cell_size: CellSize,
    pub event_rx: &'a mut Option<async_channel::Receiver<TerminalEvent>>,
}

impl TerminalSession {
    // TODO(renderer-refactor): Remove SpawnConfig
    pub fn new(spawn_config: SpawnConfig, config: Config, options: Options) -> Self {
        let columns = spawn_config.initial_cols.max(1);
        let rows = spawn_config.initial_rows.max(1);

        let dimensions = TerminalDimensions {
            grid: GridSize { columns, rows },
            screen: ScreenSize::new(
                u32::from(columns).saturating_mul(options.cell_size.width.get()),
                u32::from(rows).saturating_mul(options.cell_size.height.get()),
            )
            .unwrap(),
            cell: options.cell_size,
        };

        let terminal = Arc::new(
            Terminal::new(dimensions, DEFAULT_FG, DEFAULT_BG)
                .expect("failed to allocate ghostty terminal"),
        );
        let console_session =
            Arc::new(ConPTY::new(terminal.handle()).expect("failed to start zconpty session"));

        let renderer_wake = Arc::new(RendererWake::new());
        let callback_renderer_wake = Arc::clone(&renderer_wake);

        // Callbacks run under the terminal lock, so event delivery must never block.
        let (event_tx, event_rx) =
            async_channel::bounded::<TerminalEvent>(TERMINAL_EVENT_CHANNEL_CAPACITY);
        let callback_handle =
            terminal.set_event_sender(event_tx, move || callback_renderer_wake.wake());
        *options.event_rx = Some(event_rx);

        /*    let mut env = HashMap::new();
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
        */

        // io_notify.queue: bounded lock-free — GPUI → IO thread
        let io_queue = Arc::new(ArrayQueue::new(IO_MSG_CHANNEL_CAPACITY));
        let io_notify = Arc::new(IoThreadNotify::new(io_queue));
        let io_thread = io_thread::spawn_suspended(
            Arc::clone(&console_session),
            Arc::clone(&terminal),
            Arc::clone(&io_notify),
            Arc::clone(&renderer_wake),
        )
        .expect("failed to spawn IO thread");

        io_notify.set_io_thread(io_thread.handle());
        io_thread.resume().expect("failed to resume IO thread");

        Self {
            config,
            terminal,
            _console_session: console_session,

            surface_bounds: Bounds::default(),
            dimensions,
            pending_scroll_y: 0.0,

            metadata: SessionMetadata::default(),
            process_state: ProcessState::Running,

            io_notify,
            renderer_wake,
            _callback_handle: callback_handle,
            io_thread: Some(io_thread),
        }
    }

    // --- Public API ---

    /// Access the shared terminal.
    #[inline]
    pub fn terminal(&self) -> &Arc<Terminal> {
        &self.terminal
    }

    /// Bind the session's direct renderer wake path.
    // TODO: Courier
    #[inline]
    pub fn bind_renderer_sender(&self, sender: crossbeam_channel::Sender<()>) {
        self.renderer_wake.bind(sender);
    }

    /// Apply surface size and optional replacement font metrics with one PTY resize.
    #[inline]
    pub fn apply_resize(&mut self, size: ScreenSize, cell_size: Option<CellSize>) {
        if size == self.dimensions.screen && cell_size.is_none() {
            return;
        }
        let cell = cell_size.unwrap_or(self.dimensions.cell);
        self.dimensions = TerminalDimensions {
            grid: GridSize {
                columns: (size.width.get() / cell.width.get()).clamp(1, u16::MAX.into()) as u16,
                rows: (size.height.get() / cell.height.get()).clamp(1, u16::MAX.into()) as u16,
            },
            screen: size,
            cell,
        };

        self.io_notify.send_lossless(IoMsg::Resize(self.dimensions));
    }

    /// Current process state.
    #[inline]
    pub fn process_state(&self) -> &ProcessState {
        &self.process_state
    }

    /// Session metadata (title, cwd, bell count).
    #[inline]
    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    #[inline]
    pub fn display_title(&self) -> &str {
        self.metadata.title.as_deref().unwrap_or("Shell")
    }

    /// Mark output as read (e.g., when the tab becomes active).
    #[inline]
    pub fn mark_output_read(&mut self) {
        self.metadata.has_unread_output = false;
    }

    pub fn handle_io_event(&mut self, event: IoEvent) -> SessionEffect {
        match event {
            IoEvent::Bell => {
                self.metadata.bell_count = self.metadata.bell_count.saturating_add(1);
                self.metadata.has_unread_output = true;
                SessionEffect::Bell
            }
            IoEvent::TitleChanged(title) => {
                self.metadata.title = if title.is_empty() { None } else { Some(title) };
                SessionEffect::TitleChanged
            }
            // NOTE(renderer-refactor): These are never constructued.
            IoEvent::Exited(status) => {
                self.process_state = ProcessState::Exited(status);
                SessionEffect::None
            }
            IoEvent::Error(err) => {
                self.process_state = ProcessState::Error(err);
                SessionEffect::None
            }
        }
    }

    #[inline]
    pub fn send_key_down_event(&self, key_down_event: &KeyDownEvent) {
        if let Some(event) = key_down_event.to_key_event() {
            self.send_key_event(event);
        }
    }

    #[inline]
    pub fn send_key_up_event(&self, key_up_event: &KeyUpEvent) {
        if let Some(event) = key_up_event.to_key_event() {
            self.send_key_event(event);
        }
    }

    fn send_key_event(&self, event: KeyEvent) {
        // Modifier-only events arrive with no logical key code and no text payload.
        // Keep selection for these so Shift/Ctrl/Alt taps don't dismiss a selection.
        let not_modifier = event.code != W3cCode::UNKNOWN || event.text_len > 0;
        let selection_changed =
            event.action != KeyAction::Release && not_modifier && self.terminal.clear_selection();
        let viewport_changed =
            event.action != KeyAction::Release && !self.terminal.viewport_is_bottom();

        if viewport_changed {
            self.terminal.scroll_to_bottom();
        }

        self.io_notify
            .send_lossless(IoMsg::Input(IoInput::Key(event)));

        if selection_changed || viewport_changed {
            self.renderer_wake.wake();
        }
    }

    #[inline]
    pub fn send_modifier_change(&self, event: &ModifiersChangedEvent) {
        if let Some(event) = event.to_key_event() {
            self.io_notify
                .send_lossless(IoMsg::Input(IoInput::Key(event)));
        }
    }

    pub fn send_paste(&self, text: &str) {
        let selection_changed = self.terminal.clear_selection();
        let viewport_changed = !self.terminal.viewport_is_bottom();

        if viewport_changed {
            self.terminal.scroll_to_bottom();
        }

        self.io_notify
            .send_lossless(IoMsg::Input(IoInput::Paste(text.as_bytes().to_vec())));

        if selection_changed || viewport_changed {
            self.renderer_wake.wake();
        }
    }

    #[inline]
    pub fn send_focus_change(&self, focused: bool) {
        self.io_notify
            .send_lossless(IoMsg::Input(IoInput::Focus(focused)));
    }

    /// Copy the current selection and clear it.
    /// Caller is responsible for writing to the clipboard.
    /// Locks internally.
    #[inline]
    pub fn take_selection_text(&self) -> Option<String> {
        let text = self.terminal.take_selection_text()?;
        self.renderer_wake.wake();

        if text.is_empty() { None } else { Some(text) }
    }

    /// Scroll the viewport by delta rows. Negative = up (towards history).
    #[inline]
    pub fn scroll_viewport(&self, delta: i32) {
        let _ = self
            .io_notify
            .try_send(IoMsg::Scroll(ScrollOp::Delta(delta)));
    }

    /// Scroll to the top of scrollback.
    #[inline]
    pub fn scroll_to_top(&self) {
        let _ = self.io_notify.try_send(IoMsg::Scroll(ScrollOp::Top));
    }

    /// Scroll to the bottom (active area).
    #[inline]
    pub fn scroll_to_bottom(&self) {
        let _ = self.io_notify.try_send(IoMsg::Scroll(ScrollOp::Bottom));
    }

    /// Scroll to an absolute row offset (for scrollbar thumb drag).
    ///
    /// This cannot be forwarded to the IO thread like other scroll events
    /// because the absolute row index must be resolved against live
    /// `total_rows`, which changes as output arrives. Forwarding would
    /// introduce a TOCTOU race (stale scrollback geometry).
    /// Locks internally.
    #[inline]
    pub fn scroll_to_row(&self, row: u64) {
        self.terminal.scroll_to_row(row);
        self.renderer_wake.wake();
    }

    pub fn handle_mouse_button(
        &mut self,
        action: MouseAction,
        button: gpui::MouseButton,
        position: Point<Pixels>,
        modifiers: Modifiers,
        scale_factor: f32,
    ) -> AppAction {
        let button = match button {
            gpui::MouseButton::Left => MouseButton::Left,
            gpui::MouseButton::Right => MouseButton::Right,
            gpui::MouseButton::Middle => MouseButton::Middle,
            gpui::MouseButton::Navigate(_) => return AppAction::None,
        };

        let mode = self.terminal.mouse_mode();
        let position = self.mouse_position(position, scale_factor);

        // Finish the gesture before routing; local release preserves multi-click history.
        if button == MouseButton::Left && action == MouseAction::Release {
            self.terminal
                .send_gesture_release(position.x_px, position.y_px);
        }

        if mode.is_mouse_reporting && (!modifiers.shift || mode.is_mouse_shift_capture) {
            let selection_changed = self.terminal.clear_selection();
            self.terminal.reset_gesture();
            if selection_changed {
                self.renderer_wake.wake();
            }

            let event = MouseEvent {
                action,
                button,
                modifiers: modifiers.mods(),
                position,
            };
            self.io_notify
                .send_lossless(IoMsg::Input(IoInput::Mouse(event)));
            return AppAction::Autoscroll(false);
        }

        match (button, action) {
            (MouseButton::Left, MouseAction::Press) => {
                let update = self.terminal.send_gesture_press(
                    position.x_px,
                    position.y_px,
                    modifiers.platform || modifiers.control,
                    modifiers.shift,
                    modifiers.alt,
                );
                if update.needs_redraw {
                    self.renderer_wake.wake();
                }
                return AppAction::Autoscroll(update.autoscroll);
            }
            (MouseButton::Right, MouseAction::Press) => {
                if let Some(text) = self.take_selection_text() {
                    return AppAction::WriteClipboard(text);
                }
            }
            _ => {}
        }

        AppAction::None
    }

    /// Returns an autoscroll update for selection motion; other motion leaves the timer alone.
    pub fn handle_mouse_move(&mut self, event: &MouseMoveEvent, scale_factor: f32) -> Option<bool> {
        let mode = self.terminal.mouse_mode();

        // Map GPUI mouse buttons to zconpty / Ghostty mouse buttons.
        let button: MouseButton = match event.pressed_button {
            Some(b) => match b {
                gpui::MouseButton::Left => MouseButton::Left,
                gpui::MouseButton::Right => MouseButton::Right,
                gpui::MouseButton::Middle => MouseButton::Middle,
                gpui::MouseButton::Navigate(_) => MouseButton::Unknown,
            },
            None => MouseButton::None,
        };

        let position = self.mouse_position(event.position, scale_factor);

        let shift_override =
            event.pressed_button.is_some() && event.modifiers.shift && !mode.is_mouse_shift_capture;
        if mode.is_mouse_reporting && !shift_override {
            let event = MouseEvent {
                action: MouseAction::Move,
                button,
                modifiers: event.modifiers.mods(),
                position,
            };
            self.io_notify
                .send_lossless(IoMsg::Input(IoInput::Mouse(event)));
            return None;
        }

        if button == MouseButton::Left {
            let update =
                self.terminal
                    .send_gesture_drag(position.x_px, position.y_px, event.modifiers.alt);
            if update.needs_redraw {
                self.renderer_wake.wake();
            }
            return Some(update.autoscroll);
        }
        None
    }

    /// Advance selection scrolling atomically, then wake after the terminal lock is released.
    pub fn selection_autoscroll_tick(
        &self,
        position: Point<Pixels>,
        modifiers: Modifiers,
        scale_factor: f32,
    ) -> bool {
        let position = self.mouse_position(position, scale_factor);
        let update =
            self.terminal
                .send_gesture_autoscroll_tick(position.x_px, position.y_px, modifiers.alt);
        if update.needs_redraw {
            self.renderer_wake.wake();
        }
        update.autoscroll
    }

    #[inline]
    fn mouse_position(&self, position: Point<Pixels>, scale_factor: f32) -> MousePosition {
        // `event.position` is window-relative; subtract the element's origin
        // (derived from surface bounds) to get a position local to the terminal surface.
        let x_px = position.x.as_f32() - f32::from(self.surface_bounds.origin.x);
        let y_px = position.y.as_f32() - f32::from(self.surface_bounds.origin.y);

        MousePosition {
            x_px: x_px * scale_factor,
            y_px: y_px * scale_factor,
        }
    }

    pub fn handle_scroll_wheel(&mut self, event: &ScrollWheelEvent, scale: f32) -> SessionEffect {
        let io = self.io_notify.as_ref();

        let rows = match event.delta {
            ScrollDelta::Lines(lines) => match lines.y {
                y if y > 0.0 => lines.y.ceil() as i32,
                y if y < 0.0 => lines.y.floor() as i32,
                _ => return SessionEffect::None,
            },
            ScrollDelta::Pixels(pixels) => {
                let cell_height = self.dimensions.cell.height.get() as f32;
                let total = self.pending_scroll_y + f32::from(pixels.y);
                let rows = (total / cell_height).trunc();

                if rows == 0.0 {
                    self.pending_scroll_y = total;
                    return SessionEffect::None;
                }
                self.pending_scroll_y = total - rows * cell_height;

                rows as i32
            }
        };

        // GPUI sends positive vertical deltas for wheel-up
        let is_scroll_up = rows > 0;
        let mode = self.terminal.mouse_mode();

        if mode.is_mouse_reporting {
            let position = self.mouse_position(event.position, scale);
            let button = match is_scroll_up {
                true => MouseButton::WheelUp,
                false => MouseButton::WheelDown,
            };
            if self.terminal.clear_selection() {
                self.renderer_wake.wake();
            }
            for _ in 0..rows.unsigned_abs() {
                let mouse_event = MouseEvent {
                    action: MouseAction::Press,
                    button,
                    modifiers: event.modifiers.mods(),
                    position,
                };
                io.send_lossless(IoMsg::Input(IoInput::Mouse(mouse_event)));
            }
            return SessionEffect::None;
        }

        if mode.is_alternate_screen && mode.is_mouse_alternate_scroll {
            let code = match is_scroll_up {
                true => W3cCode::from_bytes(b"arrow_up").expect("arrow_up should resolve"),
                false => W3cCode::from_bytes(b"arrow_down").expect("arrow_down should resolve"),
            };
            if self.terminal.clear_selection() {
                self.renderer_wake.wake();
            }
            for _ in 0..rows.unsigned_abs() {
                io.send_lossless(IoMsg::Input(IoInput::Key(KeyEvent::press(code))));
            }
            return SessionEffect::None;
        }

        // This does `-rows` because GPUI and Ghostty use opposite conventions.
        match io.try_send(IoMsg::Scroll(ScrollOp::Delta(-rows))) {
            true => SessionEffect::ViewportScrolled,
            false => SessionEffect::None,
        }
    }

    pub fn handle_scroll_key(&mut self, keystroke: &Keystroke) -> SessionEffect {
        // Locks internally.
        let mode = self.terminal.mouse_mode();
        let shift_pressed = keystroke.modifiers.shift;

        let scroll_operation = 'scroll: {
            let direction = match keystroke.key.to_ascii_lowercase().as_str() {
                "home" if shift_pressed => break 'scroll ScrollOp::Top,
                "end" if shift_pressed => break 'scroll ScrollOp::Bottom,
                "pageup" | "page_up" => -1,
                "pagedown" | "page_down" => 1,
                _ => return SessionEffect::None,
            };

            // Only PageUp/PageDown reach this point.
            if mode.is_alternate_screen && !shift_pressed {
                return SessionEffect::None;
            }

            let page_rows = self.dimensions.grid.rows.saturating_sub(1).max(1) as i32;
            ScrollOp::Delta(direction * page_rows)
        };
        match self.io_notify.try_send(IoMsg::Scroll(scroll_operation)) {
            true => SessionEffect::ViewportScrolled,
            false => SessionEffect::None,
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if let Some(handle) = self.io_thread.take() {
            let io_notify = self.io_notify.clone();
            // Since zconpty is in-process v/s an external process like conhost/OpenConsole.exe,
            // spawn a thread to handle closing the console server and IO thread.
            let _ = std::thread::Builder::new()
                .name("terminal-io-reaper".into())
                .spawn(move || {
                    io_notify.send_lossless(IoMsg::Close);
                    handle.join()
                })
                .inspect_err(|e| log::warn!("failed to spawn terminal-io-reaper: {e}"));
        }
    }
}
