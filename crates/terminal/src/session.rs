use std::sync::Arc;

use crossbeam_queue::ArrayQueue;
use gpui::{AsyncApp, Context, Entity, EventEmitter, Keystroke, Modifiers, Task, WeakEntity};

use crate::config::{RenderConfig, SpawnConfig};
use crate::input::{normalize_key_event, normalize_modifier_event};
use crate::io_thread;
use crate::platform::windows::thread::PlatformThread;
use crate::surface::{AppAction, TerminalSurface};
use crate::types::{
    IoEvent, IoInput, IoMsg, IoThreadNotify, ProcessState, RendererWake, ScrollOp, SessionId,
    SessionMetadata, TerminalDimensions,
};
use ghostty::{CallbackHandle, ColorRGB, Event as TerminalEvent, Terminal};
use zconpty::{ConPTY, KeyAction, KeyEvent, MouseButton, MousePosition, key_from_w3c};

/// Capacity for the IO thread mailbox.
/// Input now flows through this queue as typed events, so keep some headroom for
/// short bursts of key, mouse, resize, and paste traffic.
const IO_MSG_CHANNEL_CAPACITY: usize = 256;

#[allow(dead_code)] // tabs not yet implemented
pub struct TerminalSession {
    pub id: SessionId,
    /// Holds the ConPTY session alive until after the IO thread exits.
    console_session: Arc<ConPTY>,
    callback_handle: CallbackHandle,
    terminal: Arc<Terminal>,
    dimensions: TerminalDimensions,
    spawn_config: SpawnConfig,
    render_config: Entity<RenderConfig>,
    /// Sender for user input and resize commands to the IO thread.
    io_notify: Arc<IoThreadNotify>,
    renderer_wake: Arc<RendererWake>,
    default_background: ColorRGB,
    metadata: SessionMetadata,
    process_state: ProcessState,
    surface: TerminalSurface,

    io_thread: Option<PlatformThread>,
    _event_task: Task<()>,
}

pub enum SessionEvent {
    TitleChanged,
}

impl EventEmitter<SessionEvent> for TerminalSession {}

