use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, CursorStyle, ElementId, Entity,
    FocusHandle, FocusOutEvent, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels,
    Point, Render, ScrollWheelEvent, Styled, Subscription, Window, div, px,
};
use gpui::{AsyncApp, Task, WeakEntity};
use terminal::{AppAction, TerminalSession};
use ui::scrollbar::ScrollbarState;

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
        // Re-render whenever the session is notified (IO thread data arrival).
        cx.observe(&session, |_this, _session, cx| {
            cx.notify();
        })
        .detach();

        let focus_handle = cx.focus_handle();

        // Register focus event handlers.
        let focus_in_sub = cx.on_focus_in(&focus_handle, window, Self::handle_focus_in);
        let focus_out_sub = cx.on_focus_out(&focus_handle, window, Self::handle_focus_out);

        let element_id = {
            let s = session.read(cx);
            ElementId::Name(format!("terminal-{}", s.id.as_u64()).into())
        };

        let scrollbar = cx.new(|_cx| ScrollbarState::new(session.clone()));

        let renderer = {
            let terminal = session.read(cx).terminal_mutex().clone();
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
            .expect("Window::d3d11_device() returned None - D3D11 backend required");
            session.read(cx).attach_renderer_sender(renderer.sender());
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
            _subscriptions: vec![focus_in_sub, focus_out_sub],
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
        self.session.update(cx, |session, _cx| {
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

    fn pixel_to_cell(
        &mut self,
        position: Point<Pixels>,
        _window: &Window,
        cx: &Context<Self>,
    ) -> Option<(u16, u16)> {
        // Lazily populate metrics. After first render, this is always Some.
        let metrics = self.cell_metrics?;

        // `event.position` is window-relative; subtract the element's origin
        // (derived from surface bounds) to get a position local to the terminal surface.
        let origin = self
            .surface_bounds
            .get()
            .map(|b| b.origin)
            .unwrap_or_default();
        let x_px = f32::from(position.x) - f32::from(origin.x);
        let y_px = f32::from(position.y) - f32::from(origin.y);
        if x_px < 0.0 || y_px < 0.0 {
            return None;
        }

        let grid = self.session.read(cx).current_size();
        if grid.cols == 0 || grid.rows == 0 {
            return None;
        }
        let col = (x_px / metrics.cell_width).floor() as u16;
        let row = (y_px / metrics.line_height).floor() as u16;
        Some((col.min(grid.cols - 1), row.min(grid.rows - 1)))
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
        let Some((x, y)) = self.pixel_to_cell(event.position, window, cx) else {
            return;
        };

        let needs_notify = self.session.update(cx, |session, _cx| {
            session.handle_left_mouse_down((x, y), event.click_count as u8, &event.modifiers)
        });
        if needs_notify {
            cx.notify();
        }
    }

    fn handle_right_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let Some((x, y)) = self.pixel_to_cell(event.position, window, cx) else {
            return;
        };
        let needs_notify = self.session.update(cx, |session, _cx| {
            session.handle_right_mouse_down((x, y), &event.modifiers)
        });
        if needs_notify {
            cx.notify();
        }
    }

    fn handle_middle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let Some((x, y)) = self.pixel_to_cell(event.position, window, cx) else {
            return;
        };
        let needs_notify = self.session.update(cx, |session, _cx| {
            session.handle_middle_mouse_down((x, y), &event.modifiers)
        });
        if needs_notify {
            cx.notify();
        }
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

        let pos = self.pixel_to_cell(event.position, window, cx);
        let needs_notify = self.session.update(cx, |session, _cx| {
            session.handle_left_mouse_up(pos, &event.modifiers)
        });
        if needs_notify {
            cx.notify();
        }
    }

    fn handle_right_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pos = self.pixel_to_cell(event.position, window, cx);
        let mut action = None;
        let mut emit = |next| action = Some(next);
        let needs_notify = self.session.update(cx, |session, _cx| {
            session.handle_right_mouse_up(pos, &event.modifiers, &mut emit)
        });
        if let Some(action) = action {
            self.handle_app_action(action, window, cx);
        }
        if needs_notify {
            cx.notify();
        }
    }

    fn handle_middle_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((x, y)) = self.pixel_to_cell(event.position, window, cx) else {
            return;
        };
        let needs_notify = self.session.update(cx, |session, _cx| {
            session.handle_middle_mouse_up((x, y), &event.modifiers)
        });
        if needs_notify {
            cx.notify();
        }
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

        let Some((x, y)) = self.pixel_to_cell(event.position, window, cx) else {
            return;
        };
        let button = event
            .pressed_button
            .and_then(mouse_button_code)
            .unwrap_or(3);

        let needs_notify = self.session.update(cx, |session, _cx| {
            session.handle_mouse_move((x, y), button, &event.modifiers)
        });
        if needs_notify {
            cx.notify();
        }
    }

    fn handle_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((x, y)) = self.pixel_to_cell(event.position, window, cx) else {
            return;
        };

        let cell_h = self.cell_metrics.map(|m| m.line_height).unwrap_or(16.0);

        let mut action = None;
        let mut emit = |next| action = Some(next);
        let needs_notify = self.session.update(cx, |session, _cx| {
            session.handle_scroll_wheel((x, y), event.delta, cell_h, &event.modifiers, &mut emit)
        });
        if let Some(action) = action {
            self.handle_app_action(action, window, cx);
        }
        if needs_notify {
            cx.notify();
        }
    }

    fn handle_paste(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(clipboard) = cx.read_from_clipboard() else {
            return;
        };
        let Some(text) = clipboard.text() else { return };
        if text.is_empty() {
            return;
        }
        self.session.read(cx).send_paste(&text);
    }

    fn handle_copy(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.session.read(cx).copy_selection() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.session.read(cx).clear_selection();
            cx.notify();
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
        self.renderer
            .sender()
            .try_send(terminal::RendererMessage::Wake)
            .ok();

        let terminal_element = TerminalElement::new(
            self.session.clone(),
            self.element_id.clone(),
            Rc::clone(&self.surface_bounds),
            self.renderer.slot(),
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
                if is_paste {
                    this.handle_paste(window, cx);
                    return;
                }

                if this.try_handle_scroll_key(&event.keystroke, window, cx) {
                    return;
                }

                this.session
                    .read(cx)
                    .send_key_event(&event.keystroke, event.is_held);
            }))
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

fn mouse_button_code(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        _ => None,
    }
}
