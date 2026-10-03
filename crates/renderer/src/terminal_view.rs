use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use config::{Config, SpawnConfig};
use font::config::FontConfig;
use font::shared_grid_set::SharedGridSet;

use font::types::FontSize;
use ghostty::{CellSize, ClipboardWrite, TerminalEvent};
use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, CursorStyle, Entity, EntityInputHandler,
    EventEmitter, ExternalSurfaceEvent, ExternalSurfaceHost, FocusHandle, Focusable, Global,
    InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render,
    Styled as _, Task, UTF16Selection, Window, div,
};
use terminal::{AppAction, IoEvent, MouseAction, Options, SessionEffect, TerminalSession};
use ui::scrollbar::{ScrollbarEvent, ScrollbarState};
use utils::floats::NotNan;

use crate::gpu::{RendererThread, RendererThreadHandle, RendererUiUpdate, ThreadOptions};
use crate::terminal_element::TerminalElement;
use crate::types::DerivedConfig;

const FONT_SIZE_STEP: f32 = 1.0;
const MIN_FONT_SIZE: f32 = 1.0;
const MAX_FONT_SIZE: f32 = 255.0;

struct GlobalGridSet(SharedGridSet);

impl Global for GlobalGridSet {}

/// GPUI entity that owns the renderer's view of a terminal session.
///
/// Implements `Render` to produce a `TerminalElement` for each frame.
/// Owns the `FocusHandle` so the terminal can receive keyboard input.
pub struct TerminalView {
    pub session: TerminalSession,

    // TODO: Doc comments
    pub host: Option<ExternalSurfaceHost>,
    thread: RendererThreadHandle,

    /// Overlay scrollbar entity. Handles its own animation and input.
    scrollbar: Entity<ScrollbarState>,
    /// Zoom requests accumulate until prepaint applies them at the current DPI.
    requested_font_points: NotNan<f32>,
    font_size: FontSize,

    focus_handle: FocusHandle,
    /// A left press began on this terminal, independently of its current PTY/local route.
    owns_left_press: bool,
    autoscroll_task: Option<Task<()>>,
}

