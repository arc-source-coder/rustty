use std::sync::Arc;

use ghostty::Terminal;
use gpui::{Modifiers, ScrollDelta};
use zconpty::{KeyEvent, MouseAction, MouseButton, MouseEvent, MousePosition, key_from_w3c};

use crate::input::pack_mouse_mods;
use crate::types::{IoInput, IoMsg, IoThreadNotify, ScrollOp};

#[derive(Debug, Clone)]
pub enum AppAction {
    WriteClipboard(String),
    ViewportScrolled,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GestureTarget {
    Pty,
    HostSelect { rectangular: bool },
}

pub struct TerminalSurface {
    drag_anchor: Option<MousePosition>,
    left_click_count: u8,
    gesture_target: Option<GestureTarget>,
    pending_scroll_y: f32,
}

impl TerminalSurface {
    pub fn new() -> Self {
        Self {
            drag_anchor: None,
            left_click_count: 0,
            gesture_target: None,
            pending_scroll_y: 0.0,
        }
    }

    pub fn focus_out(&mut self) {
        self.drag_anchor = None;
        self.left_click_count = 0;
        self.gesture_target = None;
    }

    pub fn handle_left_mouse_down(
        &mut self,
        term: &Arc<Terminal>,
        io_notify: &Arc<IoThreadNotify>,
        position: MousePosition,
        click_count: u8,
        mods: &Modifiers,
    ) -> bool {
        let target = if term.is_mouse_reporting() && !mods.shift {
            GestureTarget::Pty
        } else {
            GestureTarget::HostSelect {
                rectangular: mods.alt,
            }
        };
        self.gesture_target = Some(target);
        let click_count = normalize_click_count(click_count);

        match target {
            GestureTarget::Pty => {
                self.drag_anchor = None;
                self.left_click_count = 0;
                Self::send_mouse_event(
                    term,
                    io_notify,
                    MouseButton::Left,
                    MouseAction::Press,
                    mods,
                    position,
                )
            }
            GestureTarget::HostSelect { .. } => {
                self.left_click_count = click_count;
                match click_count {
                    1 => {
                        let had_selection = term.selection_text().is_some();
                        term.clear_selection();
                        self.drag_anchor = Some(position);
                        had_selection
                    }
                    2 => {
                        let selected = term.select_word_at(position.x as u16, position.y as u32);
                        self.drag_anchor = Some(position);
                        selected
                    }
                    3 => {
                        let selected = if mods.control || mods.platform {
                            term.select_output_at(position.x as u16, position.y as u32)
                        } else {
                            term.select_line_at(position.x as u16, position.y as u32)
                        };
                        self.drag_anchor = Some(position);
                        selected
                    }
                    _ => false,
                }
            }
        }
    }

    pub fn handle_right_mouse_down(
        &mut self,
        terminal: &Arc<Terminal>,
        io_notify: &Arc<IoThreadNotify>,
        position: MousePosition,
        mods: &Modifiers,
    ) -> bool {
        if terminal.is_mouse_reporting() && !mods.shift {
            return Self::send_mouse_event(
                terminal,
                io_notify,
                MouseButton::Right,
                MouseAction::Press,
                mods,
                position,
            );
        }
        false
    }

    pub fn handle_middle_mouse_down(
        &mut self,
        terminal: &Arc<Terminal>,
        io_notify: &Arc<IoThreadNotify>,
        position: MousePosition,
        mods: &Modifiers,
    ) -> bool {
        Self::send_mouse_event(
            terminal,
            io_notify,
            MouseButton::Middle,
            MouseAction::Press,
            mods,
            position,
        )
    }

