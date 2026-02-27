use std::time::{Duration, Instant};

use async_channel::TryRecvError;
use gpui::{AppContext, Context, Entity, Task, WeakEntity};

use ghostty_vt::{Terminal, VtEvent};
use pty::{PtyCommand, PtyEvent, PtyHandle, WindowSize};

use crate::config::{RenderConfig, SpawnConfig};
use crate::types::{GridSize, ProcessState, SessionId, SessionMetadata, SideEffect};

const DRAIN_BUDGET: Duration = Duration::from_millis(2);

/// Safety timer: if synchronized output stays enabled for longer than
/// this, force-clear the deferred repaint. Matches Ghostty's 1-second
/// timeout in its termio thread.
const SYNC_OUTPUT_SAFETY_TIMEOUT: Duration = Duration::from_secs(1);

pub struct TerminalSession {
    pub id: SessionId,
    terminal: Terminal,
    size: GridSize,
    spawn_config: SpawnConfig,
    // TODO(app-001): Replace with Model<RenderConfig> for cross-session sharing.
    render_config: RenderConfig,
    pty: PtyHandle,
    metadata: SessionMetadata,
    process_state: ProcessState,
    side_effects: Vec<SideEffect>,

    /// When synchronized output mode was first detected as active.
    /// Used for the safety timer. `None` when mode is inactive.
    sync_output_since: Option<Instant>,

    /// Guards against scheduling multiple one-shot re-drain tasks
    /// when the budget expires repeatedly under heavy output.
    pending_redrain: bool,

    // Tasks kept alive for the session's lifetime.
    _drain_task: Task<()>,
    _sync_safety_task: Option<Task<()>>,
}

impl TerminalSession {
    pub fn new(
        spawn_config: SpawnConfig,
        render_config: RenderConfig,
        cx: &mut Context<Self>,
    ) -> Self {
        let size = GridSize::new(spawn_config.initial_cols, spawn_config.initial_rows);

        let terminal =
            Terminal::new(size.cols, size.rows).expect("failed to allocate ghostty terminal");

        let mut env = std::collections::HashMap::new();
        env.insert("TERM".into(), spawn_config.term.clone());
        env.insert("COLORTERM".into(), spawn_config.color_term.clone());

        let pty_options = pty::Options {
            shell: Some(pty::Shell::new(
                spawn_config.shell_program.clone(),
                spawn_config.shell_args.clone(),
            )),
            env,
            ..Default::default()
        };

        let window_size = WindowSize {
            num_cols: size.cols,
            num_lines: size.rows,
            cell_width: 8,   // placeholder — renderer will resize with real metrics
            cell_height: 16, // placeholder
        };

        let pty = PtyHandle::spawn(pty_options, window_size).expect("failed to spawn PTY");

        let drain_task = Self::start_drain_task(pty.event_rx.clone(), cx);

        Self {
            id: SessionId::new(),
            terminal,
            size,
            spawn_config,
            render_config,
            pty,
            metadata: SessionMetadata::default(),
            process_state: ProcessState::Running,
            side_effects: Vec::new(),
            sync_output_since: None,
            pending_redrain: false,
            _drain_task: drain_task,
            _sync_safety_task: None,
        }
    }

