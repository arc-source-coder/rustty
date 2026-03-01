use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use ghostty_vt::Terminal;
use gpui::{
    App, Context, ElementId, Entity, FocusHandle, FocusOutEvent, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent, Styled, Subscription,
    Window, div, px,
};
use terminal::TerminalSession;

use crate::terminal_element::{CellMetrics, TerminalElement};

/// Shared between `TerminalView` and `TerminalElement`. `TerminalElement` writes
/// the element's window-relative origin during prepaint; `TerminalView` reads it
/// in mouse handlers to convert window-space positions to element-local space.
type SharedOrigin = Rc<Cell<Option<Point<Pixels>>>>;

/// GPUI entity that owns the renderer's view of a terminal session.
///
/// Implements `Render` to produce a `TerminalElement` for each frame.
/// Owns the `FocusHandle` so the terminal can receive keyboard input.
pub struct TerminalView {
    session: Entity<TerminalSession>,
    terminal: Arc<Mutex<Terminal>>,
    element_id: ElementId,
    focus_handle: FocusHandle,
    /// Cached cell metrics. None until first render; always Some during mouse events
    /// (render always precedes input). Re-measured lazily if None.
    cell_metrics: Option<CellMetrics>,
    /// The element's window-relative origin, written by `TerminalElement::prepaint`.
    /// Used to convert window-space mouse positions to element-local positions.
    element_origin: SharedOrigin,
    /// Focus event subscriptions. Must be stored to keep the listeners active.
    _subscriptions: Vec<Subscription>,
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

        let (terminal, element_id) = {
            let s = session.read(cx);
            let terminal = s.terminal_mutex().clone();
            let element_id = ElementId::Name(format!("terminal-{}", s.id.as_u64()).into());
            (terminal, element_id)
        };
        Self {
            session,
            terminal,
            element_id,
            focus_handle,
            cell_metrics: None,
            element_origin: Rc::new(Cell::new(None)),
            _subscriptions: vec![focus_in_sub, focus_out_sub],
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
        self.session.read(cx).send_focus_change(false);
    }

    pub fn session(&self) -> &Entity<TerminalSession> {
        &self.session
    }

    fn pixel_to_cell(
        &mut self,
        position: Point<Pixels>,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<(u16, u16)> {
        // Lazily populate metrics. After first render, this is always Some.
        let metrics = self.cell_metrics.get_or_insert_with(|| {
            let render_config = self.session.read(cx).render_config().read(cx).clone();
            TerminalElement::measure_cell(
                render_config.font_family.clone(),
                px(render_config.font_size),
                window,
            )
        });

        // `event.position` is window-relative; subtract the element's origin to
        // get a position local to the terminal surface (i.e. excluding the title bar).
        let origin = self.element_origin.get().unwrap_or_default();
        let x_px = f32::from(position.x) - f32::from(origin.x);
        let y_px = f32::from(position.y) - f32::from(origin.y);
        if x_px < 0.0 || y_px < 0.0 {
            return None;
        }

        let grid = self.session.read(cx).current_size();
        let col = (x_px / f32::from(metrics.cell_width)).floor() as u16;
        let row = (y_px / f32::from(metrics.line_height)).floor() as u16;
        Some((
            col.min(grid.cols.saturating_sub(1)),
            row.min(grid.rows.saturating_sub(1)),
        ))
    }

    fn handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);

        let Some(button) = mouse_button_code(event.button) else {
            return;
        };
        let Some((x, y)) = self.pixel_to_cell(event.position, window, cx) else {
            return;
        };

        // action 0 = press
        self.session.read(cx).send_mouse_event(
            button,
            0, // press
            event.modifiers.shift,
            event.modifiers.alt,
            event.modifiers.control,
            x,
            y,
        );
    }

    fn handle_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(button) = mouse_button_code(event.button) else {
            return;
        };
        let Some((x, y)) = self.pixel_to_cell(event.position, window, cx) else {
            return;
        };

        // action 1 = release
        self.session.read(cx).send_mouse_event(
            button,
            1, // release
            event.modifiers.shift,
            event.modifiers.alt,
            event.modifiers.control,
            x,
            y,
        );
    }

    fn handle_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Skip if mouse is outside the terminal bounds (negative coords after
        // subtracting element origin). This prevents spurious events during
        // window drag operations or when cursor leaves the terminal area.
        let Some((x, y)) = self.pixel_to_cell(event.position, window, cx) else {
            return;
        };

        // Determine button code: use pressed button if any, otherwise 3 (no button).
        // Button 3 is used for hover motion in Button/Any mouse modes.
        // The encoder will only produce output when mouse mode supports motion.
        let button = event
            .pressed_button
            .and_then(mouse_button_code)
            .unwrap_or(3);

        // action 2 = motion
        self.session.read(cx).send_mouse_event(
            button,
            2, // motion
            event.modifiers.shift,
            event.modifiers.alt,
            event.modifiers.control,
            x,
            y,
        );
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

        let cell_h = self
            .cell_metrics
            .map(|m| f32::from(m.line_height))
            .unwrap_or(16.0);
        let scroll_up = match event.delta {
            ScrollDelta::Lines(d) => d.y > 0.0,
            ScrollDelta::Pixels(d) => f32::from(d.y) > 0.0,
        };
        let lines = match event.delta {
            ScrollDelta::Lines(d) => d.y.abs().ceil() as usize,
            ScrollDelta::Pixels(d) => (f32::from(d.y).abs() / cell_h).ceil() as usize,
        }
        .max(1);

        // 64 = scroll up, 65 = scroll down
        let button: u8 = if scroll_up { 64 } else { 65 };

        // Try mouse reporting first. send_mouse_event returns false when
        // mouse reporting is disabled — fall back to viewport scrolling.
        // First event determines if mouse reporting is active; subsequent
        // events in the same gesture are sent the same way.
        let consumed = self.session.read(cx).send_mouse_event(
            button,
            0,
            event.modifiers.shift,
            event.modifiers.alt,
            event.modifiers.control,
            x,
            y,
        );

        if consumed {
            // Mouse reporting active — send one event per remaining line.
            for _ in 1..lines {
                self.session.read(cx).send_mouse_event(
                    button,
                    0,
                    event.modifiers.shift,
                    event.modifiers.alt,
                    event.modifiers.control,
                    x,
                    y,
                );
            }
        } else {
            // No mouse reporting — scroll the viewport.
            // Negative = scroll up (towards history), positive = scroll down.
            let delta = if scroll_up {
                -(lines as i32)
            } else {
                lines as i32
            };
            self.terminal
                .lock()
                .expect("terminal mutex poisoned")
                .scroll_viewport(delta);
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
            self.terminal.clone(),
            self.element_id.clone(),
            Rc::clone(&self.element_origin),
        );

        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.to_lowercase();
                let mods = &event.keystroke.modifiers;

                // Intercept paste: Ctrl+V (Windows) or Ctrl+Shift+V (Linux) or Cmd+V (macOS)
                let is_paste = (mods.control && key == "v")
                    || (mods.control && mods.shift && key == "v")
                    || (mods.platform && key == "v");
                if is_paste {
                    this.handle_paste(window, cx);
                    return;
                }

                this.session
                    .read(cx)
                    .send_key_event(&event.keystroke, event.is_held);
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::handle_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::handle_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::handle_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::handle_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::handle_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::handle_mouse_up))
            .on_mouse_move(cx.listener(Self::handle_mouse_move))
            .on_scroll_wheel(cx.listener(Self::handle_scroll_wheel))
            .child(terminal_element)
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
