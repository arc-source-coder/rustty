use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use ghostty_vt::{MouseMode, Terminal};
use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, CursorStyle, ElementId, Entity,
    FocusHandle, FocusOutEvent, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels,
    Point, Render, ScrollDelta, ScrollWheelEvent, Styled, Subscription, Window, div, px,
};
use terminal::TerminalSession;
use ui::scrollbar::ScrollbarState;

use crate::terminal_element::{CellMetrics, TerminalElement};

/// `TerminalElement` writes the surface bounds during prepaint; `TerminalView` reads them
/// each frame for mouse-coordinate conversion and to sync the scrollbar snapshot.
type SurfaceBoundsCell = Rc<Cell<Option<Bounds<Pixels>>>>;

/// Controls where mouse events go for the duration of a left-button drag gesture.
/// Determined once on mouse-down and held fixed until mouse-up to prevent
/// mode drift if modifiers change mid-gesture.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GestureTarget {
    /// Events forwarded to the PTY (mouse reporting mode is active).
    Pty,
    /// Events handled as host-side selection. rectangular = Alt was held at gesture start.
    HostSelect { rectangular: bool },
}

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
    /// Surface bounds written by `TerminalElement::prepaint` each frame.
    /// Used to convert window-space mouse positions to element-local positions
    /// and to sync the scrollbar geometry snapshot.
    surface_bounds: SurfaceBoundsCell,
    /// The viewport cell where a left-drag selection started.
    /// None after double/triple-click (those complete immediately).
    drag_anchor: Option<(u16, u16)>,
    /// Gesture routing, set on left mouse-down, cleared on mouse-up and focus-out.
    gesture_target: Option<GestureTarget>,
    /// Overlay scrollbar entity. Handles its own animation and input.
    scrollbar: Entity<ScrollbarState>,
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

        let scrollbar = cx.new(|_cx| ScrollbarState::new(session.clone()));

        Self {
            session,
            terminal,
            element_id,
            focus_handle,
            cell_metrics: None,
            surface_bounds: Rc::new(Cell::new(None)),
            drag_anchor: None,
            gesture_target: None,
            scrollbar,
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
        self.drag_anchor = None;
        self.gesture_target = None;
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
        let col = (x_px / f32::from(metrics.cell_width)).floor() as u16;
        let row = (y_px / f32::from(metrics.line_height)).floor() as u16;
        Some((
            col.min(grid.cols.saturating_sub(1)),
            row.min(grid.rows.saturating_sub(1)),
        ))
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

        // PTY consumes the gesture when mouse reporting is active AND Shift is not held.
        // Shift always forces host-side selection override.
        let mouse_reporting = self
            .terminal
            .lock()
            .expect("terminal mutex poisoned")
            .input_opts()
            .mouse_event
            != MouseMode::None;

        let target = if mouse_reporting && !event.modifiers.shift {
            GestureTarget::Pty
        } else {
            // rectangular is captured at gesture start so Alt cannot flip mid-drag.
            GestureTarget::HostSelect {
                rectangular: event.modifiers.alt,
            }
        };
        self.gesture_target = Some(target);

        match target {
            GestureTarget::Pty => {
                self.session.read(cx).send_mouse_event(
                    0,
                    0,
                    event.modifiers.shift,
                    event.modifiers.alt,
                    event.modifiers.control,
                    x,
                    y,
                );
            }
            GestureTarget::HostSelect { .. } => {
                match self.session.read(cx).handle_mouse_down(
                    (x, y),
                    event.click_count as u8,
                    &event.modifiers,
                ) {
                    None => {}
                    Some(anchor) => {
                        self.drag_anchor = anchor;
                        cx.notify();
                    }
                }
            }
        }
    }

    fn handle_right_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let mouse_reporting = self
            .terminal
            .lock()
            .expect("terminal mutex poisoned")
            .input_opts()
            .mouse_event
            != MouseMode::None;

        if mouse_reporting && !event.modifiers.shift {
            let Some((x, y)) = self.pixel_to_cell(event.position, window, cx) else {
                return;
            };
            self.session.read(cx).send_mouse_event(
                2,
                0,
                event.modifiers.shift,
                event.modifiers.alt,
                event.modifiers.control,
                x,
                y,
            );
        }
        // Otherwise: wait for right mouse-up to copy+clear.
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
        self.session.read(cx).send_mouse_event(
            1,
            0,
            event.modifiers.shift,
            event.modifiers.alt,
            event.modifiers.control,
            x,
            y,
        );
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

        if let Some(GestureTarget::Pty) = self.gesture_target {
            if let Some((x, y)) = self.pixel_to_cell(event.position, window, cx) {
                self.session.read(cx).send_mouse_event(
                    0,
                    1,
                    event.modifiers.shift,
                    event.modifiers.alt,
                    event.modifiers.control,
                    x,
                    y,
                );
            }
        }
        // HostSelect: selection already committed incrementally via mouse_move.
        self.drag_anchor = None;
        self.gesture_target = None;
    }

    fn handle_right_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mouse_reporting = self
            .terminal
            .lock()
            .expect("terminal mutex poisoned")
            .input_opts()
            .mouse_event
            != MouseMode::None;

        if mouse_reporting && !event.modifiers.shift {
            if let Some((x, y)) = self.pixel_to_cell(event.position, window, cx) {
                self.session.read(cx).send_mouse_event(
                    2,
                    1,
                    event.modifiers.shift,
                    event.modifiers.alt,
                    event.modifiers.control,
                    x,
                    y,
                );
            }
            return;
        }
        // Host-side right-click: copy selection to clipboard, then clear it.
        if let Some(text) = self.session.read(cx).copy_selection() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        self.session.read(cx).clear_selection();
        cx.notify();
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
        self.session.read(cx).send_mouse_event(
            1,
            1,
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

        match self.gesture_target {
            Some(GestureTarget::HostSelect { rectangular }) => {
                if let Some((ax, ay)) = self.drag_anchor {
                    // rectangular was captured at gesture start; ignore current modifiers.
                    self.session.read(cx).set_selection(
                        (ax, ay as u32),
                        (x, y as u32),
                        rectangular,
                    );
                    cx.notify();
                }
            }
            _ => {
                // PTY gesture or hover — forward to PTY (no-op when reporting off)
                self.session.read(cx).send_mouse_event(
                    button,
                    2,
                    event.modifiers.shift,
                    event.modifiers.alt,
                    event.modifiers.control,
                    x,
                    y,
                );
            }
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
            self.session.read(cx).scroll_viewport(delta);
            self.scrollbar.update(cx, |s, cx| s.on_scroll(window, cx));
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
        let key = keystroke.key.to_lowercase();
        let mods = &keystroke.modifiers;
        let session = self.session.read(cx);

        let scroll_action = match key.as_str() {
            "pageup" | "page_up" if mods.shift || !session.is_alternate_screen() => {
                let page = session.current_size().rows.saturating_sub(1).max(1) as i32;
                Some(ScrollAction::Delta(-page))
            }
            "pagedown" | "page_down" if mods.shift || !session.is_alternate_screen() => {
                let page = session.current_size().rows.saturating_sub(1).max(1) as i32;
                Some(ScrollAction::Delta(page))
            }
            "home" if mods.shift => Some(ScrollAction::ToTop),
            "end" if mods.shift => Some(ScrollAction::ToBottom),
            _ => None,
        };

        if let Some(action) = scroll_action {
            match action {
                ScrollAction::Delta(delta) => session.scroll_viewport(delta),
                ScrollAction::ToTop => session.scroll_to_top(),
                ScrollAction::ToBottom => session.scroll_to_bottom(),
            }
            self.scrollbar.update(cx, |s, cx| s.on_scroll(window, cx));
            cx.notify();
            return true;
        }
        false
    }

    /// Check if there's an active selection in the terminal.
    fn has_selection(&self, cx: &Context<Self>) -> bool {
        self.session.read(cx).copy_selection().is_some()
    }
}

