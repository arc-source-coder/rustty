use std::collections::HashMap;
use std::time::{Duration, Instant};

use config::Config;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, DragMoveEvent, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, ParentElement, PromptLevel, Render, ScrollHandle,
    StatefulInteractiveElement, Styled, Subscription, Window, WindowControlArea, div, px, rgba,
};
use renderer::{TerminalView, TerminalViewEvent};
use ui::title_bar::WindowsWindowControls;

use crate::actions::{
    CloseActiveTab, CloseWindow, DecreaseFontSize, IncreaseFontSize, NewTab, ResetFontSize,
    SelectNextTab, SelectPreviousTab, SelectTab1, SelectTab2, SelectTab3, SelectTab4, SelectTab5,
    SelectTab6, SelectTab7, SelectTab8, SelectTab9,
};
use crate::profile::ProfileIconKind;
use crate::profile_registry::ProfileRegistry;
use crate::tab_strip::{
    ACTIVE_TAB_BOTTOM_OVERLAP, ACTIVE_TAB_SHOULDER_WIDTH, LEFT_DRAG_GUTTER_WIDTH,
    NEW_TAB_BUTTON_SECTION_LEFT_PADDING, NEW_TAB_BUTTON_SECTION_RIGHT_PADDING,
    RIGHT_DRAG_GUTTER_MIN_WIDTH, TAB_CLOSE_DURATION, TAB_DRAG_AUTOSCROLL_EDGE_WIDTH,
    TAB_FALLBACK_WIDTH, TAB_OPEN_DURATION, TAB_OPEN_FAST_DURATION, TAB_REORDER_DURATION,
    TAB_SCROLL_BUTTON_GAP, TAB_SCROLL_DURATION, TabDragLayout, TabDragState, TabScrollDirection,
    TabScrollUpdate, TabVisualStyle, offset_is_effectively_zero, render_new_tab_button,
    render_tab_close_button, render_tab_scroll_button, render_tab_visual, resolve_tab_widths,
    scroll_tab_into_view, tab_animation_progress, tab_drag_autoscroll_offset_x,
    tab_layout_positions, tab_scroll_target_offset, tab_strip_layout_plan, tab_visual_position,
    tab_visual_style, title_bar_metrics, title_bar_palette, update_tab_drag_state,
};
use crate::types::TabId;

const WORKSPACE_KEY_CONTEXT: &str = "Workspace";

macro_rules! define_select_tab_handlers {
    ($(($method:ident, $action:ty, $index:expr)),+ $(,)?) => {
        $(
            fn $method(&mut self, _: &$action, window: &mut Window, cx: &mut Context<Self>) {
                self.select_tab_index($index, window, cx);
            }
        )+
    };
}

/// Content hosted by a tab.
pub enum TabContent {
    Terminal { view: Entity<TerminalView> },
}

/// A single tab's data. Thin wrapper to allow future fields
/// (pinned state, custom icon, etc.) without migrating callers.
pub struct TabEntry {
    pub live_content: Option<TabContent>,
    title_fallback: String,
    profile_icon: ProfileIconKind,
    pub(crate) phase: TabAnimationPhase,
    pub(crate) reorder_animation: Option<TabReorderAnimation>,
    _title_subscription: Subscription,
}

#[derive(Clone, Copy)]
pub(crate) enum TabAnimationPhase {
    Opening { start: Instant, duration: Duration },
    Open,
    Closing { start: Instant, duration: Duration },
}

#[derive(Clone, Copy)]
pub(crate) struct TabReorderAnimation {
    pub(crate) start: Instant,
    pub(crate) offset_x: gpui::Pixels,
}

#[derive(Clone, Copy)]
struct TabScrollAnimation {
    start: Instant,
    from_x: gpui::Pixels,
    to_x: gpui::Pixels,
}

#[derive(Clone, Copy)]
struct TabLayoutAnimation {
    start: Instant,
    duration: Duration,
    from_width: gpui::Pixels,
    to_width: gpui::Pixels,
}

#[derive(Clone)]
struct DraggedTab {
    tab_id: TabId,
}

struct EmptyView;

impl Render for EmptyView {
    fn render(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(0.)).h(px(0.))
    }
}

/// Root entity for a window. Owns the tab list, tab data, and
/// shared references to app-global state.
pub struct Workspace {
    tabs: Vec<TabId>,
    tab_entries: HashMap<TabId, TabEntry>,
    active_tab: TabId,
    profiles: Entity<ProfileRegistry>,
    tab_scroll_handle: ScrollHandle,
    tab_scroll_animation: Option<TabScrollAnimation>,
    tab_layout_animations: HashMap<TabId, TabLayoutAnimation>,
    scroll_active_tab_into_view: bool,
    tab_strip_is_scrollable: bool,
    dragging: Option<TabDragState>,
    tab_drag_layout: Option<TabDragLayout>,
    hovered_tab: Option<TabId>,
}

impl Workspace {
    /// Create a new workspace with a single tab running the default profile.
    pub fn new(
        profiles: Entity<ProfileRegistry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tab_id = TabId::new();
        let mut workspace = Self {
            tabs: vec![tab_id],
            tab_entries: HashMap::new(),
            active_tab: tab_id,
            profiles,
            tab_scroll_handle: ScrollHandle::new(),
            tab_scroll_animation: None,
            tab_layout_animations: HashMap::new(),
            scroll_active_tab_into_view: true,
            tab_strip_is_scrollable: false,
            dragging: None,
            tab_drag_layout: None,
            hovered_tab: None,
        };
        let tab_entry = workspace.build_terminal_tab(TabAnimationPhase::Open, window, cx);
        workspace.tab_entries.insert(tab_id, tab_entry);
        workspace.update_window_title(window, cx);
        workspace
    }

