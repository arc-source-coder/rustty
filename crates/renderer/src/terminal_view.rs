use std::sync::{Arc, Mutex};

use ghostty_vt::Terminal;
use gpui::{App, Context, ElementId, Entity, IntoElement, Render, Window};
use terminal::TerminalSession;

use crate::terminal_element::TerminalElement;

/// GPUI entity that owns the renderer's view of a terminal session.
///
/// Implements `Render` to produce a `TerminalElement` for each frame.
/// Caches the ElementId and Arc<Mutex<Terminal>> at construction time
/// to avoid repeated Entity reads and string formatting in render().
pub struct TerminalView {
    session: Entity<TerminalSession>,
    terminal: Arc<Mutex<Terminal>>,
    element_id: ElementId,
}

impl TerminalView {
    pub fn new(session: Entity<TerminalSession>, cx: &App) -> Self {
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
        }
    }

    pub fn session(&self) -> &Entity<TerminalSession> {
        &self.session
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        TerminalElement::new(
            self.session.clone(),
            self.terminal.clone(),
            self.element_id.clone(),
        )
    }
}