    pub fn handle_left_mouse_up(
        &mut self,
        terminal: &Arc<Terminal>,
        io_notify: &Arc<IoThreadNotify>,
        position: Option<MousePosition>,
        mods: &Modifiers,
    ) -> bool {
        let consumed = match (self.gesture_target, position) {
            (Some(GestureTarget::Pty), Some(pos)) => Self::send_mouse_event(
                terminal,
                io_notify,
                MouseButton::Left,
                MouseAction::Release,
                mods,
                pos,
            ),
            _ => false,
        };
        self.drag_anchor = None;
        self.left_click_count = 0;
        self.gesture_target = None;
        consumed
    }

    pub fn handle_right_mouse_up(
        &mut self,
        terminal: &Arc<Terminal>,
        io_notify: &Arc<IoThreadNotify>,
        position: Option<MousePosition>,
        mods: &Modifiers,
        emit: &mut dyn FnMut(AppAction),
    ) -> bool {
        if terminal.is_mouse_reporting() && !mods.shift {
            return position.is_some_and(|pos| {
                Self::send_mouse_event(
                    terminal,
                    io_notify,
                    MouseButton::Right,
                    MouseAction::Release,
                    mods,
                    pos,
                )
            });
        }

        // Locks internally.
        let copied = {
            let term = terminal;
            let text = term.selection_text().map(|s| s.as_str().to_owned());
            if text.is_some() {
                term.clear_selection();
            }
            text
        };

        if let Some(text) = copied {
            emit(AppAction::WriteClipboard(text));
            return true;
        }

        false
    }

    pub fn handle_middle_mouse_up(
        &mut self,
        terminal: &Arc<Terminal>,
        io_notify: &Arc<IoThreadNotify>,
        position: MousePosition,
        mods: &Modifiers,
    ) -> bool {
        Self::send_mouse_event(
            terminal,
            io_notify,
            MouseButton::Middle,
            MouseAction::Release,
            mods,
            position,
        )
    }