    fn build_terminal_tab(
        &self,
        phase: TabAnimationPhase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TabEntry {
        let default_profile = self.profiles.read(cx).default_profile().clone();
        let spawn_config = default_profile.spawn_config.clone();
        let view = cx.new(|cx| TerminalView::new(spawn_config, Config::default(), window, cx));

        let title_sub = cx.subscribe_in(&view, window, |workspace, view, event, window, cx| {
            if matches!(event, TerminalViewEvent::TitleChanged) {
                if workspace.active_terminal_view() == Some(view) {
                    workspace.update_window_title(window, cx);
                }
                cx.notify();
            }
        });

        TabEntry {
            live_content: Some(TabContent::Terminal { view }),
            title_fallback: default_profile.name.clone(),
            profile_icon: default_profile.profile_icon_kind(),
            phase,
            reorder_animation: None,
            _title_subscription: title_sub,
        }
    }

    /// Render the active tab's content as an `AnyElement`.
    // TODO(renderer-refactor): Audit this
    fn render_active_content(&self) -> Option<AnyElement> {
        let entry = self.tab_entries.get(&self.active_tab)?;
        let content = entry.live_content.as_ref()?;
        Some(match content {
            TabContent::Terminal { view, .. } => view.clone().into_any_element(),
        })
    }

    fn active_terminal_background(&self, cx: &App) -> gpui::Hsla {
        let Some(entry) = self.tab_entries.get(&self.active_tab) else {
            return rgba(0x1e1e2eff).into();
        };

        let Some(content) = entry.live_content.as_ref() else {
            return rgba(0x1e1e2eff).into();
        };

        match content {
            TabContent::Terminal { view } => rgba(view.read(cx).default_background_rgba()).into(),
        }
    }

    fn tab_title(&self, entry: &TabEntry, cx: &App) -> String {
        let title = match entry.live_content.as_ref() {
            Some(TabContent::Terminal { view }) => view.read(cx).title().map(str::to_owned),
            None => None,
        };

        title.unwrap_or_else(|| entry.title_fallback.clone())
    }

    fn update_window_title(&self, window: &mut Window, cx: &App) {
        if let Some(entry) = self.tab_entries.get(&self.active_tab) {
            window.set_window_title(&self.tab_title(entry, cx));
        }
    }

    fn active_terminal_view(&self) -> Option<&Entity<TerminalView>> {
        let entry = self.tab_entries.get(&self.active_tab)?;
        match entry.live_content.as_ref()? {
            TabContent::Terminal { view } => Some(view),
        }
    }

    fn open_tabs(&self) -> Vec<TabId> {
        self.tabs
            .iter()
            .copied()
            .filter(|tab_id| self.tab_is_selectable(*tab_id))
            .collect()
    }

    fn open_tab_count(&self) -> usize {
        self.tabs
            .iter()
            .copied()
            .filter(|tab_id| self.tab_is_selectable(*tab_id))
            .count()
    }

    fn tab_is_selectable(&self, tab_id: TabId) -> bool {
        self.tab_entries
            .get(&tab_id)
            .map(|entry| !matches!(entry.phase, TabAnimationPhase::Closing { .. }))
            .unwrap_or(false)
    }

    fn handle_tab_drag_move(
        &mut self,
        event: &DragMoveEvent<DraggedTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.event.pressed_button != Some(MouseButton::Left) {
            self.cancel_tab_drag();
            cx.stop_active_drag(window);
            cx.notify();
            return;
        }

        let dragged_tab_id = event.drag(cx).tab_id;
        let mut viewport_bounds = event.bounds;
        if self.tab_strip_is_scrollable {
            let scroll_bounds = self.tab_scroll_handle.bounds();
            if scroll_bounds.size.width > px(0.) {
                viewport_bounds = scroll_bounds;
            }
        }

        let pointer_viewport_x = (event.event.position.x - viewport_bounds.left())
            .clamp(px(0.), viewport_bounds.size.width);
        let scroll_offset_x = if self.tab_strip_is_scrollable {
            self.tab_scroll_handle.offset().x
        } else {
            px(0.)
        };
        let pointer_content_x = event.event.position.x - viewport_bounds.left() - scroll_offset_x;

        let position_changed = self.update_tab_drag_position(dragged_tab_id, pointer_content_x);
        let autoscroll_changed = self.update_tab_drag_autoscroll(pointer_viewport_x);
        if position_changed || autoscroll_changed {
            cx.notify();
        }
    }

    fn update_tab_drag_position(
        &mut self,
        dragged_tab_id: TabId,
        pointer_content_x: gpui::Pixels,
    ) -> bool {
        let Some(drag_layout) = self.tab_drag_layout.as_ref() else {
            return false;
        };
        let visual_drag_bounds = self.tab_drag_visual_bounds(dragged_tab_id);
        let Some(drag_update) = update_tab_drag_state(
            &self.tabs,
            &mut self.dragging,
            drag_layout,
            dragged_tab_id,
            pointer_content_x,
            visual_drag_bounds,
        ) else {
            return false;
        };

        if drag_update.from_index == drag_update.to_index {
            return true;
        }

        if drag_update.should_reorder {
            self.move_tab_to_index(drag_update.from_index, drag_update.to_index, dragged_tab_id);
        }

        true
    }

    fn update_tab_drag_autoscroll(&mut self, pointer_viewport_x: gpui::Pixels) -> bool {
        let Some(drag_state) = self.dragging.as_ref() else {
            return false;
        };
        let visual_left_x = drag_state.visual_left_x;
        let dragged_tab_width = self.dragged_tab_width(drag_state.tab_id);

        if !self.tab_strip_is_scrollable || self.tab_scroll_handle.max_offset().x <= px(0.) {
            if let Some(drag_state) = self.dragging.as_mut() {
                drag_state.autoscroll_pointer_x = None;
            }
            return false;
        }

        if !tab_drag_autoscroll_edge_reached(
            visual_left_x,
            dragged_tab_width,
            self.tab_scroll_handle.offset().x,
            self.tab_scroll_handle.bounds().size.width,
            pointer_viewport_x,
        ) {
            if let Some(drag_state) = self.dragging.as_mut() {
                drag_state.autoscroll_pointer_x = None;
            }
            return false;
        }

        self.tab_scroll_animation = None;
        let Some(drag_state) = self.dragging.as_mut() else {
            return false;
        };
        if drag_state.autoscroll_pointer_x.is_none() {
            drag_state.autoscroll_last_update = Instant::now();
        }
        drag_state.autoscroll_pointer_x = Some(pointer_viewport_x);
        true
    }

    fn clear_drag_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag_state) = self.dragging.take() else {
            return;
        };