pub enum TerminalViewEvent {
    TitleChanged,
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl TerminalView {
    pub fn new(
        spawn_config: SpawnConfig,
        config: Config,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Register focus event handlers.
        let focus_handle = cx.focus_handle();

        cx.on_release_in(window, |this, window, _cx| {
            this.cancel_left_press();
            if let Some(host) = this.host.take() {
                if host.close(window).is_err() {
                    log::warn!("failed to close external surface host");
                }
            }
        })
        .detach();

        // Called when the terminal gains focus (or a descendant gains focus).
        cx.on_focus_in(&focus_handle, window, |this, _window, _cx| {
            this.thread.set_focus(true);
            this.session.send_focus_change(true);
        })
        .detach();
        // Called when the terminal loses focus (including all descendants).
        cx.on_focus_out(&focus_handle, window, |this, _event, _window, _cx| {
            this.cancel_left_press();
            this.thread.set_focus(false);
            this.session.send_focus_change(false);
        })
        .detach();

        cx.observe_window_activation(window, |this, window, _cx| {
            if !window.is_window_active() {
                this.cancel_left_press();
            }
        })
        .detach();

        // NOTE(renderer-refactor): Entity<ScrollbarState> looks redundant
        let scrollbar = cx.new(|_cx| ScrollbarState::new());
        cx.subscribe(&scrollbar, |this, _scrollbar, event, _cx| match event {
            ScrollbarEvent::ScrollToRow(row) => this.session.scroll_to_row(*row),
        })
        .detach();

        let font_config = FontConfig::from(&config);
        let dpi = (window.scale_factor() * 96.0) as u16;
        let font_size =
            FontSize { points: NotNan::new(config.font_size).unwrap(), x_dpi: dpi, y_dpi: dpi };

        if !cx.has_global::<GlobalGridSet>() {
            let grid_set = SharedGridSet::new().expect("failed to create initialize SharedGridSet");
            cx.set_global(GlobalGridSet(grid_set));
        }
        let shared_grid_set = &mut cx.global_mut::<GlobalGridSet>().0;
        let grid = shared_grid_set
            .grid_ref(&font_config, font_size)
            .expect("failed to create font grid for terminal");

        let renderer_config = DerivedConfig::from(&config);

        let mut event_rx = None;
        let options = Options {
            cell_size: CellSize::new(grid.metrics.cell_width, grid.metrics.cell_height).unwrap(),
            event_rx: &mut event_rx,
        };
        let session = TerminalSession::new(spawn_config, config, options);

        let event_rx = event_rx.unwrap();
        cx.spawn(async move |this, cx| -> Result<()> {
            while let Ok(event) = event_rx.recv().await {
                this.update(cx, |this, cx| {
                    let effect = match event {
                        TerminalEvent::Bell => this.session.handle_io_event(IoEvent::Bell),
                        TerminalEvent::TitleChanged(title) => {
                            this.session.handle_io_event(IoEvent::TitleChanged(title))
                        }
                        TerminalEvent::ClipboardWrite(write) => {
                            let item = match write {
                                ClipboardWrite::Clear => ClipboardItem { entries: Vec::new() },
                                ClipboardWrite::Text(text) => ClipboardItem::new_string(text),
                            };
                            cx.write_to_clipboard(item);
                            return;
                        }
                    };
                    if effect == SessionEffect::TitleChanged {
                        cx.emit(TerminalViewEvent::TitleChanged);
                    }
                    cx.notify();
                })?;
            }
            Ok(())
        })
        .detach();

        let (ui_tx, ui_rx) = async_channel::bounded(8);

        let (event_tx, event_rx) = crossbeam_channel::unbounded::<ExternalSurfaceEvent>();
        let host = window.create_external_surface_host(event_tx).unwrap();

        let mut swap_chain = None;
        let thread_options = ThreadOptions {
            swap_chain: &mut swap_chain,
            // `Workspace` immediately focuses every newly constructed terminal,
            // but that assignment happens after this constructor returns.
            focused: window.is_window_active(),
        };
        let thread = RendererThread::new(
            renderer_config,
            Arc::clone(&grid),
            Arc::clone(session.terminal()),
            ui_tx,
            event_rx,
            thread_options,
        )
        .expect("Failed to start terminal renderer thread");

        let swap_chain = swap_chain.expect("missing renderer swapchain");
        host.set_swap_chain(window, swap_chain).expect("Failed to set renderer swapchain");

        // Courier
        session.bind_renderer_sender(thread.waker());

        cx.spawn(async move |this, cx| -> Result<()> {
            while let Ok(update) = ui_rx.recv().await {
                this.update(cx, |this, cx| {
                    match update {
                        RendererUiUpdate::Scrollbar(info) => {
                            this.scrollbar.update(cx, |state, _cx| state.sync_snapshot(info))
                        }
                    }
                    cx.notify();
                })?;
            }
            Ok(())
        })
        .detach();

        Self {
            session,
            host: Some(host),
            thread,
            focus_handle,
            requested_font_points: font_size.points,
            font_size,
            scrollbar,
            owns_left_press: false,
            autoscroll_task: None,
        }
    }

    /// Current terminal title, if one has been reported by the child process.
    pub fn title(&self) -> Option<&str> {
        self.session.metadata().title.as_deref()
    }

    /// Terminal background encoded as RGBA for GPUI.
    pub fn default_background_rgba(&self) -> u32 {
        0x1e1e2eff
    }

    /// Clear the unread-output state when this terminal becomes active.
    pub fn mark_output_read(&mut self) {
        self.session.mark_output_read();
    }

    pub fn increase_font_size(&mut self, cx: &mut Context<Self>) {
        self.change_font_size(self.requested_font_points.get() + FONT_SIZE_STEP, cx);
    }

    pub fn decrease_font_size(&mut self, cx: &mut Context<Self>) {
        self.change_font_size(self.requested_font_points.get() - FONT_SIZE_STEP, cx);
    }

    pub fn reset_font_size(&mut self, cx: &mut Context<Self>) {
        self.change_font_size(self.session.config.font_size, cx);
    }

    fn change_font_size(&mut self, points: f32, cx: &mut Context<Self>) {
        self.requested_font_points =
            NotNan::new(points.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)).unwrap();
        cx.notify();
    }

    pub(crate) fn update_font(&mut self, scale: f32, cx: &mut Context<Self>) -> Option<CellSize> {
        let dpi = (scale * 96.0) as u16;
        let next_size = FontSize { points: self.requested_font_points, x_dpi: dpi, y_dpi: dpi };

        if next_size == self.font_size {
            return None;
        }

        // Acquire the replacement before changing live terminal state so a
        // font discovery failure leaves the current grid intact.
        let font_config = FontConfig::from(&self.session.config);
        let shared_grid_set = &mut cx.global_mut::<GlobalGridSet>().0;
        let grid = match shared_grid_set.grid_ref(&font_config, next_size) {
            Ok(grid) => grid,
            Err(error) => {
                log::warn!(
                    "failed to acquire font grid for {}pt at {dpi} DPI: {error:#}",
                    next_size.points
                );
                self.requested_font_points = self.font_size.points;
                return None;
            }
        };

        let cell_size = CellSize::new(grid.metrics.cell_width, grid.metrics.cell_height).unwrap();
        self.thread.set_font_grid(grid);
        self.font_size = next_size;
        Some(cell_size)
    }