    /// Spawn a long-lived async task that awaits PTY events and triggers drain.
    ///
    /// Design: single listener on the channel. Loops on `.recv().await`.
    /// When an event arrives, calls `this.update()` to handle it + drain
    /// remaining events within budget. No `is_empty()` race — the task
    /// either waits for new data (`.recv().await`) or the entity handles
    /// budget overflow via a one-shot re-drain.
    fn start_drain_task(
        event_rx: async_channel::Receiver<PtyEvent>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(
            async move |this: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                loop {
                    match event_rx.recv().await {
                        Ok(first_event) => {
                            let result = this.update(cx, |session, cx| {
                                session.handle_pty_event(first_event, cx);
                                session.drain_pty_output(cx);
                            });
                            if result.is_err() {
                                break; // Entity dropped
                            }
                        }
                        Err(_) => {
                            break; // Channel closed — PTY exited
                        }
                    }
                }
            },
        )
    }

    /// Handle a single PtyEvent.
    fn handle_pty_event(&mut self, event: PtyEvent, _cx: &mut Context<Self>) {
        match event {
            PtyEvent::Output(bytes) => {
                self.terminal.feed(&bytes);
                self.metadata.has_unread_output = true;
            }
            PtyEvent::Exited(status) => {
                self.process_state = ProcessState::Exited(status);
            }
            PtyEvent::Error(e) => {
                self.process_state = ProcessState::Error(e.to_string());
            }
        }
    }

    /// Budgeted drain loop: process pending PTY output within a 2ms budget.
    /// Called after the async task wakes us with the first event already handled.
    fn drain_pty_output(&mut self, cx: &mut Context<Self>) {
        let deadline = Instant::now() + DRAIN_BUDGET;

        while Instant::now() < deadline {
            match self.pty.event_rx.try_recv() {
                Ok(event) => {
                    self.handle_pty_event(event, cx);
                    if !matches!(self.process_state, ProcessState::Running) {
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => break,
            }
        }

        // --- Post-drain: device responses, side effects, repaint ---

        // 1. Process terminal events: flush device responses to PTY
        //    and queue side effects.
        self.process_terminal_events();

        // 2. Process queued side effects (bell, title changes).
        self.process_side_effects();

        // 3. Schedule repaint (respecting synchronized output).
        self.schedule_repaint(cx);

        // 4. If budget expired, schedule a one-shot re-drain.
        //    The pending_redrain flag prevents scheduling multiple
        //    re-drains when budget expires repeatedly under heavy output.
        if Instant::now() >= deadline && !self.pending_redrain {
            self.pending_redrain = true;
            cx.spawn(
                async move |this: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                    let _ = this.update(cx, |session, cx| {
                        session.pending_redrain = false;
                        session.drain_pty_output(cx);
                    });
                },
            )
            .detach();
        }
    }

    /// Process terminal events from the last feed cycle.
    ///
    /// - Device responses → written to PTY immediately (before user input)
    /// - Bell/title → queued as SideEffects for processing after drain
    fn process_terminal_events(&mut self) {
        let events = self.terminal.drain_events();
        for event in events {
            match event {
                VtEvent::DeviceResponse(bytes) => {
                    let _ = self.pty.command_tx.try_send(PtyCommand::Write(bytes));
                }
                VtEvent::Bell => {
                    self.side_effects.push(SideEffect::Bell);
                }
                VtEvent::TitleChanged(title) => {
                    self.side_effects.push(SideEffect::TitleChanged(title));
                }
            }
        }
    }

    /// Process queued side effects.
    fn process_side_effects(&mut self) {
        let effects = std::mem::take(&mut self.side_effects);
        for effect in effects {
            match effect {
                SideEffect::Bell => {
                    self.metadata.bell_count += 1;
                }
                SideEffect::TitleChanged(title) => {
                    self.metadata.title = if title.is_empty() { None } else { Some(title) };
                }
            }
        }
    }

    /// Schedule a repaint, respecting synchronized output mode.
    ///
    /// When synchronized output (DEC 2026) is active, defer cx.notify()
    /// to avoid partial-frame rendering. The terminal state is still
    /// current — only the repaint trigger is deferred.
    fn schedule_repaint(&mut self, cx: &mut Context<Self>) {
        if self.terminal.is_synchronized_output() {
            // Track when sync mode started for the safety timer.
            let since = *self.sync_output_since.get_or_insert_with(Instant::now);

            // Start safety timer if not already running.
            if self._sync_safety_task.is_none() {
                self._sync_safety_task = Some(cx.spawn(
                    async move |this: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                        cx.background_executor()
                            .timer(SYNC_OUTPUT_SAFETY_TIMEOUT)
                            .await;
                        let _ = this.update(cx, |session, cx| {
                            // Force repaint if sync mode is still active
                            // after the timeout.
                            if session.sync_output_since.is_some() {
                                session.sync_output_since = None;
                                session._sync_safety_task = None;
                                cx.notify();
                            }
                        });
                    },
                ));
            }

            // If safety timeout exceeded, force repaint now.
            if since.elapsed() >= SYNC_OUTPUT_SAFETY_TIMEOUT {
                self.sync_output_since = None;
                self._sync_safety_task = None;
                cx.notify();
            }
        } else {
            // Sync mode is off — repaint normally.
            self.sync_output_since = None;
            self._sync_safety_task = None;
            cx.notify();
        }
    }

    // --- Public API ---

    /// Access the underlying terminal (for renderer to call begin_frame()).
    /// NOTE(renderer-001): The renderer will use this to call
    /// `terminal().begin_frame()` for render data access.
    pub fn terminal(&self) -> &Terminal {
        &self.terminal
    }

    /// Set cell pixel dimensions. Called by the renderer whenever font
    /// metrics change. Updates both the shim (for size reports) and the
    /// stored cell size used in PTY resize commands.
    ///
    /// NOTE(renderer-001): Wire this up when the renderer calculates
    /// font metrics during layout.
    pub fn set_cell_size(&mut self, width_px: u16, height_px: u16) {
        self.terminal.set_cell_size(width_px, height_px);
    }

    /// Resize the terminal grid and notify the PTY.
    pub fn resize(
        &mut self,
        new_size: GridSize,
        cell_width: u16,
        cell_height: u16,
        cx: &mut Context<Self>,
    ) {
        if new_size == self.size {
            return;
        }
        self.size = new_size;
        self.terminal.resize(new_size.cols, new_size.rows);

        let window_size = WindowSize {
            num_cols: new_size.cols,
            num_lines: new_size.rows,
            cell_width,
            cell_height,
        };
        let _ = self
            .pty
            .command_tx
            .try_send(PtyCommand::Resize(window_size));

        // Resize force-clears synchronized output (per spec).
        self.sync_output_since = None;
        self._sync_safety_task = None;
        cx.notify();
    }

    /// Write user input bytes to the PTY.
    /// Device responses are always flushed before user input during
    /// drain, so calling this from the UI thread preserves ordering.
    pub fn write_to_pty(&self, data: Vec<u8>) {
        let _ = self.pty.command_tx.try_send(PtyCommand::Write(data));
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
    fn side_effect_variants() {
        let _bell = SideEffect::Bell;
        let _title = SideEffect::TitleChanged("test".into());
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