        self.start_tab_drag_return_animation(drag_state);
        self.focus_active_terminal(window, cx);
        cx.notify();
    }

    fn cancel_tab_drag(&mut self) {
        let Some(drag_state) = self.dragging.take() else {
            return;
        };

        self.start_tab_drag_return_animation(drag_state);
    }

    fn start_tab_drag_return_animation(&mut self, drag_state: TabDragState) {
        let Some(layout) = self.tab_drag_layout.as_ref() else {
            return;
        };
        let Some(target_x) = layout.positions.get(&drag_state.tab_id).copied() else {
            return;
        };
        let Some(entry) = self.tab_entries.get_mut(&drag_state.tab_id) else {
            return;
        };

        let offset_x = drag_state.visual_left_x - target_x;
        if offset_is_effectively_zero(offset_x) {
            entry.reorder_animation = None;
        } else {
            entry.reorder_animation = Some(TabReorderAnimation {
                start: Instant::now(),
                offset_x,
            });
        }
    }

    fn set_hovered_tab(&mut self, tab_id: TabId, is_hovered: bool, cx: &mut Context<Self>) {
        let next_hovered_tab = if is_hovered {
            Some(tab_id)
        } else if self.hovered_tab == Some(tab_id) {
            None
        } else {
            self.hovered_tab
        };

        if self.hovered_tab == next_hovered_tab {
            return;
        }

        self.hovered_tab = next_hovered_tab;
        cx.notify();
    }

    fn advance_tab_animations(&mut self, window: &mut Window) {
        let mut tabs_to_remove = Vec::new();
        let mut needs_animation_frame = false;
        let now = Instant::now();

        for tab_id in self.tabs.iter().copied() {
            let Some(entry) = self.tab_entries.get_mut(&tab_id) else {
                continue;
            };

            match entry.phase {
                TabAnimationPhase::Opening { start, duration } => {
                    if now.duration_since(start) >= duration {
                        entry.phase = TabAnimationPhase::Open;
                    } else {
                        needs_animation_frame = true;
                    }
                }
                TabAnimationPhase::Open => {}
                TabAnimationPhase::Closing { start, duration } => {
                    if now.duration_since(start) >= duration {
                        tabs_to_remove.push(tab_id);
                    } else {
                        needs_animation_frame = true;
                    }
                }
            }

            if let Some(reorder_animation) = entry.reorder_animation {
                if now.duration_since(reorder_animation.start) >= TAB_REORDER_DURATION {
                    entry.reorder_animation = None;
                } else {
                    needs_animation_frame = true;
                }
            }
        }

        if self.advance_tab_layout_animations(now) {
            needs_animation_frame = true;
        }

        if self.advance_tab_scroll_animation(now) {
            needs_animation_frame = true;
        }

        if self.advance_tab_drag_autoscroll(now) {
            needs_animation_frame = true;
        }

        if !tabs_to_remove.is_empty() {
            self.tabs.retain(|tab_id| !tabs_to_remove.contains(tab_id));
            for tab_id in tabs_to_remove {
                self.tab_entries.remove(&tab_id);
                self.tab_layout_animations.remove(&tab_id);
            }
        }

        if self
            .dragging
            .as_ref()
            .is_some_and(|drag_state| !self.tab_entries.contains_key(&drag_state.tab_id))
        {
            self.dragging = None;
        }

        if self
            .hovered_tab
            .is_some_and(|tab_id| !self.tab_entries.contains_key(&tab_id))
        {
            self.hovered_tab = None;
        }

        if needs_animation_frame {
            window.request_animation_frame();
        }
    }

    fn advance_tab_layout_animations(&mut self, now: Instant) -> bool {
        let mut finished_tabs = Vec::new();
        let mut needs_animation_frame = false;

        for (tab_id, animation) in &self.tab_layout_animations {
            if now.duration_since(animation.start) >= animation.duration {
                finished_tabs.push(*tab_id);
            } else {
                needs_animation_frame = true;
            }
        }

        for tab_id in finished_tabs {
            self.tab_layout_animations.remove(&tab_id);
        }

        needs_animation_frame
    }

    fn advance_tab_scroll_animation(&mut self, now: Instant) -> bool {
        let Some(animation) = self.tab_scroll_animation else {
            return false;
        };

        let max_offset_x = self.tab_scroll_handle.max_offset().x;
        let target_x = animation.to_x.clamp(-max_offset_x, px(0.));
        let progress = tab_scroll_animation_progress(animation.start, now);
        let next_x = animation.from_x + (target_x - animation.from_x) * progress;
        let current_offset = self.tab_scroll_handle.offset();

        if progress >= 1.0 || offset_is_effectively_zero(next_x - target_x) {
            self.tab_scroll_handle
                .set_offset(gpui::point(target_x, current_offset.y));
            self.tab_scroll_animation = None;
            false
        } else {
            self.tab_scroll_handle
                .set_offset(gpui::point(next_x, current_offset.y));
            true
        }
    }

    fn advance_tab_drag_autoscroll(&mut self, now: Instant) -> bool {
        let Some(drag_state) = self.dragging.as_mut() else {
            return false;
        };
        if !self.tab_strip_is_scrollable {
            drag_state.autoscroll_pointer_x = None;
            return false;
        }
        let Some(pointer_viewport_x) = drag_state.autoscroll_pointer_x else {
            return false;
        };

        let dragged_tab_id = drag_state.tab_id;
        let elapsed = now.duration_since(drag_state.autoscroll_last_update);
        drag_state.autoscroll_last_update = now;

        let current_offset = self.tab_scroll_handle.offset();
        let Some(next_offset_x) = tab_drag_autoscroll_offset_x(
            pointer_viewport_x,
            self.tab_scroll_handle.bounds().size.width,
            current_offset.x,
            self.tab_scroll_handle.max_offset().x,
            elapsed,
        ) else {
            return false;
        };

        self.tab_scroll_handle
            .set_offset(gpui::point(next_offset_x, current_offset.y));
        let pointer_content_x = pointer_viewport_x - next_offset_x;
        self.update_tab_drag_position(dragged_tab_id, pointer_content_x);
        true
    }

    fn start_tab_scroll_animation(
        &mut self,
        direction: TabScrollDirection,
        cx: &mut Context<Self>,
    ) {
        let Some(target_x) = tab_scroll_target_offset(&self.tab_scroll_handle, direction) else {
            return;
        };

        self.tab_scroll_animation = Some(TabScrollAnimation {
            start: Instant::now(),
            from_x: self.tab_scroll_handle.offset().x,
            to_x: target_x,
        });
        cx.notify();
    }

    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let previous_target_widths = self.resolve_selectable_tab_widths(window, cx);
        let mut previous_widths = self.tab_widths_with_animations(&previous_target_widths, now);
        let new_tab_id = TabId::new();
        let entry = self.build_terminal_tab(
            TabAnimationPhase::Opening {
                start: now,
                duration: TAB_OPEN_DURATION,
            },
            window,
            cx,
        );

        self.tabs.push(new_tab_id);
        self.tab_entries.insert(new_tab_id, entry);
        self.active_tab = new_tab_id;
        self.scroll_active_tab_into_view = true;
        previous_widths.insert(new_tab_id, px(0.));

        let target_widths = self.resolve_selectable_tab_widths(window, cx);
        let duration = if self.tab_widths_change(&previous_widths, &target_widths, new_tab_id) {
            TAB_OPEN_DURATION
        } else {
            TAB_OPEN_FAST_DURATION
        };
        if let Some(entry) = self.tab_entries.get_mut(&new_tab_id) {
            entry.phase = TabAnimationPhase::Opening {
                start: now,
                duration,
            };
        }
        self.start_tab_layout_animation(previous_widths, target_widths, duration, now);

        if let Some(view) = self.active_terminal_view() {
            view.update(cx, |view, _cx| view.mark_output_read());
        }

        self.update_window_title(window, cx);
        self.focus_active_terminal(window, cx);
        cx.notify();
    }

    fn activate_tab(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tab_is_selectable(tab_id) {
            return;
        }

        let changed = self.active_tab != tab_id;
        if changed {
            self.active_tab = tab_id;
            self.scroll_active_tab_into_view = true;
            if let Some(view) = self.active_terminal_view() {
                view.update(cx, |view, _cx| view.mark_output_read());
            }
        }

        self.focus_active_terminal(window, cx);
        if changed {
            self.update_window_title(window, cx);
            cx.notify();
        }
    }

    fn close_tab(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(phase) = self.tab_entries.get(&tab_id).map(|entry| entry.phase) else {
            return;
        };
        if matches!(phase, TabAnimationPhase::Closing { .. }) {
            return;
        }

        let mut open_tabs = self.open_tabs();
        let Some(closing_index) = open_tabs.iter().position(|candidate| *candidate == tab_id)
        else {
            return;
        };

        if open_tabs.len() == 1 {
            window.remove_window();
            return;
        }

        let was_active = self.active_tab == tab_id;
        let closing_title = self
            .tab_entries
            .get(&tab_id)
            .map(|entry| self.tab_title(entry, cx));
        let now = Instant::now();
        let previous_target_widths = self.resolve_selectable_tab_widths(window, cx);
        let previous_widths = self.tab_widths_with_animations(&previous_target_widths, now);
        if let Some(tab_entry) = self.tab_entries.get_mut(&tab_id) {
            if let Some(title) = closing_title {
                tab_entry.title_fallback = title;
            }
            tab_entry.live_content = None;
            tab_entry.phase = TabAnimationPhase::Closing {
                start: now,
                duration: TAB_CLOSE_DURATION,
            };
        }
        let mut target_widths = self.resolve_selectable_tab_widths(window, cx);
        target_widths.insert(tab_id, px(0.));
        self.start_tab_layout_animation(previous_widths, target_widths, TAB_CLOSE_DURATION, now);

        if self
            .dragging
            .as_ref()
            .is_some_and(|drag_state| drag_state.tab_id == tab_id)
        {
            self.dragging = None;
        }

        if was_active {
            let Some(next_active_index) = next_active_after_close(closing_index, open_tabs.len())
            else {
                window.remove_window();
                return;
            };

            open_tabs.remove(closing_index);
            if let Some(next_tab_id) = open_tabs.get(next_active_index).copied() {
                self.active_tab = next_tab_id;
                self.scroll_active_tab_into_view = true;
                if let Some(view) = self.active_terminal_view() {
                    view.update(cx, |view, _cx| view.mark_output_read());
                }
            }

            self.update_window_title(window, cx);
        }

        self.focus_active_terminal(window, cx);

        cx.notify();
    }

    fn select_tab_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let open_tabs = self.open_tabs();
        let Some(tab_id) = open_tabs.get(index).copied() else {
            return;
        };
        self.activate_tab(tab_id, window, cx);
    }

    fn move_tab_to_index(&mut self, from: usize, to: usize, dragged_tab_id: TabId) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }

        let now = Instant::now();
        let Some(layout) = self.tab_drag_layout.as_ref() else {
            return;
        };
        let tab_slot_widths = self.tab_slot_widths(&layout.widths, now);
        let positions_before = tab_layout_positions(&self.tabs, &tab_slot_widths);
        let visual_positions_before = self.tab_visual_positions(&positions_before, now);

        move_tab_ids_to_index(&mut self.tabs, from, to);

        let positions_after = tab_layout_positions(&self.tabs, &tab_slot_widths);
        for tab_id in self.tabs.iter().copied() {
            if tab_id == dragged_tab_id {
                if let Some(entry) = self.tab_entries.get_mut(&tab_id) {
                    entry.reorder_animation = None;
                }
                continue;
            }

            let Some(entry) = self.tab_entries.get_mut(&tab_id) else {
                continue;
            };
            let Some(visual_x_before) = visual_positions_before.get(&tab_id).copied() else {
                continue;
            };
            let Some(layout_x_after) = positions_after.get(&tab_id).copied() else {
                continue;
            };

            let offset_x = visual_x_before - layout_x_after;
            if offset_is_effectively_zero(offset_x) {
                entry.reorder_animation = None;
            } else {
                entry.reorder_animation = Some(TabReorderAnimation {
                    start: now,
                    offset_x,
                });
            }
        }
    }

    fn selectable_tab_titles(&self, cx: &App) -> Vec<(TabId, String)> {
        self.tabs
            .iter()
            .copied()
            .filter(|tab_id| self.tab_is_selectable(*tab_id))
            .filter_map(|tab_id| {
                self.tab_entries
                    .get(&tab_id)
                    .map(|entry| (tab_id, self.tab_title(entry, cx)))
            })
            .collect()
    }

    fn resolve_selectable_tab_widths(
        &self,
        window: &Window,
        cx: &App,
    ) -> HashMap<TabId, gpui::Pixels> {
        let tab_titles = self.selectable_tab_titles(cx);
        let strip_layout_plan = tab_strip_layout_plan(
            tab_titles.len(),
            window.viewport_size().width,
            RIGHT_DRAG_GUTTER_MIN_WIDTH,
        );

        resolve_tab_widths(
            &tab_titles,
            strip_layout_plan.tabs_available_width,
            strip_layout_plan.scrollable,
        )
    }

    fn tab_widths_change(
        &self,
        previous_widths: &HashMap<TabId, gpui::Pixels>,
        target_widths: &HashMap<TabId, gpui::Pixels>,
        excluded_tab_id: TabId,
    ) -> bool {
        for tab_id in self.tabs.iter().copied() {
            if tab_id == excluded_tab_id {
                continue;
            }

            let previous_width = previous_widths
                .get(&tab_id)
                .copied()
                .unwrap_or(TAB_FALLBACK_WIDTH);
            let target_width = target_widths
                .get(&tab_id)
                .copied()
                .unwrap_or(TAB_FALLBACK_WIDTH);
            if !offset_is_effectively_zero(previous_width - target_width) {
                return true;
            }
        }

        false
    }

    fn start_tab_layout_animation(
        &mut self,
        previous_widths: HashMap<TabId, gpui::Pixels>,
        target_widths: HashMap<TabId, gpui::Pixels>,
        duration: Duration,
        now: Instant,
    ) {
        for (tab_id, target_width) in target_widths {
            let from_width = previous_widths
                .get(&tab_id)
                .copied()
                .unwrap_or(TAB_FALLBACK_WIDTH);

            if offset_is_effectively_zero(from_width - target_width) {
                self.tab_layout_animations.remove(&tab_id);
                continue;
            }

            if self
                .tab_layout_animations
                .get(&tab_id)
                .is_some_and(|animation| {
                    offset_is_effectively_zero(animation.to_width - target_width)
                })
            {
                continue;
            }

            self.tab_layout_animations.insert(
                tab_id,
                TabLayoutAnimation {
                    start: now,
                    duration,
                    from_width,
                    to_width: target_width,
                },
            );
        }
    }

    fn tab_slot_widths(
        &self,
        resolved_widths: &HashMap<TabId, gpui::Pixels>,
        now: Instant,
    ) -> HashMap<TabId, gpui::Pixels> {
        self.tabs
            .iter()
            .copied()
            .filter_map(|tab_id| {
                let entry = self.tab_entries.get(&tab_id)?;
                let full_width = resolved_widths.get(&tab_id).copied()?;
                let visual_style = tab_visual_style(entry, full_width, now);
                Some((tab_id, visual_style.slot_width))
            })
            .collect()
    }

    fn tab_widths_with_animations(
        &self,
        target_widths: &HashMap<TabId, gpui::Pixels>,
        now: Instant,
    ) -> HashMap<TabId, gpui::Pixels> {
        self.tabs
            .iter()
            .copied()
            .filter_map(|tab_id| {
                if let Some(animation) = self.tab_layout_animations.get(&tab_id).copied() {
                    return Some((tab_id, tab_layout_animation_width(animation, now)));
                }

                let width = target_widths.get(&tab_id).copied()?;
                Some((tab_id, width))
            })
            .collect()
    }

    fn tab_visual_positions(
        &self,
        layout_positions: &HashMap<TabId, gpui::Pixels>,
        now: Instant,
    ) -> HashMap<TabId, gpui::Pixels> {
        self.tabs
            .iter()
            .copied()
            .filter_map(|tab_id| {
                let entry = self.tab_entries.get(&tab_id)?;
                let layout_x = layout_positions.get(&tab_id).copied()?;
                Some((tab_id, tab_visual_position(entry, layout_x, now)))
            })
            .collect()
    }

    fn focus_active_terminal(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.active_terminal_focus_handle(cx), cx);
    }

    fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab(self.active_tab, window, cx);
    }

    fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.new_tab(window, cx);
    }

    fn on_close_active_tab(
        &mut self,
        _: &CloseActiveTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_active_tab(window, cx);
    }

    fn on_select_next_tab(
        &mut self,
        _: &SelectNextTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_next_tab(window, cx);
    }

    fn on_select_previous_tab(
        &mut self,
        _: &SelectPreviousTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_previous_tab(window, cx);
    }

    define_select_tab_handlers!(
        (on_select_tab_1, SelectTab1, 0),
        (on_select_tab_2, SelectTab2, 1),
        (on_select_tab_3, SelectTab3, 2),
        (on_select_tab_4, SelectTab4, 3),
        (on_select_tab_5, SelectTab5, 4),
        (on_select_tab_6, SelectTab6, 5),
        (on_select_tab_7, SelectTab7, 6),
        (on_select_tab_8, SelectTab8, 7),
        (on_select_tab_9, SelectTab9, 8),
    );

    fn select_next_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open_tabs = self.open_tabs();
        let len = open_tabs.len();
        if len == 0 {
            return;
        }

        let current = open_tabs
            .iter()
            .position(|tab_id| *tab_id == self.active_tab)
            .unwrap_or(0);
        let next = wrapped_index(current, len, 1);
        self.select_tab_index(next, window, cx);
    }

    fn select_previous_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open_tabs = self.open_tabs();
        let len = open_tabs.len();
        if len == 0 {
            return;
        }

        let current = open_tabs
            .iter()
            .position(|tab_id| *tab_id == self.active_tab)
            .unwrap_or(0);
        let previous = wrapped_index(current, len, -1);
        self.select_tab_index(previous, window, cx);
    }

    pub fn active_terminal_focus_handle(&self, cx: &App) -> FocusHandle {
        let entry = self
            .tab_entries
            .get(&self.active_tab)
            .expect("active tab exists");
        let content = entry
            .live_content
            .as_ref()
            .expect("active tab has live content");
        match content {
            TabContent::Terminal { view, .. } => view.read(cx).focus_handle(cx),
        }
    }

    fn dragged_tab_width(&self, tab_id: TabId) -> gpui::Pixels {
        self.tab_drag_layout
            .as_ref()
            .and_then(|layout| layout.widths.get(&tab_id).copied())
            .unwrap_or(TAB_FALLBACK_WIDTH)
    }

    fn tab_drag_visual_bounds(&self, tab_id: TabId) -> Option<(gpui::Pixels, gpui::Pixels)> {
        if !self.tab_strip_is_scrollable {
            return None;
        }

        let tab_width = self.dragged_tab_width(tab_id);
        let viewport_width = self.tab_scroll_handle.bounds().size.width;
        if viewport_width <= px(0.) {
            return None;
        }

        let visible_left_x = -self.tab_scroll_handle.offset().x + ACTIVE_TAB_SHOULDER_WIDTH;
        let visible_right_x = -self.tab_scroll_handle.offset().x + viewport_width;
        let max_left_x = visible_right_x - tab_width - ACTIVE_TAB_SHOULDER_WIDTH;
        if max_left_x < visible_left_x {
            Some((visible_left_x, visible_left_x))
        } else {
            Some((visible_left_x, max_left_x))
        }
    }

    fn should_confirm_window_close(&self) -> bool {
        should_confirm_window_close(self.open_tab_count())
    }

    /// Called by `on_window_should_close`. Returns `true` to allow close,
    /// `false` to cancel (a prompt is shown instead).
    pub fn handle_window_should_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.should_confirm_window_close() {
            return true;
        }

        let tab_count = self.open_tab_count();
        let receiver = window.prompt(
            PromptLevel::Warning,
            "Do you want to close all tabs?",
            Some(&format!("You have {tab_count} tabs open")),
            &["Close all", "Cancel"],
            cx,
        );

        cx.spawn_in(window, async move |_, cx| {
            if let Ok(answer_index) = receiver.await {
                // Index 0 = "Close all"
                if answer_index == 0 {
                    cx.update(|window, _cx| window.remove_window()).ok();
                }
            }
        })
        .detach();

        false
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.advance_tab_animations(window);
        let active_content = self.render_active_content();
        let metrics = title_bar_metrics(window);
        let palette = title_bar_palette(self.active_terminal_background(cx));
        let now = Instant::now();
        let tab_titles = self.selectable_tab_titles(cx);
        let strip_layout_plan = tab_strip_layout_plan(
            tab_titles.len(),
            window.viewport_size().width,
            RIGHT_DRAG_GUTTER_MIN_WIDTH,
        );
        let is_scrollable = strip_layout_plan.scrollable;
        self.tab_strip_is_scrollable = is_scrollable;
        let resolved_widths = resolve_tab_widths(
            &tab_titles,
            strip_layout_plan.tabs_available_width,
            is_scrollable,
        );
        let animated_widths = self.tab_widths_with_animations(&resolved_widths, now);
        let tab_slot_widths = self.tab_slot_widths(&animated_widths, now);
        let mut tab_drag_positions = tab_layout_positions(&self.tabs, &tab_slot_widths);
        for position in tab_drag_positions.values_mut() {
            *position += ACTIVE_TAB_SHOULDER_WIDTH;
        }
        self.tab_drag_layout = Some(TabDragLayout {
            widths: animated_widths,
            positions: tab_drag_positions,
            padding_left: ACTIVE_TAB_SHOULDER_WIDTH,
        });

        let selectable_tabs = self.open_tabs();
        let active_tab_scroll_index = selectable_tabs
            .iter()
            .position(|tab_id| *tab_id == self.active_tab);
        let active_tab_index = self
            .tabs
            .iter()
            .position(|tab_id| *tab_id == self.active_tab);
        let hovered_tab_index = self
            .hovered_tab
            .and_then(|hovered_tab| self.tabs.iter().position(|tab_id| *tab_id == hovered_tab));

        if !is_scrollable || self.scroll_active_tab_into_view {
            self.tab_scroll_animation = None;
        }

        if is_scrollable
            && self.scroll_active_tab_into_view
            && let Some(active_index) = active_tab_scroll_index
        {
            let scroll_update =
                scroll_tab_into_view(&self.tab_scroll_handle, active_index, tab_titles.len());
            let keep_scrolling_active_tab = self
                .tab_entries
                .get(&self.active_tab)
                .is_some_and(|entry| matches!(entry.phase, TabAnimationPhase::Opening { .. }));

            // Middle tabs use GPUI's deferred `scroll_to_item`. Edge tabs are snapped
            // to the exact button-reachable offset so the corresponding arrow disables
            // immediately instead of leaving a few pixels still scrollable.
            if scroll_update == TabScrollUpdate::Deferred && !keep_scrolling_active_tab {
                window.request_animation_frame();
            }
            self.scroll_active_tab_into_view = keep_scrolling_active_tab;
        }

        let tab_drag_layout = self
            .tab_drag_layout
            .as_ref()
            .expect("tab drag layout was just assigned");
        let resolved_widths = &tab_drag_layout.widths;

        let scroll_offset_x = self.tab_scroll_handle.offset().x;
        let max_scroll_x = self.tab_scroll_handle.max_offset().x;
        let can_scroll_left = f32::from(scroll_offset_x) < -0.5;
        let can_scroll_right = f32::from(max_scroll_x + scroll_offset_x) > 0.5;

        let mut tabs = div()
            .id("title-bar-tabs")
            .flex()
            .flex_row()
            .relative()
            .items_stretch()
            .h_full()
            .min_w_0()
            .pl(ACTIVE_TAB_SHOULDER_WIDTH)
            .pr(ACTIVE_TAB_SHOULDER_WIDTH)
            .overflow_y_hidden()
            .on_drag_move::<DraggedTab>(cx.listener(
                |this, event: &DragMoveEvent<DraggedTab>, window, cx| {
                    this.handle_tab_drag_move(event, window, cx);
                },
            ));

        let mut tab_overlay = None;

        for (tab_index, tab_id) in self.tabs.iter().copied().enumerate() {
            let Some(entry) = self.tab_entries.get(&tab_id) else {
                continue;
            };

            let title = self.tab_title(entry, cx);
            let is_active = self.active_tab == tab_id;
            let is_hovered = self.hovered_tab == Some(tab_id);
            let is_closing = matches!(entry.phase, TabAnimationPhase::Closing { .. });
            let is_draggable = is_active && matches!(entry.phase, TabAnimationPhase::Open);
            let is_dragged = self
                .dragging
                .as_ref()
                .is_some_and(|drag_state| drag_state.tab_id == tab_id);
            let full_width = resolved_widths
                .get(&tab_id)
                .copied()
                .unwrap_or(TAB_FALLBACK_WIDTH);
            let visual_style = tab_visual_style(entry, full_width, now);
            let show_separator = self.dragging.is_none()
                && tab_separator_is_visible(tab_index, active_tab_index, hovered_tab_index);
            let close_button = if is_closing {
                render_tab_close_button(is_active, true).into_any_element()
            } else {
                render_tab_close_button(is_active, false)
                    .id(format!("title-bar-tab-close-{}", tab_id.as_u64()))
                    .hover(move |style| {
                        style
                            .bg(palette.close_button_hover_background)
                            .text_color(palette.close_button_hover_foreground)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.close_tab(tab_id, window, cx);
                        cx.stop_propagation();
                    }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .into_any_element()
            };

            let mut tab_shell = div()
                .id(format!("title-bar-tab-shell-{}", tab_id.as_u64()))
                .group(format!("title-bar-tab-group-{}", tab_id.as_u64()))
                .flex_none()
                .child(render_tab_visual(
                    entry.profile_icon,
                    &title,
                    is_active,
                    is_hovered && !is_dragged,
                    show_separator,
                    visual_style,
                    full_width,
                    metrics,
                    palette,
                    close_button,
                ));

            let tab_overlay_position = if is_dragged {
                self.dragging
                    .as_ref()
                    .map(|drag_state| drag_state.visual_left_x)
            } else if is_active && entry.reorder_animation.is_some() {
                self.tab_drag_layout
                    .as_ref()
                    .and_then(|layout| layout.positions.get(&tab_id).copied())
            } else {
                None
            };

            if let Some(overlay_left_x) = tab_overlay_position {
                tab_shell = tab_shell.invisible();
                let overlay_left_x = if is_scrollable {
                    overlay_left_x + scroll_offset_x
                } else {
                    overlay_left_x
                };
                let overlay_style = if is_dragged {
                    TabVisualStyle {
                        slot_width: full_width,
                        surface_offset_x: px(0.),
                        surface_opacity: 1.0,
                    }
                } else {
                    visual_style
                };
                tab_overlay = Some(
                    div()
                        .absolute()
                        .top(px(0.))
                        .left(overlay_left_x)
                        .w(overlay_style.slot_width)
                        .h_full()
                        .when(is_dragged, |overlay| {
                            overlay.child(
                                div()
                                    .absolute()
                                    .top(px(0.))
                                    .left(-ACTIVE_TAB_SHOULDER_WIDTH)
                                    .w(full_width + ACTIVE_TAB_SHOULDER_WIDTH * 2.0)
                                    .h_full()
                                    .bg(palette.title_bar_background),
                            )
                        })
                        .child(render_tab_visual(
                            entry.profile_icon,
                            &title,
                            is_active,
                            false,
                            false,
                            overlay_style,
                            full_width,
                            metrics,
                            palette,
                            render_tab_close_button(is_active, false).into_any_element(),
                        ))
                        .into_any_element(),
                );
            }

            if !is_closing {
                tab_shell = tab_shell
                    .on_hover(cx.listener(move |this, is_hovered: &bool, _window, cx| {
                        this.set_hovered_tab(tab_id, *is_hovered, cx);
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_tab(tab_id, window, cx);
                    }))
                    .when(!is_active, |tab| {
                        tab.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                this.activate_tab(tab_id, window, cx);
                            }),
                        )
                    })
                    .on_mouse_up(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            this.close_tab(tab_id, window, cx);
                        }),
                    );
            }

            if is_draggable {
                tab_shell =
                    tab_shell.on_drag(DraggedTab { tab_id }, |_dragged, _position, _window, cx| {
                        cx.stop_propagation();
                        cx.new(|_| EmptyView)
                    });
            }

            tabs = tabs.child(tab_shell);
        }

        if !is_scrollable && let Some(tab_overlay) = tab_overlay.take() {
            tabs = tabs.child(tab_overlay);
        }

        let left_scroll_button = {
            let button =
                render_tab_scroll_button(TabScrollDirection::Left, can_scroll_left, palette)
                    .id("title-bar-scroll-left")
                    .when(can_scroll_left, |button| {
                        button
                            .hover(|style| {
                                style
                                    .bg(palette.inactive_tab_hover_background)
                                    .text_color(palette.active_tab_foreground)
                            })
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.start_tab_scroll_animation(TabScrollDirection::Left, cx);
                            }))
                    });

            div()
                .flex_none()
                .h_full()
                .pr(TAB_SCROLL_BUTTON_GAP)
                .flex()
                .items_center()
                .child(button)
        };

        let right_scroll_button = {
            let button =
                render_tab_scroll_button(TabScrollDirection::Right, can_scroll_right, palette)
                    .id("title-bar-scroll-right")
                    .when(can_scroll_right, |button| {
                        button
                            .hover(|style| {
                                style
                                    .bg(palette.inactive_tab_hover_background)
                                    .text_color(palette.active_tab_foreground)
                            })
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.start_tab_scroll_animation(TabScrollDirection::Right, cx);
                            }))
                    });

            div()
                .flex_none()
                .h_full()
                .pl(TAB_SCROLL_BUTTON_GAP)
                .flex()
                .items_center()
                .child(button)
        };

        let title_bar_tabs = if is_scrollable {
            div()
                .id("title-bar-tabs-viewport")
                .flex_none()
                .flex()
                .flex_row()
                .items_stretch()
                .h_full()
                .w(strip_layout_plan.tab_strip_width)
                .min_w_0()
                .overflow_hidden()
                .child(left_scroll_button)
                .child(
                    div()
                        .id("title-bar-tabs-scroll-viewport")
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_hidden()
                        .child(
                            tabs.id("title-bar-tabs-scroll")
                                .w_full()
                                .h_full()
                                .overflow_x_scroll()
                                .overflow_y_hidden()
                                .track_scroll(&self.tab_scroll_handle),
                        )
                        .when_some(tab_overlay, |scroll_viewport, tab_overlay| {
                            scroll_viewport.child(tab_overlay)
                        }),
                )
                .child(right_scroll_button)
                .into_any_element()
        } else {
            div()
                .id("title-bar-tabs-viewport")
                .flex_none()
                .flex()
                .flex_row()
                .items_stretch()
                .h_full()
                .child(tabs.id("title-bar-tabs-scroll").flex_none())
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .key_context(WORKSPACE_KEY_CONTEXT)
            .on_drop(cx.listener(|this, _dragged: &DraggedTab, window, cx| {
                this.clear_drag_state(window, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.clear_drag_state(window, cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.clear_drag_state(window, cx);
                }),
            )
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_close_active_tab))
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| {
                if this.handle_window_should_close(window, cx) {
                    window.remove_window();
                }
            }))
            .on_action(cx.listener(Self::on_select_next_tab))
            .on_action(cx.listener(Self::on_select_previous_tab))
            .on_action(cx.listener(Self::on_select_tab_1))
            .on_action(cx.listener(Self::on_select_tab_2))
            .on_action(cx.listener(Self::on_select_tab_3))
            .on_action(cx.listener(Self::on_select_tab_4))
            .on_action(cx.listener(Self::on_select_tab_5))
            .on_action(cx.listener(Self::on_select_tab_6))
            .on_action(cx.listener(Self::on_select_tab_7))
            .on_action(cx.listener(Self::on_select_tab_8))
            .on_action(cx.listener(Self::on_select_tab_9))
            .on_action(cx.listener(|this, _: &IncreaseFontSize, _window, cx| {
                if let Some(view) = this.active_terminal_view() {
                    view.update(cx, |view, cx| view.increase_font_size(cx));
                }
            }))
            .on_action(cx.listener(|this, _: &DecreaseFontSize, _window, cx| {
                if let Some(view) = this.active_terminal_view() {
                    view.update(cx, |view, cx| view.decrease_font_size(cx));
                }
            }))
            .on_action(cx.listener(|this, _: &ResetFontSize, _window, cx| {
                if let Some(view) = this.active_terminal_view() {
                    view.update(cx, |view, cx| view.reset_font_size(cx));
                }
            }))
            .child(
                div()
                    .id("title-bar")
                    .w_full()
                    .h(metrics.height)
                    .bg(palette.title_bar_background)
                    .flex()
                    .items_stretch()
                    .child(
                        div()
                            .id("title-bar-left-gutter")
                            .flex_none()
                            .w(LEFT_DRAG_GUTTER_WIDTH)
                            .h_full()
                            .window_control_area(WindowControlArea::Drag),
                    )
                    .child(
                        div()
                            .id("title-bar-strip")
                            .flex_none()
                            .h_full()
                            .pt(metrics.top_gap)
                            .flex()
                            .items_start()
                            .overflow_hidden()
                            .child(
                                div()
                                    .id("title-bar-strip-row")
                                    .flex_none()
                                    .h(metrics.tab_surface_height)
                                    .flex()
                                    .items_stretch()
                                    .child(title_bar_tabs)
                                    .child(
                                        div()
                                            .id("title-bar-new-tab")
                                            .flex_none()
                                            .h_full()
                                            .pl(NEW_TAB_BUTTON_SECTION_LEFT_PADDING)
                                            .pr(NEW_TAB_BUTTON_SECTION_RIGHT_PADDING)
                                            .flex()
                                            .items_center()
                                            .child(
                                                render_new_tab_button(palette, metrics)
                                                    .id("title-bar-new-tab-button")
                                                    .hover(|style| {
                                                        style
                                                            .bg(palette
                                                                .inactive_tab_hover_background)
                                                    })
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.new_tab(window, cx);
                                                        },
                                                    )),
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("title-bar-right-gutter")
                            .flex_grow_1()
                            .min_w(RIGHT_DRAG_GUTTER_MIN_WIDTH)
                            .h_full()
                            .window_control_area(WindowControlArea::Drag),
                    )
                    .child(WindowsWindowControls::new(metrics.height)),
            )
            .child(
                div()
                    .flex_1()
                    .mt(-ACTIVE_TAB_BOTTOM_OVERLAP)
                    .overflow_hidden()
                    .when_some(active_content, |el, content| el.child(content)),
            )
    }
}

