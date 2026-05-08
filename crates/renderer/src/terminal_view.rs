use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, CursorStyle, ElementId, Entity,
    FocusHandle, FocusOutEvent, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    KeyUpEvent, Keystroke, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, Render, ScrollWheelEvent, Styled, Subscription,
    Window, div, px,
};
use gpui::{AsyncApp, Task, WeakEntity};
use terminal::{AppAction, MousePosition, TerminalSession};
use ui::scrollbar::{ScrollbarEvent, ScrollbarState};

use crate::gpu::{RendererCellMetrics, RendererTextConfig, RendererUiUpdate, TerminalRenderer};
use crate::terminal_element::TerminalElement;

/// `TerminalElement` writes the surface bounds during prepaint; `TerminalView` reads them
/// each frame for mouse-coordinate conversion and to sync the scrollbar snapshot.
type SurfaceBoundsCell = Rc<Cell<Option<Bounds<Pixels>>>>;

/// GPUI entity that owns the renderer's view of a terminal session.
///
/// Implements `Render` to produce a `TerminalElement` for each frame.
/// Owns the `FocusHandle` so the terminal can receive keyboard input.
pub struct TerminalView {
    session: Entity<TerminalSession>,
    element_id: ElementId,
    focus_handle: FocusHandle,
    /// Renderer-authoritative cell metrics. None until renderer publishes them.
    cell_metrics: Option<RendererCellMetrics>,
    /// Surface bounds written by `TerminalElement::prepaint` each frame.
    /// Used to convert window-space mouse positions to element-local positions
    /// and to sync the scrollbar geometry snapshot.
    surface_bounds: SurfaceBoundsCell,
    /// Overlay scrollbar entity. Handles its own animation and input.
    scrollbar: Entity<ScrollbarState>,
    /// Focus event subscriptions. Must be stored to keep the listeners active.
    _subscriptions: Vec<Subscription>,
    /// GPU renderer companion. Owns the renderer thread for this tab.
    renderer: TerminalRenderer,
    _renderer_update_task: Task<()>,
}