impl TerminalSession {
    pub fn new(
        spawn_config: SpawnConfig,
        render_config: Entity<RenderConfig>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Channels:
        //   io_notify.queue: bounded lock-free — GPUI → IO thread
        //   event_tx/rx:     unbounded bell/title events — Ghostty callbacks → GPUI task
        let io_queue = Arc::new(ArrayQueue::new(IO_MSG_CHANNEL_CAPACITY));
        let io_notify = Arc::new(IoThreadNotify::new(io_queue));
        let (event_tx, event_rx) = async_channel::unbounded::<TerminalEvent>();
        let renderer_wake = Arc::new(RendererWake::new());

        let default_fg = ColorRGB::new(0xDD, 0xDD, 0xDD);
        let default_bg = ColorRGB::new(0x1E, 0x1E, 0x2E);
        let terminal = Arc::new(
            Terminal::new(
                spawn_config.initial_cols,
                spawn_config.initial_rows,
                default_fg,
                default_bg,
            )
            .expect("failed to allocate ghostty terminal"),
        );
        let console_session =
            Arc::new(ConPTY::new(terminal.handle()).expect("failed to start console session"));
        let callback_renderer_wake = renderer_wake.clone();
        let callback_handle =
            terminal.set_event_sender(event_tx, move || callback_renderer_wake.wake());

        let dimensions = initial_dimensions(&spawn_config);
        terminal.set_dimensions(dimensions);

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
        let io_thread = io_thread::spawn_suspended(
            console_session.clone(),
            terminal.clone(),
            io_notify.clone(),
            renderer_wake.clone(),
        )
        .expect("failed to spawn IO thread");

        io_notify.set_io_thread(io_thread.handle());

        log::debug!("terminal session thread spawned: io_tid={}", io_thread.id());

        io_thread.resume().expect("failed to resume IO thread");

        let event_task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            while let Ok(event) = event_rx.recv().await {
                if this
                    .update(cx, |this, cx| match event {
                        TerminalEvent::Bell => this.handle_io_event(IoEvent::Bell, cx),
                        TerminalEvent::TitleChanged(title) => {
                            this.handle_io_event(IoEvent::TitleChanged(title), cx)
                        }
                    })
                    .is_err()
                {
                    break; // Entity dropped
                }
            }
        });

        Self {
            id: SessionId::new(),
            console_session,
            callback_handle,
            terminal,
            dimensions,
            spawn_config,
            render_config,
            io_notify,
            renderer_wake,
            default_background: default_bg,
            metadata: SessionMetadata::default(),
            process_state: ProcessState::Running,
            surface: TerminalSurface::new(),
            io_thread: Some(io_thread),
            _event_task: event_task,
        }
    }

    // --- Public API ---

    /// Access the shared terminal.
    pub fn terminal(&self) -> &Arc<Terminal> {
        &self.terminal
    }

    /// Bind the session's direct renderer wake path.
    pub fn bind_renderer_sender(
        &self,
        sender: crossbeam_channel::Sender<crate::types::RendererMessage>,
    ) {
        self.renderer_wake.bind(sender);
    }

    /// Apply the latest terminal dimensions from layout.
    pub fn apply_resize(&mut self, dimensions: TerminalDimensions) {
        if self.dimensions == dimensions {
            return;
        }

        self.dimensions = dimensions;
        let _ = self.io_notify.try_send(IoMsg::Resize(dimensions));
    }

    /// Current process state.
    pub fn process_state(&self) -> &ProcessState {
        &self.process_state
    }

    /// Session metadata (title, cwd, bell count).
    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    pub fn display_title(&self) -> &str {
        self.metadata
            .title
            .as_deref()
            .unwrap_or(self.spawn_config.shell_program.as_str())
    }

    // TODO: Wire up actual terminal background.
    pub fn default_background_rgba(&self) -> u32 {
        u32::from_be_bytes([
            self.default_background.r(),
            self.default_background.g(),
            self.default_background.b(),
            0xff,
        ])
    }

    /// Mark output as read (e.g., when the tab becomes active).
    pub fn mark_output_read(&mut self) {
        self.metadata.has_unread_output = false;
    }

    fn handle_io_event(&mut self, event: IoEvent, cx: &mut Context<Self>) {
        match event {
            IoEvent::Bell => {
                self.metadata.bell_count = self.metadata.bell_count.saturating_add(1);
                self.metadata.has_unread_output = true;
            }
            IoEvent::TitleChanged(title) => {
                self.metadata.title = if title.is_empty() { None } else { Some(title) };
                cx.emit(SessionEvent::TitleChanged);
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

    pub fn send_key_event(
        &self,
        keystroke: &Keystroke,
        native_key: Option<gpui::WindowsNativeKey>,
        action: KeyAction,
        is_held: bool,
    ) {
        let Some(event) = normalize_key_event(keystroke, native_key, action, is_held) else {
            return;
        };

        let mut needs_renderer_wake = false;

        if should_clear_selection_on_key_event(&event) && self.terminal.selection_text().is_some() {
            self.terminal.clear_selection();
            needs_renderer_wake = true;
        }

        if should_reveal_key_input(action) && !self.terminal.viewport_is_bottom() {
            self.terminal.scroll_to_bottom();
            needs_renderer_wake = true;
        }

        if needs_renderer_wake {
            self.renderer_wake.wake();
        }

        self.io_notify
            .send_lossless(IoMsg::Input(IoInput::Key(event)));
    }

    pub fn send_key_down(
        &self,
        keystroke: &Keystroke,
        native_key: Option<gpui::WindowsNativeKey>,
        is_held: bool,
    ) {
        self.send_key_event(keystroke, native_key, KeyAction::Press, is_held);
    }

    pub fn send_key_up(&self, keystroke: &Keystroke, native_key: Option<gpui::WindowsNativeKey>) {
        self.send_key_event(keystroke, native_key, KeyAction::Release, false);
    }

    pub fn send_modifier_change(&self, modifiers: &Modifiers, native_key: gpui::WindowsNativeKey) {
        let event = normalize_modifier_event(modifiers, native_key);

        self.io_notify
            .send_lossless(IoMsg::Input(IoInput::Key(event)));
    }

    pub fn send_paste(&self, text: &str) {
        let mut needs_renderer_wake = false;

        if self.terminal.selection_text().is_some() {
            self.terminal.clear_selection();
            needs_renderer_wake = true;
        }

        if !self.terminal.viewport_is_bottom() {
            self.terminal.scroll_to_bottom();
            needs_renderer_wake = true;
        }

        if needs_renderer_wake {
            self.renderer_wake.wake();
        }

        self.io_notify
            .send_lossless(IoMsg::Input(IoInput::Paste(text.as_bytes().to_vec())));
    }

    pub fn send_focus_change(&self, focused: bool) {
        self.io_notify
            .send_lossless(IoMsg::Input(IoInput::Focus(focused)));
    }

    pub fn send_scroll_arrow(&self, scroll_up: bool) {
        let code = if scroll_up {
            key_from_w3c(b"arrow_up").expect("arrow_up should resolve")
        } else {
            key_from_w3c(b"arrow_down").expect("arrow_down should resolve")
        };
        self.io_notify
            .send_lossless(IoMsg::Input(IoInput::Key(KeyEvent::press(code))));
    }

    /// Set terminal selection in viewport coordinates (0-indexed).
    /// start and end are (col, row) pairs where row is u32 for scrollback-aware coords.
    /// rectangular = true for Alt+drag block selection.
    /// Locks internally.
    pub fn set_selection(&self, start: (u16, u32), end: (u16, u32), rectangular: bool) {
        self.terminal
            .set_selection(start.0, start.1, end.0, end.1, rectangular);
        self.renderer_wake.wake();
    }

    /// Copy the current selection and clear it.
    /// Caller is responsible for writing to the clipboard.
    /// Locks internally.
    pub fn take_selection_text(&self) -> Option<String> {
        let text = self.surface.take_selection_text(&self.terminal);
        if text.is_some() {
            self.renderer_wake.wake();
        }
        text
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
    /// Locks internally.
    pub fn scroll_to_row(&self, row: u64) {
        self.terminal.scroll_to_row(row);
        self.renderer_wake.wake();
    }

    pub fn surface_focus_out(&mut self) {
        self.surface.focus_out();
    }

    pub fn handle_left_mouse_down(
        &mut self,
        position: MousePosition,
        click_count: u8,
        mods: &Modifiers,
    ) -> bool {
        let changed = self.surface.handle_left_mouse_down(
            &self.terminal,
            &self.io_notify,
            position,
            click_count,
            mods,
        );

        if !self.terminal.is_mouse_reporting() || mods.shift {
            if changed {
                self.renderer_wake.wake();
            }
        }
        changed
    }

    pub fn handle_right_mouse_down(&mut self, position: MousePosition, mods: &Modifiers) -> bool {
        self.surface
            .handle_right_mouse_down(&self.terminal, &self.io_notify, position, mods)
    }

    pub fn handle_middle_mouse_down(&mut self, position: MousePosition, mods: &Modifiers) -> bool {
        self.surface
            .handle_middle_mouse_down(&self.terminal, &self.io_notify, position, mods)
    }

    pub fn handle_left_mouse_up(
        &mut self,
        position: Option<MousePosition>,
        mods: &Modifiers,
    ) -> bool {
        self.surface
            .handle_left_mouse_up(&self.terminal, &self.io_notify, position, mods)
    }

    pub fn handle_right_mouse_up(
        &mut self,
        position: Option<MousePosition>,
        mods: &Modifiers,
        emit: &mut dyn FnMut(AppAction),
    ) -> bool {
        let changed = self.surface.handle_right_mouse_up(
            &self.terminal,
            &self.io_notify,
            position,
            mods,
            emit,
        );

        if !self.terminal.is_mouse_reporting() || mods.shift {
            if changed {
                self.renderer_wake.wake();
            }
        }
        changed
    }

    pub fn handle_middle_mouse_up(&mut self, position: MousePosition, mods: &Modifiers) -> bool {
        self.surface
            .handle_middle_mouse_up(&self.terminal, &self.io_notify, position, mods)
    }

    pub fn handle_mouse_move(
        &mut self,
        pos: MousePosition,
        pressed_button: MouseButton,
        mods: &Modifiers,
    ) -> bool {
        let changed = self.surface.handle_mouse_move(
            &self.terminal,
            &self.io_notify,
            pos,
            pressed_button,
            mods,
        );

        if pressed_button == MouseButton::Left
            && (!self.terminal.is_mouse_reporting() || mods.shift)
        {
            if changed {
                self.renderer_wake.wake();
            }
        }
        changed
    }

    pub fn handle_scroll_wheel(
        &mut self,
        position: MousePosition,
        delta: gpui::ScrollDelta,
        cell_height: f32,
        mods: &Modifiers,
        emit: &mut dyn FnMut(AppAction),
    ) -> bool {
        self.surface.handle_scroll_wheel(
            &self.terminal,
            &self.io_notify,
            position,
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
        let cell_height_px = self.dimensions.cell_height_px.max(1);
        let rows = self.dimensions.screen_height_px / cell_height_px;

        self.surface.handle_scroll_key(
            &self.terminal,
            &self.io_notify,
            &keystroke.key,
            &keystroke.modifiers,
            rows.max(1) as u16,
            emit,
        )
    }

    /// Locks internally.
    pub fn has_selection(&self) -> bool {
        self.terminal.selection_text().is_some()
    }
}

fn initial_dimensions(spawn_config: &SpawnConfig) -> TerminalDimensions {
    // Bootstrap dimensions only. The renderer publishes authoritative text metrics
    // when it starts, and layout then replaces this initial guess via apply_resize.
    let cell_width_px = 8;
    let cell_height_px = 16;

    TerminalDimensions {
        screen_width_px: u32::from(spawn_config.initial_cols) * cell_width_px,
        screen_height_px: u32::from(spawn_config.initial_rows) * cell_height_px,
        cell_width_px,
        cell_height_px,
    }
}

fn should_reveal_key_input(action: KeyAction) -> bool {
    match action {
        KeyAction::Release => false,
        KeyAction::Press | KeyAction::Repeat => true,
    }
}

fn should_clear_selection_on_key_event(event: &KeyEvent) -> bool {
    if event.action == KeyAction::Release {
        return false;
    }

    // Modifier-only events arrive with no logical key code and no text payload.
    // Keep selection for these so Shift/Ctrl/Alt taps don't dismiss a selection.
    event.code.as_raw() != 0 || event.text_len > 0
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if let Some(handle) = self.io_thread.take() {
            let io_notify = self.io_notify.clone();
            // Since zconpty is in-process v/s external process like conhost/OpenConsole.exe,
            // spawn a thread to handle closing the console server and IO thread.
            let builder = std::thread::Builder::new().name("terminal-io-reaper".into());
            if let Err(err) = builder.spawn(move || {
                io_notify.send_lossless(IoMsg::Close);
                handle.join();
            }) {
                log::warn!("failed to spawn terminal-io-reaper: {err}");
            }
        }
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
    fn key_event_selection_clear_ignores_modifier_only_events() {
        let mut event = KeyEvent::new(KeyAction::Press, zconpty::W3cCode::from_raw(0));
        event.text_len = 0;

        assert!(!should_clear_selection_on_key_event(&event));
    }

    #[test]
    fn key_event_selection_clear_for_non_modifier_press_or_repeat() {
        let press = KeyEvent::new(KeyAction::Press, key_from_w3c(b"enter").expect("enter key"));
        let repeat = KeyEvent::new(KeyAction::Repeat, key_from_w3c(b"key_a").expect("a key"));

        assert!(should_clear_selection_on_key_event(&press));
        assert!(should_clear_selection_on_key_event(&repeat));
    }

    #[test]
    fn key_event_selection_clear_for_text_without_logical_key() {
        let mut event = KeyEvent::new(KeyAction::Press, zconpty::W3cCode::from_raw(0));
        event.text[0] = b'a';
        event.text_len = 1;

        assert!(should_clear_selection_on_key_event(&event));
    }

    #[test]
    fn key_event_selection_clear_ignores_release_events() {
        let event = KeyEvent::new(
            KeyAction::Release,
            key_from_w3c(b"enter").expect("enter key"),
        );

        assert!(!should_clear_selection_on_key_event(&event));
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