fn move_tab_ids_to_index(tabs: &mut Vec<TabId>, from: usize, to: usize) {
    if from >= tabs.len() || to >= tabs.len() || from == to {
        return;
    }

    let tab_id = tabs.remove(from);
    tabs.insert(to, tab_id);
}

fn wrapped_index(current: usize, len: usize, delta: isize) -> usize {
    assert!(len > 0);
    let len = len as isize;
    (current as isize + delta).rem_euclid(len) as usize
}

/// Prompt when more than one tab is open, regardless of process state.
fn should_confirm_window_close(tab_count: usize) -> bool {
    tab_count > 1
}

fn next_active_after_close(closing_index: usize, len: usize) -> Option<usize> {
    if len <= 1 {
        return None;
    }

    if closing_index + 1 < len {
        Some(closing_index)
    } else {
        Some(closing_index.saturating_sub(1))
    }
}

fn tab_separator_is_visible(
    tab_index: usize,
    active_tab_index: Option<usize>,
    hovered_tab_index: Option<usize>,
) -> bool {
    if tab_index_is_selected_or_left_adjacent(tab_index, active_tab_index) {
        return false;
    }

    if tab_index_is_selected_or_left_adjacent(tab_index, hovered_tab_index) {
        return false;
    }

    true
}

fn tab_drag_autoscroll_edge_reached(
    drag_left_x: gpui::Pixels,
    tab_width: gpui::Pixels,
    scroll_offset_x: gpui::Pixels,
    viewport_width: gpui::Pixels,
    pointer_viewport_x: gpui::Pixels,
) -> bool {
    let edge_width = TAB_DRAG_AUTOSCROLL_EDGE_WIDTH.min(viewport_width / 2.0);
    if edge_width <= px(0.) {
        return false;
    }

    let visible_left_x = -scroll_offset_x + ACTIVE_TAB_SHOULDER_WIDTH;
    let visible_right_x = -scroll_offset_x + viewport_width - ACTIVE_TAB_SHOULDER_WIDTH;
    if pointer_viewport_x < edge_width {
        return drag_left_x <= visible_left_x + px(0.5);
    }
    if pointer_viewport_x > viewport_width - edge_width {
        return drag_left_x + tab_width >= visible_right_x - px(0.5);
    }

    false
}