impl TerminalView {
    pub fn new(
        session: Entity<TerminalSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        // Register focus event handlers.
        let focus_in_sub = cx.on_focus_in(&focus_handle, window, Self::handle_focus_in);
        let focus_out_sub = cx.on_focus_out(&focus_handle, window, Self::handle_focus_out);

        let element_id = {
            let s = session.read(cx);
            ElementId::Name(format!("terminal-{}", s.id.as_u64()).into())
        };

        let scrollbar = cx.new(|_cx| ScrollbarState::new());
        let scrollbar_sub = cx.subscribe(&scrollbar, |this, _scrollbar, event, cx| {
            this.handle_scrollbar_event(event, cx);
        });

        let renderer = {
            let terminal = session.read(cx).terminal().clone();
            let render_config = session.read(cx).render_config().read(cx).clone();
            let (ui_tx, ui_rx) = async_channel::bounded(8);
            let renderer = TerminalRenderer::new(
                window,
                terminal,
                RendererTextConfig {
                    font_family: render_config.font_family,
                    font_size: px(render_config.font_size),
                    scale_factor: window.scale_factor(),
                    // Renderer thread owns metric resolution; zero here means
                    // use renderer-side defaults until authoritative metrics are sent.
                    cell_width: px(0.0),
                    line_height: px(0.0),
                    baseline: px(0.0),
                },
                ui_tx,
            )
            .expect("failed to create terminal external surface renderer");
            session.read(cx).bind_renderer_sender(renderer.sender());
            let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                while let Ok(update) = ui_rx.recv().await {
                    let updated = this.update(cx, |this, cx| {
                        match update {
                            RendererUiUpdate::Scrollbar(info) => {
                                this.scrollbar
                                    .update(cx, |state, _cx| state.sync_snapshot(info));
                            }
                            RendererUiUpdate::Metrics(metrics) => {
                                this.cell_metrics = Some(RendererCellMetrics {
                                    cell_width: metrics.cell_width.max(1.0),
                                    line_height: metrics.line_height.max(1.0),
                                });
                            }
                        }
                        cx.notify();
                    });
                    if updated.is_err() {
                        break;
                    }
                }
            });
            (renderer, task)
        };

        Self {
            session,
            element_id,
            focus_handle,
            cell_metrics: None,
            surface_bounds: Rc::new(Cell::new(None)),
            scrollbar,
            _subscriptions: vec![focus_in_sub, focus_out_sub, scrollbar_sub],
            renderer: renderer.0,
            _renderer_update_task: renderer.1,
        }
    }

    /// Called when the terminal gains focus (or a descendant gains focus).
    fn handle_focus_in(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.session.read(cx).send_focus_change(true);
    }

    /// Called when the terminal loses focus (or a descendant loses focus).
    fn handle_focus_out(
        &mut self,
        _event: FocusOutEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session.update(cx, |session, _session_cx| {
            session.surface_focus_out();
        });
        self.session.read(cx).send_focus_change(false);
    }

    pub fn session(&self) -> &Entity<TerminalSession> {
        &self.session
    }

    fn handle_app_action(
        &mut self,
        action: AppAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            AppAction::WriteClipboard(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            AppAction::ViewportScrolled => {
                self.scrollbar
                    .update(cx, |state, cx| state.on_scroll(window, cx));
            }
        }
    }

    fn update_session_with_action(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut TerminalSession, &mut dyn FnMut(AppAction)) -> bool,
    ) {
        let mut action = None;
        self.session.update(cx, |session, _session_cx| {
            let mut emit = |next| action = Some(next);
            update(session, &mut emit);
        });
        if let Some(action) = action {
            self.handle_app_action(action, window, cx);
        }
    }

    fn handle_scrollbar_event(&mut self, event: &ScrollbarEvent, cx: &mut Context<Self>) {
        match event {
            ScrollbarEvent::ScrollToRow(row) => {
                self.session.update(cx, |session, _session_cx| {
                    session.scroll_to_row(*row);
                });
            }
        }
    }

    fn mouse_position(&self, position: Point<Pixels>, scale_factor: f32) -> Option<MousePosition> {
        let metrics = self.cell_metrics?;
        // `event.position` is window-relative; subtract the element's origin
        // (derived from surface bounds) to get a position local to the terminal surface.
        let origin = self
            .surface_bounds
            .get()
            .map(|b| b.origin)
            .unwrap_or_default();
        let x_px = position.x.as_f32() - f32::from(origin.x);
        let y_px = position.y.as_f32() - f32::from(origin.y);
        if x_px < 0.0 || y_px < 0.0 {
            return None;
        }

        let bounds = self.surface_bounds.get()?;
        let cols = (f32::from(bounds.size.width) / metrics.cell_width)
            .floor()
            .max(1.0) as u16;
        let rows = (f32::from(bounds.size.height) / metrics.line_height)
            .floor()
            .max(1.0) as u16;

        let mouse_col = (x_px / metrics.cell_width).floor() as u32;
        let mouse_row = (y_px / metrics.line_height).floor() as u32;

        Some(MousePosition {
            // These will never be 0 because all grid construction sites clamp to min 1.
            x: mouse_col.min((cols - 1) as u32),
            y: mouse_row.min((rows - 1) as u32),
            x_px: x_px * scale_factor,
            y_px: y_px * scale_factor,
        })
    }

    fn handle_left_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Scrollbar drag takes priority: the scrollbar entity registers its own
        // mouse handlers in `ScrollbarElement::paint`, so we only need to ensure
        // that when the scrollbar is actively dragging we skip selection logic.
        if self.scrollbar.read(cx).is_dragging() {
            return;
        }

        window.focus(&self.focus_handle, cx);
        let Some(position) = self.mouse_position(event.position, window.scale_factor()) else {
            return;
        };

        self.session.update(cx, |session, _session_cx| {
            session.handle_left_mouse_down(position, event.click_count as u8, &event.modifiers)
        });
    }

    fn handle_right_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let Some(position) = self.mouse_position(event.position, window.scale_factor()) else {
            return;
        };
        self.session.update(cx, |session, _session_cx| {
            session.handle_right_mouse_down(position, &event.modifiers)
        });
    }

    fn handle_middle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let Some(position) = self.mouse_position(event.position, window.scale_factor()) else {
            return;
        };
        self.session.update(cx, |session, _session_cx| {
            session.handle_middle_mouse_down(position, &event.modifiers)
        });
    }

    fn handle_left_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Scrollbar drag is ended by the scrollbar element's own mouse handler.
        if self.scrollbar.read(cx).is_dragging() {
            return;
        }

        let position = self.mouse_position(event.position, window.scale_factor());
        self.session.update(cx, |session, _session_cx| {
            session.handle_left_mouse_up(position, &event.modifiers)
        });
    }

    fn handle_right_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position = self.mouse_position(event.position, window.scale_factor());
        self.update_session_with_action(window, cx, |session, emit| {
            session.handle_right_mouse_up(position, &event.modifiers, emit)
        });
    }

    fn handle_middle_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(position) = self.mouse_position(event.position, window.scale_factor()) else {
            return;
        };
        self.session.update(cx, |session, _session_cx| {
            session.handle_middle_mouse_up(position, &event.modifiers)
        });
    }

    fn handle_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Scrollbar drag is handled in the scrollbar element's own mouse handler.
        if self.scrollbar.read(cx).is_dragging() {
            return;
        }

        let Some(position) = self.mouse_position(event.position, window.scale_factor()) else {
            return;
        };

        // Map GPUI mouse buttons to zconpty / Ghostty mouse buttons.
        let button: terminal::MouseButton = match event.pressed_button {
            Some(b) => match b {
                MouseButton::Left => terminal::MouseButton::Left,
                MouseButton::Right => terminal::MouseButton::Right,
                MouseButton::Middle => terminal::MouseButton::Middle,
                _ => terminal::MouseButton::Unknown,
            },
            None => terminal::MouseButton::None,
        };

        self.session.update(cx, |session, _session_cx| {
            session.handle_mouse_move(position, button, &event.modifiers)
        });
    }

    fn handle_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(position) = self.mouse_position(event.position, window.scale_factor()) else {
            return;
        };

        let cell_h = self.cell_metrics.map(|m| m.line_height).unwrap_or(16.0);

        self.update_session_with_action(window, cx, |session, emit| {
            session.handle_scroll_wheel(position, event.delta, cell_h, &event.modifiers, emit)
        });
    }

    fn handle_paste(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(clipboard) = cx.read_from_clipboard() else {
            return false;
        };

        let Some(text) = clipboard.text() else {
            // Non-text clipboard payloads (e.g. images) should not be swallowed.
            // Let Ctrl+V propagate so TUI apps can handle native clipboard paste flows.
            return false;
        };

        if text.is_empty() {
            return false;
        }

        self.session.read(cx).send_paste(&text);
        true
    }

    fn handle_copy(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.session.read(cx).take_selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Try to handle a keystroke as a scroll command.
    /// Returns `true` if the key was consumed (should not be forwarded to PTY).
    fn try_handle_scroll_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut action = None;
        let mut emit = |next| action = Some(next);
        let handled = self.session.update(cx, |session, _cx| {
            session.handle_scroll_key(keystroke, &mut emit)
        });
        if let Some(action) = action {
            self.handle_app_action(action, window, cx);
        }
        if handled {
            cx.notify();
        }
        handled
    }

    fn has_selection(&self, cx: &Context<Self>) -> bool {
        self.session.read(cx).has_selection()
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let terminal_element = TerminalElement::new(
            self.session.clone(),
            self.element_id.clone(),
            Rc::clone(&self.surface_bounds),
            self.renderer.host(),
            self.cell_metrics,
        );

        div()
            .size_full()
            .cursor(CursorStyle::IBeam)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.to_lowercase();
                let mods = &event.keystroke.modifiers;

                // Intercept copy: Ctrl+C (Windows, if selection exists) or Ctrl+Shift+C (all platforms)
                let is_copy = mods.control && key == "c" && (mods.shift || this.has_selection(cx));
                if is_copy {
                    this.handle_copy(window, cx);
                    return;
                }

                // Intercept paste: Ctrl+V (Windows) or Ctrl+Shift+V (Linux) or Cmd+V (macOS)
                let is_paste = (mods.control || mods.platform) && key == "v";
                if is_paste && this.handle_paste(window, cx) {
                    return;
                }

                if this.try_handle_scroll_key(&event.keystroke, window, cx) {
                    return;
                }

                this.session.read(cx).send_key_down(
                    &event.keystroke,
                    event.native_key,
                    event.is_held,
                );
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _window, cx| {
                this.session
                    .read(cx)
                    .send_key_up(&event.keystroke, event.native_key);
            }))
            .on_modifiers_changed(cx.listener(
                |this, event: &ModifiersChangedEvent, _window, cx| {
                    let Some(native_key) = event.changed_native_key else {
                        return;
                    };

                    this.session
                        .read(cx)
                        .send_modifier_change(&event.modifiers, native_key);
                },
            ))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::handle_left_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::handle_left_mouse_up))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(Self::handle_right_mouse_down),
            )
            .on_mouse_up(MouseButton::Right, cx.listener(Self::handle_right_mouse_up))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(Self::handle_middle_mouse_down),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(Self::handle_middle_mouse_up),
            )
            .on_mouse_move(cx.listener(Self::handle_mouse_move))
            .on_scroll_wheel(cx.listener(Self::handle_scroll_wheel))
            .child(terminal_element)
            .child(self.scrollbar.clone())
    }
}
