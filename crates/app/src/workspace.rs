use std::collections::HashMap;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, Styled, Window, div, prelude::FluentBuilder,
};
use renderer::TerminalView;
use terminal::{RenderConfig, TerminalSession};
use ui::title_bar::TitleBar;

use crate::profile_registry::ProfileRegistry;
use crate::types::TabId;

/// Content hosted by a tab.
pub enum TabContent {
    Terminal(Entity<TerminalView>),
}

/// A single tab's data. Thin wrapper to allow future fields
/// (pinned state, custom icon, etc.) without migrating callers.
pub struct TabEntry {
    pub content: TabContent,
}

/// Root entity for a window. Owns the tab list, tab data, and
/// shared references to app-global state.
pub struct Workspace {
    tabs: Vec<TabId>,
    tab_entries: HashMap<TabId, TabEntry>,
    active_tab: TabId,
    titlebar: Entity<TitleBar>,
    profiles: Entity<ProfileRegistry>,
    render_config: Entity<RenderConfig>,
}

impl Workspace {
    /// Create a new workspace with a single tab running the default profile.
    pub fn new(
        profiles: Entity<ProfileRegistry>,
        render_config: Entity<RenderConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let titlebar = cx.new(|cx| TitleBar::new(cx));

        let default_profile = profiles.read(cx).default_profile();
        let spawn_config = default_profile.spawn_config.clone();

        let session = cx.new(|cx| TerminalSession::new(spawn_config, render_config.clone(), cx));
        let view = cx.new(|cx| TerminalView::new(session, window, cx));

        let tab_id = TabId::new();
        let mut tab_entries = HashMap::new();
        tab_entries.insert(
            tab_id,
            TabEntry {
                content: TabContent::Terminal(view),
            },
        );

        Self {
            tabs: vec![tab_id],
            tab_entries,
            active_tab: tab_id,
            titlebar,
            profiles,
            render_config,
        }
    }

    /// Render the active tab's content as an `AnyElement`.
    fn render_active_content(&self) -> Option<AnyElement> {
        let entry = self.tab_entries.get(&self.active_tab)?;
        Some(match &entry.content {
            TabContent::Terminal(view) => view.clone().into_any_element(),
        })
    }

    pub fn active_terminal_focus_handle(&self, cx: &App) -> FocusHandle {
        let entry = self
            .tab_entries
            .get(&self.active_tab)
            .expect("active tab exists");
        match &entry.content {
            TabContent::Terminal(view) => view.read(cx).focus_handle(cx),
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_content = self.render_active_content();
        window.focus(&self.active_terminal_focus_handle(cx), cx);
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.titlebar.clone().into_any_element())
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .when_some(active_content, |el, content| el.child(content)),
            )
    }
}