fn tab_index_is_selected_or_left_adjacent(tab_index: usize, target_index: Option<usize>) -> bool {
    let Some(target_index) = target_index else {
        return false;
    };

    if tab_index == target_index {
        return true;
    }

    tab_index + 1 == target_index
}

fn tab_layout_animation_width(animation: TabLayoutAnimation, now: Instant) -> gpui::Pixels {
    let progress = tab_animation_progress(animation.start, animation.duration, now);
    animation.from_width + (animation.to_width - animation.from_width) * progress
}

fn tab_scroll_animation_progress(start: Instant, now: Instant) -> f32 {
    let elapsed = now.duration_since(start).as_secs_f32();
    let duration = TAB_SCROLL_DURATION.as_secs_f32();
    let progress = if duration == 0.0 {
        1.0
    } else {
        (elapsed / duration).clamp(0.0, 1.0)
    };

    1.0 - (1.0 - progress).powi(5)
}

#[cfg(test)]
mod tests {
    use super::{
        move_tab_ids_to_index, next_active_after_close, tab_drag_autoscroll_edge_reached,
        tab_separator_is_visible, wrapped_index,
    };
    use crate::types::TabId;
    use gpui::px;

    #[test]
    fn wrapped_index_moves_forward_with_wrap() {
        assert_eq!(wrapped_index(0, 4, 1), 1);
        assert_eq!(wrapped_index(3, 4, 1), 0);
    }

