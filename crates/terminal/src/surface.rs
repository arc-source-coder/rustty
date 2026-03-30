use std::sync::{Arc, Mutex, MutexGuard};

use ghostty_vt::{MouseMode, Terminal};
use gpui::{Modifiers, ScrollDelta};

use crate::input::{ENCODE_BUF_SIZE, encode_mouse_event};
use crate::types::{IoMsg, IoThreadNotify, ScrollOp};

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
    drag_anchor: Option<(u16, u16)>,
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
        terminal: &Arc<Mutex<Terminal>>,
        io_notify: &Arc<IoThreadNotify>,
        pos: (u16, u16),
        click_count: u8,
        mods: &Modifiers,
    ) -> bool {
        let mouse_reporting = {
            let term = lock_terminal(terminal);
            term.input_opts().mouse_event != MouseMode::None
        };

        let target = if mouse_reporting && !mods.shift {
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
                Self::send_mouse_event(terminal, io_notify, 0, 0, mods, pos)
            }
            GestureTarget::HostSelect { .. } => {
                self.left_click_count = click_count;
                let mut term = lock_terminal(terminal);
                match click_count {
                    1 => {
                        let had_selection = term.selection_text().is_some();
                        term.clear_selection();
                        self.drag_anchor = Some(pos);
                        had_selection
                    }
                    2 => {
                        let selected = term.select_word_at(pos.0, pos.1 as u32);
                        self.drag_anchor = Some(pos);
                        selected
                    }
                    3 => {
                        let selected = if mods.control || mods.platform {
                            term.select_output_at(pos.0, pos.1 as u32)
                        } else {
                            term.select_line_at(pos.0, pos.1 as u32)
                        };
                        self.drag_anchor = Some(pos);
                        selected
                    }
                    _ => false,
                }
            }
        }
    }

    pub fn handle_right_mouse_down(
        &mut self,
        terminal: &Arc<Mutex<Terminal>>,
        io_notify: &Arc<IoThreadNotify>,
        pos: (u16, u16),
        mods: &Modifiers,
    ) -> bool {
        let mouse_reporting = {
            let term = lock_terminal(terminal);
            term.input_opts().mouse_event != MouseMode::None
        };
        if mouse_reporting && !mods.shift {
            return Self::send_mouse_event(terminal, io_notify, 2, 0, mods, pos);
        }
        false
    }

    pub fn handle_middle_mouse_down(
        &mut self,
        terminal: &Arc<Mutex<Terminal>>,
        io_notify: &Arc<IoThreadNotify>,
        pos: (u16, u16),
        mods: &Modifiers,
    ) -> bool {
        Self::send_mouse_event(terminal, io_notify, 1, 0, mods, pos)
    }

    pub fn handle_left_mouse_up(
        &mut self,
        terminal: &Arc<Mutex<Terminal>>,
        io_notify: &Arc<IoThreadNotify>,
        pos: Option<(u16, u16)>,
        mods: &Modifiers,
    ) -> bool {
        let consumed = match (self.gesture_target, pos) {
            (Some(GestureTarget::Pty), Some(pos)) => {
                Self::send_mouse_event(terminal, io_notify, 0, 1, mods, pos)
            }
            _ => false,
        };
        self.drag_anchor = None;
        self.left_click_count = 0;
        self.gesture_target = None;
        consumed
    }

    pub fn handle_right_mouse_up(
        &mut self,
        terminal: &Arc<Mutex<Terminal>>,
        io_notify: &Arc<IoThreadNotify>,
        pos: Option<(u16, u16)>,
        mods: &Modifiers,
        emit: &mut dyn FnMut(AppAction),
    ) -> bool {
        let mouse_reporting = {
            let term = lock_terminal(terminal);
            term.input_opts().mouse_event != MouseMode::None
        };

        if mouse_reporting && !mods.shift {
            return pos.is_some_and(|p| Self::send_mouse_event(terminal, io_notify, 2, 1, mods, p));
        }

        let copied = {
            let mut term = lock_terminal(terminal);
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
        terminal: &Arc<Mutex<Terminal>>,
        io_notify: &Arc<IoThreadNotify>,
        pos: (u16, u16),
        mods: &Modifiers,
    ) -> bool {
        Self::send_mouse_event(terminal, io_notify, 1, 1, mods, pos)
    }

    pub fn handle_mouse_move(
        &mut self,
        terminal: &Arc<Mutex<Terminal>>,
        io_notify: &Arc<IoThreadNotify>,
        pos: (u16, u16),
        pressed_button: u8,
        mods: &Modifiers,
    ) -> bool {
        match self.gesture_target {
            Some(GestureTarget::HostSelect { rectangular }) => {
                if let Some((ax, ay)) = self.drag_anchor {
                    let mut term = lock_terminal(terminal);
                    match (rectangular, self.left_click_count) {
                        (false, 2) => {
                            let _ = term.select_word_drag(ax, ay as u32, pos.0, pos.1 as u32);
                        }
                        (false, 3) => {
                            let _ = term.select_line_drag(ax, ay as u32, pos.0, pos.1 as u32);
                        }
                        _ => {
                            term.set_selection(ax, ay as u32, pos.0, pos.1 as u32, rectangular);
                        }
                    }
                    return true;
                }
                false
            }
            _ => Self::send_mouse_event(terminal, io_notify, pressed_button, 2, mods, pos),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn handle_scroll_wheel(
        &mut self,
        terminal: &Arc<Mutex<Terminal>>,
        io_notify: &Arc<IoThreadNotify>,
        pos: (u16, u16),
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

        let context = {
            let mut term = lock_terminal(terminal);
            let opts = term.input_opts();
            let is_alternate_screen = term.is_alternate_screen();
            let mouse_alternate_scroll = term.mouse_alternate_scroll_enabled();
            let mut selection_cleared = false;

            if (opts.mouse_event != MouseMode::None
                || (is_alternate_screen && mouse_alternate_scroll))
                && term.selection_text().is_some()
            {
                term.clear_selection();
                selection_cleared = true;
            }

            ScrollContext {
                opts,
                is_alternate_screen,
                mouse_alternate_scroll,
                selection_cleared,
            }
        };

        let mut notify = context.selection_cleared;
        let button = if scroll_up { 64 } else { 65 };

        if context.opts.mouse_event != MouseMode::None {
            for _ in 0..rows {
                Self::send_mouse_event_with_opts(io_notify, context.opts, button, 0, mods, pos);
            }
            return notify;
        }

        if context.is_alternate_screen && context.mouse_alternate_scroll {
            let seq = if context.opts.cursor_key_application {
                if scroll_up {
                    b"\x1bOA".as_slice()
                } else {
                    b"\x1bOB".as_slice()
                }
            } else if scroll_up {
                b"\x1b[A".as_slice()
            } else {
                b"\x1b[B".as_slice()
            };

            for _ in 0..rows {
                write_small_to_pty(io_notify, seq);
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
        terminal: &Arc<Mutex<Terminal>>,
        io_notify: &Arc<IoThreadNotify>,
        key: &str,
        mods: &Modifiers,
        page_rows: u16,
        emit: &mut dyn FnMut(AppAction),
    ) -> bool {
        let is_alternate_screen = {
            let term = lock_terminal(terminal);
            term.is_alternate_screen()
        };
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
        terminal: &Arc<Mutex<Terminal>>,
        io_notify: &Arc<IoThreadNotify>,
        button: u8,
        action: u8,
        mods: &Modifiers,
        pos: (u16, u16),
    ) -> bool {
        let opts = {
            let term = lock_terminal(terminal);
            term.input_opts()
        };
        Self::send_mouse_event_with_opts(io_notify, opts, button, action, mods, pos)
    }

    fn send_mouse_event_with_opts(
        io_notify: &Arc<IoThreadNotify>,
        opts: ghostty_vt::InputOpts,
        button: u8,
        action: u8,
        mods: &Modifiers,
        pos: (u16, u16),
    ) -> bool {
        let mut buf = [0u8; ENCODE_BUF_SIZE];
        if let Some(bytes) = encode_mouse_event(
            opts,
            button,
            action,
            mods.shift,
            mods.alt,
            mods.control,
            pos.0,
            pos.1,
            &mut buf,
        ) {
            write_small_to_pty(io_notify, bytes);
            true
        } else {
            false
        }
    }
}

struct ScrollContext {
    opts: ghostty_vt::InputOpts,
    is_alternate_screen: bool,
    mouse_alternate_scroll: bool,
    selection_cleared: bool,
}

fn lock_terminal(terminal: &Arc<Mutex<Terminal>>) -> MutexGuard<'_, Terminal> {
    terminal.lock().expect("terminal mutex poisoned")
}

fn write_small_to_pty(io_notify: &Arc<IoThreadNotify>, data: &[u8]) {
    io_notify.try_send_input_small(data);
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