    fn set_autoscroll(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !active {
            self.autoscroll_task = None;
            return;
        }
        if self.autoscroll_task.is_some() {
            return;
        }

        self.autoscroll_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(15)).await;
                let active = this.update_in(cx, |view, window, _cx| {
                    let active = view.session.selection_autoscroll_tick(
                        window.mouse_position(),
                        window.modifiers(),
                        window.scale_factor(),
                    );
                    if !active {
                        view.autoscroll_task = None;
                    }
                    active
                });
                let Ok(true) = active else {
                    break;
                };
            }
        }));
    }

    fn cancel_left_press(&mut self) {
        self.autoscroll_task = None;
        if std::mem::take(&mut self.owns_left_press) {
            self.session.terminal().reset_gesture();
        }
    }

    pub(crate) fn owns_left_press(&self) -> bool {
        self.owns_left_press
    }

    pub fn handle_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (self.owns_left_press, event.pressed_button) {
            (true, Some(MouseButton::Left)) => cx.stop_propagation(),
            (true, _) => {
                cx.stop_propagation();
                self.cancel_left_press();
                return;
            }
            (false, Some(MouseButton::Left)) => return,
            (false, _) => {}
        }
        if let Some(active) = self.session.handle_mouse_move(event, window.scale_factor()) {
            self.set_autoscroll(active, window, cx);
        }
    }

    pub fn handle_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button == MouseButton::Left {
            if !std::mem::take(&mut self.owns_left_press) {
                return;
            }
            cx.stop_propagation();
            self.autoscroll_task = None;
        }
        let effect = self.session.handle_mouse_button(
            MouseAction::Release,
            event.button,
            event.position,
            event.modifiers,
            window.scale_factor(),
        );
        if let AppAction::Autoscroll(active) = effect {
            self.set_autoscroll(active, window, cx);
        }
    }

    fn handle_paste(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(ref text) = cx.read_from_clipboard().and_then(|i| i.text()) else {
            // Non-text clipboard payloads (e.g. images) should not be swallowed.
            // Let Ctrl+V propagate so TUI apps can handle native clipboard paste flows.
            return false;
        };

        if text.is_empty() {
            return false;
        }

        self.session.send_paste(text);
        true
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        // Terminal input is an insertion stream, not an editable document.
        self.session.send_text(text);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        _new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        // Preedit must stay out of the input stream until committed.
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .cursor(CursorStyle::IBeam)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let (keystroke, mods) = (&event.keystroke, &event.keystroke.modifiers);

                // Let Windows open the native system menu for Alt+Space.
                if keystroke.key == "space" && keystroke.modifiers == Modifiers::alt() {
                    return;
                }

                // Let Windows produce text/IME events; its key-up still reaches the PTY.
                if event.prefer_character_input {
                    return;
                }

                let key = keystroke.key.to_lowercase();

                // Intercept copy:
                //  Ctrl+C (Windows, if selection exists)
                //  Ctrl+Shift+C (all platforms)
                if mods.control && key == "c" {
                    if let Some(text) = this.session.take_selection_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        return cx.stop_propagation();
                    }
                }

                // Intercept paste: Ctrl+V (Windows) or Ctrl+Shift+V (Linux) or Cmd+V (macOS)
                let is_paste = (mods.control || mods.platform) && key == "v";
                if is_paste && this.handle_paste(cx) {
                    return cx.stop_propagation();
                }

                // Try to handle this key as a scroll key
                let effect = this.session.handle_scroll_key(keystroke);
                if effect == SessionEffect::ViewportScrolled {
                    this.scrollbar.update(cx, |state, cx| {
                        state.on_scroll(window, cx);
                    });
                    return cx.stop_propagation();
                }
                // This event should be sent to the PTY
                this.session.send_key_down_event(event);
                // Consuming the key prevents Windows from also generating WM_CHAR.
                cx.stop_propagation()
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _window, _cx| {
                this.session.send_key_up_event(event)
            }))
            .on_modifiers_changed(cx.listener(|this, event, _window, _cx| {
                this.session.send_modifier_change(event);
            }))
            .on_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                if event.button == MouseButton::Left && this.scrollbar.read(cx).is_dragging() {
                    return;
                }
                if let MouseButton::Navigate(_) = event.button {
                    return;
                }

                window.focus(&this.focus_handle, cx);
                if event.button == MouseButton::Left {
                    this.cancel_left_press();
                    this.owns_left_press = true;
                }
                let effect = this.session.handle_mouse_button(
                    MouseAction::Press,
                    event.button,
                    event.position,
                    event.modifiers,
                    window.scale_factor(),
                );
                match effect {
                    AppAction::WriteClipboard(text) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    AppAction::Autoscroll(active) => {
                        this.set_autoscroll(active, window, cx);
                    }
                    AppAction::None => {}
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event, window, cx| {
                let effect = this.session.handle_scroll_wheel(event, window.scale_factor());
                if let SessionEffect::ViewportScrolled = effect {
                    this.scrollbar.update(cx, |state, cx| {
                        state.on_scroll(window, cx);
                    });
                }
            }))
            .child(TerminalElement { terminal_view: cx.entity() })
            .child(self.scrollbar.clone())
    }
}