    #[test]
    fn wrapped_index_moves_backward_with_wrap() {
        assert_eq!(wrapped_index(2, 4, -1), 1);
        assert_eq!(wrapped_index(0, 4, -1), 3);
    }

    #[test]
    fn next_active_after_close_prefers_tab_to_the_right() {
        assert_eq!(next_active_after_close(1, 4), Some(1));
    }

    #[test]
    fn next_active_after_close_falls_back_to_leftmost_available() {
        assert_eq!(next_active_after_close(3, 4), Some(2));
    }

    #[test]
    fn move_tab_to_index_moves_first_tab_to_second_position() {
        let first = TabId::new();
        let second = TabId::new();
        let third = TabId::new();
        let mut tabs = vec![first, second, third];

        move_tab_ids_to_index(&mut tabs, 0, 1);

        assert_eq!(tabs, vec![second, first, third]);
    }

    #[test]
    fn move_tab_to_index_moves_last_tab_to_first_position() {
        let first = TabId::new();
        let second = TabId::new();
        let third = TabId::new();
        let mut tabs = vec![first, second, third];

        move_tab_ids_to_index(&mut tabs, 2, 0);

        assert_eq!(tabs, vec![third, first, second]);
    }

    #[test]
    fn move_tab_to_index_is_no_op_when_target_matches_source() {
        let first = TabId::new();
        let second = TabId::new();
        let third = TabId::new();
        let mut tabs = vec![first, second, third];

        move_tab_ids_to_index(&mut tabs, 1, 1);

        assert_eq!(tabs, vec![first, second, third]);
    }