/// Scroll actions that can be triggered by keyboard shortcuts.
enum ScrollAction {
    Delta(i32),
    ToTop,
    ToBottom,
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
            Rc::clone(&self.surface_bounds),
        );

        // Sync the scrollbar snapshot. `surface_bounds` was written by `TerminalElement`
        // during the previous prepaint; it lags one frame on first render but is always
        // fresh once the element has painted. `ScrollbarElement::compute_layout` receives
        // the live bounds from its own prepaint, so geometry is never stale.
        // `last_scrollbar_info` was captured under the render mutex in prepaint
        if self.surface_bounds.get().is_some() {
            let info = self.session.read(cx).last_scrollbar_info();
            self.scrollbar
                .update(cx, |state, _cx| state.sync_snapshot(info));
        }

        div()
            .size_full()
            .cursor(CursorStyle::IBeam)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.to_lowercase();
                let mods = &event.keystroke.modifiers;

                // Intercept copy: Ctrl+C (Windows, if selection exists) or Ctrl+Shift+C (all platforms)
                let is_copy = (mods.control && mods.shift && key == "c")
                    || (cfg!(target_os = "windows")
                        && mods.control
                        && !mods.shift
                        && key == "c"
                        && this.has_selection(cx));
                if is_copy {
                    this.handle_copy(window, cx);
                    return;
                }

                // Intercept paste: Ctrl+V (Windows) or Ctrl+Shift+V (Linux) or Cmd+V (macOS)
                let is_paste = (mods.control && key == "v")
                    || (mods.control && mods.shift && key == "v")
                    || (mods.platform && key == "v");
                if is_paste {
                    this.handle_paste(window, cx);
                    return;
                }

                // Intercept scroll keys
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