    pub fn handle_mouse_move(
        &mut self,
        term: &Arc<Terminal>,
        io_notify: &Arc<IoThreadNotify>,
        position: MousePosition,
        pressed_button: MouseButton,
        mods: &Modifiers,
    ) -> bool {
        match self.gesture_target {
            Some(GestureTarget::HostSelect { rectangular }) => {
                // Locks internally.
                if let Some(pos) = self.drag_anchor {
                    match (rectangular, self.left_click_count) {
                        (false, 2) => {
                            let _ = term.select_word_drag(
                                pos.x as u16,
                                pos.y,
                                position.x as u16,
                                position.y,
                            );
                        }
                        (false, 3) => {
                            let _ = term.select_line_drag(
                                pos.x as u16,
                                pos.y,
                                position.x as u16,
                                position.y,
                            );
                        }
                        _ => {
                            term.set_selection(
                                pos.x as u16,
                                pos.y,
                                position.x as u16,
                                position.y,
                                rectangular,
                            );
                        }
                    }
                    return true;
                }
                false
            }
            _ => Self::send_mouse_event(
                term,
                io_notify,
                pressed_button,
                MouseAction::Move,
                mods,
                position,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn handle_scroll_wheel(
        &mut self,
        terminal: &Arc<Terminal>,
        io_notify: &Arc<IoThreadNotify>,
        position: MousePosition,
        delta: ScrollDelta,
        cell_height: f32,
        mods: &Modifiers,
        emit: &mut dyn FnMut(AppAction),
    ) -> bool {
        let rows = match delta {
            ScrollDelta::Lines(lines) => lines.y.abs().ceil() as i32,
            ScrollDelta::Pixels(pixels) => {
                let cell_height = cell_height.max(1.0);
                let total = self.pending_scroll_y + f32::from(pixels.y);
                let rows = (total.abs() / cell_height).floor() as i32;
                if rows == 0 {
                    self.pending_scroll_y = total;
                    return false;
                }

                let consumed = rows as f32 * cell_height * total.signum();
                self.pending_scroll_y = total - consumed;
                rows
            }
        }
        .max(1);

        let scroll_up = match delta {
            ScrollDelta::Lines(lines) => lines.y > 0.0,
            ScrollDelta::Pixels(pixels) => f32::from(pixels.y) > 0.0,
        };

        // Locks internally.
        let is_mouse_reporting = terminal.is_mouse_reporting();
        let is_alternate_screen = terminal.is_alternate_screen();
        let mouse_alternate_scroll = terminal.mouse_alternate_scroll_enabled();

        let mut selection_cleared = false;

        if (is_mouse_reporting || (is_alternate_screen && mouse_alternate_scroll))
            && terminal.selection_text().is_some()
        {
            terminal.clear_selection();
            selection_cleared = true;
        }

        let mut notify = selection_cleared;
        let button = if scroll_up {
            MouseButton::WheelUp
        } else {
            MouseButton::WheelDown
        };

        if is_mouse_reporting {
            for _ in 0..rows {
                Self::send_mouse_event(
                    terminal,
                    io_notify,
                    button,
                    MouseAction::Press,
                    mods,
                    position,
                );
            }
            return notify;
        }

        if is_alternate_screen && mouse_alternate_scroll {
            let code = if scroll_up {
                key_from_w3c(b"arrow_up").expect("arrow_up should resolve")
            } else {
                key_from_w3c(b"arrow_down").expect("arrow_down should resolve")
            };

            for _ in 0..rows {
                io_notify.send_lossless(IoMsg::Input(IoInput::Key(KeyEvent::press(code))));
            }
            return true;
        }

        queue_scroll(
            io_notify,
            ScrollOp::Delta(if scroll_up { -rows } else { rows }),
        );
        emit(AppAction::ViewportScrolled);
        notify = true;
        notify
    }

    pub fn handle_scroll_key(
        &mut self,
        terminal: &Arc<Terminal>,
        io_notify: &Arc<IoThreadNotify>,
        key: &str,
        mods: &Modifiers,
        page_rows: u16,
        emit: &mut dyn FnMut(AppAction),
    ) -> bool {
        // Locks internally.
        let is_alternate_screen = { terminal.is_alternate_screen() };
        let scroll_op = match key {
            k if (k.eq_ignore_ascii_case("pageup") || k.eq_ignore_ascii_case("page_up"))
                && (mods.shift || !is_alternate_screen) =>
            {
                Some(ScrollOp::Delta(
                    -(page_rows.saturating_sub(1).max(1) as i32),
                ))
            }
            k if (k.eq_ignore_ascii_case("pagedown") || k.eq_ignore_ascii_case("page_down"))
                && (mods.shift || !is_alternate_screen) =>
            {
                Some(ScrollOp::Delta(page_rows.saturating_sub(1).max(1) as i32))
            }
            k if k.eq_ignore_ascii_case("home") && mods.shift => Some(ScrollOp::Top),
            k if k.eq_ignore_ascii_case("end") && mods.shift => Some(ScrollOp::Bottom),
            _ => None,
        };

        if let Some(op) = scroll_op {
            queue_scroll(io_notify, op);
            emit(AppAction::ViewportScrolled);
            return true;
        }

        false
    }

    fn send_mouse_event(
        terminal: &Arc<Terminal>,
        io_notify: &Arc<IoThreadNotify>,
        button: MouseButton,
        action: MouseAction,
        modifiers: &Modifiers,
        position: MousePosition,
    ) -> bool {
        if !terminal.is_mouse_reporting() {
            return false;
        }
        let event = MouseEvent {
            button,
            action,
            mods: pack_mouse_mods(modifiers),
            position,
        };
        io_notify.send_lossless(IoMsg::Input(IoInput::Mouse(event)));

        true
    }
}

fn queue_scroll(io_notify: &Arc<IoThreadNotify>, op: ScrollOp) {
    let _ = io_notify.try_send(IoMsg::Scroll(op));
}

fn normalize_click_count(click_count: u8) -> u8 {
    match click_count {
        2 => 2,
        3 => 3,
        _ => 1,
    }
}