    #[test]
    fn move_tab_to_index_keeps_active_tab_id_stable() {
        let first = TabId::new();
        let second = TabId::new();
        let third = TabId::new();
        let active_tab = second;
        let mut tabs = vec![first, second, third];

        move_tab_ids_to_index(&mut tabs, 2, 0);

        assert_eq!(active_tab, second);
        assert!(tabs.contains(&active_tab));
    }

    #[test]
    fn tab_separator_hides_selected_tab_and_left_neighbor() {
        assert!(!tab_separator_is_visible(1, Some(1), None));
        assert!(!tab_separator_is_visible(0, Some(1), None));
        assert!(tab_separator_is_visible(2, Some(1), None));
    }

    #[test]
    fn tab_separator_hides_hovered_tab_and_left_neighbor() {
        assert!(!tab_separator_is_visible(2, Some(0), Some(2)));
        assert!(!tab_separator_is_visible(1, Some(0), Some(2)));
        assert!(tab_separator_is_visible(3, Some(0), Some(2)));
    }

    #[test]
    fn tab_drag_autoscroll_waits_for_dragged_tab_edge() {
        assert!(!tab_drag_autoscroll_edge_reached(
            px(240.),
            px(100.),
            px(-200.),
            px(360.),
            px(10.),
        ));
        assert!(tab_drag_autoscroll_edge_reached(
            px(204.),
            px(100.),
            px(-200.),
            px(360.),
            px(10.),
        ));
    }
}
