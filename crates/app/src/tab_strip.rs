use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Background, Corners, Div, FontWeight, Hsla, IntoElement, ParentElement,
    Pixels, ScrollHandle, Styled, Window, canvas, div, fill, point, px, rgba, size,
};
use gpui::{Bounds, FillOptions, PathBuilder, PathStyle};

use crate::profile::ProfileIconKind;
use crate::types::TabId;
use crate::workspace::{TabAnimationPhase, TabEntry};
use ui::title_bar::{WINDOWS_CAPTION_BUTTON_WIDTH, title_bar_height, windows_symbol_font};

pub const LEFT_DRAG_GUTTER_WIDTH: Pixels = px(4.);
pub const RIGHT_DRAG_GUTTER_MIN_WIDTH: Pixels = px(45.);
const TAB_RADIUS: Pixels = px(8.);
const TAB_MIN_WIDTH: Pixels = px(100.);
const TAB_MAX_WIDTH: Pixels = px(240.);
pub const TAB_FALLBACK_WIDTH: Pixels = TAB_MIN_WIDTH;
const TAB_LABEL_GAP: Pixels = px(6.);
const TAB_ICON_LABEL_GAP: Pixels = px(8.);
const TAB_HORIZONTAL_PADDING_LEFT: Pixels = px(8.);
const TAB_HORIZONTAL_PADDING_RIGHT: Pixels = px(4.);
const TAB_ICON_SIZE: Pixels = px(16.);
const TAB_CLOSE_BUTTON_SLOT_WIDTH: Pixels = px(32.);
const TAB_CLOSE_BUTTON_SIZE: Pixels = px(24.);
const TAB_BUTTON_RADIUS: Pixels = px(4.);
const TAB_CLOSE_GLYPH_SIZE: Pixels = px(12.);
const NEW_TAB_BUTTON_SIZE: Pixels = px(28.);
pub const NEW_TAB_BUTTON_SECTION_LEFT_PADDING: Pixels = px(3.);
pub const NEW_TAB_BUTTON_SECTION_RIGHT_PADDING: Pixels = px(4.);
const NEW_TAB_GLYPH_SIZE: Pixels = px(12.);
pub const ACTIVE_TAB_SHOULDER_WIDTH: Pixels = px(4.);
const ACTIVE_TAB_SHOULDER_HEIGHT: Pixels = px(4.);
pub const ACTIVE_TAB_BOTTOM_OVERLAP: Pixels = px(1.);
const INACTIVE_TAB_HOVER_INSET_X: Pixels = px(4.);
const INACTIVE_TAB_HOVER_INSET_BOTTOM: Pixels = px(0.);
const TAB_SEPARATOR_INSET_Y: Pixels = px(8.);
const TAB_SEPARATOR_WIDTH: Pixels = px(1.);
const TAB_SCROLL_BUTTON_SIZE: Pixels = px(32.);
pub const TAB_SCROLL_BUTTON_GAP: Pixels = px(2.);
const TAB_SCROLL_GLYPH_SIZE: Pixels = px(10.);
const TAB_SCROLL_AMOUNT: Pixels = px(100.);
const ACTIVE_TAB_PATH_TOLERANCE: f32 = 0.05;
// WT's tab close glyphs read crisper than the title text: active tabs use a
// nearly white glyph, inactive tabs use the primary dark text brush directly.
const ACTIVE_CLOSE_BUTTON_FOREGROUND: u32 = 0xffffffff;
const INACTIVE_CLOSE_BUTTON_FOREGROUND: u32 = 0x161616f2;
const CLOSE_BUTTON_HOVER_FOREGROUND: u32 = 0xffffffff;
const CLOSE_BUTTON_HOVER_BACKGROUND: u32 = 0xc42b1cff;
const TAB_DRAG_REORDER_THRESHOLD: Pixels = px(12.);
pub(crate) const TAB_DRAG_AUTOSCROLL_EDGE_WIDTH: Pixels = px(48.);
const TAB_DRAG_AUTOSCROLL_MAX_SPEED: Pixels = px(520.);
pub(crate) const TAB_OPEN_DURATION: Duration = Duration::from_millis(180);
pub(crate) const TAB_OPEN_FAST_DURATION: Duration = Duration::from_millis(140);
pub(crate) const TAB_CLOSE_DURATION: Duration = Duration::from_millis(160);
pub(crate) const TAB_REORDER_DURATION: Duration = Duration::from_millis(160);
pub(crate) const TAB_SCROLL_DURATION: Duration = Duration::from_millis(160);
const TAB_OPEN_REVEAL_SHIFT: Pixels = px(10.);

#[derive(Clone, Copy)]
pub struct TabVisualStyle {
    pub slot_width: Pixels,
    pub surface_offset_x: Pixels,
    pub surface_opacity: f32,
}

#[derive(Clone, Copy)]
pub struct TabStripLayoutPlan {
    pub tab_strip_width: Pixels,
    pub tabs_available_width: Pixels,
    pub scrollable: bool,
}

#[derive(Clone, Copy)]
pub struct TitleBarMetrics {
    pub height: Pixels,
    pub top_gap: Pixels,
    pub tab_surface_height: Pixels,
    pub new_tab_button_size: Pixels,
}

#[derive(Clone, Copy)]
pub struct TitleBarPalette {
    pub title_bar_background: Hsla,
    pub active_tab_background: Hsla,
    pub active_tab_foreground: Hsla,
    pub inactive_tab_background: Hsla,
    pub inactive_tab_foreground: Hsla,
    pub inactive_tab_hover_background: Hsla,
    pub tab_separator: Hsla,
    pub tab_border_brush: Hsla,
    pub close_button_hover_background: Hsla,
    pub close_button_hover_foreground: Hsla,
}

pub struct TabDragState {
    pub tab_id: TabId,
    pub pointer_offset_x: Pixels,
    // Keep the dragged tab visually pinned at the overflow edge while
    // the logical drag position keeps moving through the scrollable strip.
    pub visual_left_x: Pixels,
    pub drag_left_x: Pixels,
    pub last_reorder_left_x: Pixels,
    pub autoscroll_pointer_x: Option<Pixels>,
    pub autoscroll_last_update: Instant,
}

pub struct TabDragLayout {
    pub tab_ids: Vec<TabId>,
    pub widths: Vec<Pixels>,
    pub positions: Vec<Pixels>,
    pub padding_left: Pixels,
}

impl TabDragLayout {
    pub fn width_for(&self, tab_id: TabId) -> Option<Pixels> {
        let index = self
            .tab_ids
            .iter()
            .position(|candidate| *candidate == tab_id)?;
        self.widths.get(index).copied()
    }

    pub fn position_for(&self, tab_id: TabId) -> Option<Pixels> {
        let index = self
            .tab_ids
            .iter()
            .position(|candidate| *candidate == tab_id)?;
        self.positions.get(index).copied()
    }
}

pub struct TabDragUpdate {
    pub from_index: usize,
    pub to_index: usize,
    pub should_reorder: bool,
}

#[derive(Clone, Copy)]
enum TabShoulderSide {
    Left,
    Right,
}

pub(crate) trait TabDragEntry {
    fn tab_id(&self) -> TabId;
}

impl TabDragEntry for TabEntry {
    fn tab_id(&self) -> TabId {
        self.id
    }
}

impl TabDragEntry for TabId {
    fn tab_id(&self) -> TabId {
        *self
    }
}

pub fn title_bar_metrics(window: &Window) -> TitleBarMetrics {
    let height = title_bar_height(window);
    let top_gap = if window.is_maximized() {
        px(4.)
    } else {
        px(8.)
    };

    TitleBarMetrics {
        height,
        top_gap,
        tab_surface_height: height - top_gap,
        new_tab_button_size: NEW_TAB_BUTTON_SIZE,
    }
}

pub fn title_bar_palette(active_terminal_background: Hsla) -> TitleBarPalette {
    let title_bar_background: Hsla = rgba(0xe8e8e8ff).into();
    let inactive_tab_background = title_bar_background;
    let inactive_tab_hover_background = blend_color(title_bar_background, rgba(0x00000020).into());
    let active_tab_foreground = contrast_foreground(active_terminal_background);
    let inactive_tab_foreground = contrast_foreground(inactive_tab_background);

    TitleBarPalette {
        title_bar_background,
        active_tab_background: active_terminal_background,
        active_tab_foreground,
        inactive_tab_background,
        inactive_tab_foreground,
        inactive_tab_hover_background,
        tab_separator: rgba(0x00000023).into(),
        tab_border_brush: rgba(0x0000000f).into(),
        close_button_hover_background: rgba(CLOSE_BUTTON_HOVER_BACKGROUND).into(),
        close_button_hover_foreground: rgba(CLOSE_BUTTON_HOVER_FOREGROUND).into(),
    }
}

pub fn tab_strip_layout_plan(
    tab_count: usize,
    viewport_width: Pixels,
    right_drag_gutter_min_width: Pixels,
) -> TabStripLayoutPlan {
    let reserved_without_scroll = LEFT_DRAG_GUTTER_WIDTH
        + right_drag_gutter_min_width
        + WINDOWS_CAPTION_BUTTON_WIDTH * 3.0
        + NEW_TAB_BUTTON_SECTION_LEFT_PADDING
        + NEW_TAB_BUTTON_SECTION_RIGHT_PADDING
        + NEW_TAB_BUTTON_SIZE
        + ACTIVE_TAB_SHOULDER_WIDTH * 2.0;

    let tab_strip_width = (viewport_width - reserved_without_scroll).max(px(0.));
    let min_total_width = TAB_MIN_WIDTH * tab_count as f32;

    if tab_count > 0 && min_total_width > tab_strip_width {
        let scroll_reserved_width = TAB_SCROLL_BUTTON_SIZE * 2.0 + TAB_SCROLL_BUTTON_GAP * 2.0;
        TabStripLayoutPlan {
            tab_strip_width,
            tabs_available_width: (tab_strip_width - scroll_reserved_width).max(px(0.)),
            scrollable: true,
        }
    } else {
        TabStripLayoutPlan {
            tab_strip_width,
            tabs_available_width: tab_strip_width,
            scrollable: false,
        }
    }
}

pub fn tab_target_width(tab_count: usize, available_width: Pixels, scrollable: bool) -> Pixels {
    if tab_count == 0 {
        return px(0.);
    }

    if scrollable {
        return TAB_MIN_WIDTH;
    }

    (available_width / tab_count as f32).clamp(TAB_MIN_WIDTH, TAB_MAX_WIDTH)
}

pub fn tab_layout_positions(tab_widths: &[Pixels]) -> Vec<Pixels> {
    let mut positions = Vec::with_capacity(tab_widths.len());
    let mut current_x = px(0.);

    for width in tab_widths.iter().copied() {
        positions.push(current_x);
        current_x += width;
    }

    positions
}

pub(crate) fn update_tab_drag_state<T: TabDragEntry>(
    tabs: &[T],
    dragging: &mut Option<TabDragState>,
    layout: &TabDragLayout,
    dragged_tab_id: TabId,
    pointer_x: Pixels,
    visual_drag_bounds: Option<(Pixels, Pixels)>,
) -> Option<TabDragUpdate> {
    let from_index = tabs
        .iter()
        .position(|entry| entry.tab_id() == dragged_tab_id)?;
    let tab_left_x = layout.position_for(dragged_tab_id)?;
    let tab_width = layout.width_for(dragged_tab_id)?;

    if dragging
        .as_ref()
        .is_none_or(|drag_state| drag_state.tab_id != dragged_tab_id)
    {
        *dragging = Some(TabDragState {
            tab_id: dragged_tab_id,
            pointer_offset_x: (pointer_x - tab_left_x).clamp(px(0.), tab_width),
            visual_left_x: tab_left_x,
            drag_left_x: tab_left_x,
            last_reorder_left_x: tab_left_x,
            autoscroll_pointer_x: None,
            autoscroll_last_update: Instant::now(),
        });
    }

    let drag_state = dragging.as_mut()?;
    let drag_min_x = layout.padding_left;
    let mut drag_max_x = layout.padding_left;

    for entry in tabs {
        let tab_id = entry.tab_id();
        if tab_id == dragged_tab_id {
            continue;
        }
        drag_max_x += layout.width_for(tab_id)?;
    }

    let drag_left_x = (pointer_x - drag_state.pointer_offset_x).clamp(drag_min_x, drag_max_x);
    drag_state.drag_left_x = drag_left_x;
    drag_state.visual_left_x = if let Some((visible_min_x, visible_max_x)) = visual_drag_bounds {
        drag_left_x.clamp(visible_min_x, visible_max_x)
    } else {
        drag_left_x
    };

    let to_index = tab_drag_target_index(
        tabs,
        layout,
        dragged_tab_id,
        drag_state.drag_left_x,
        layout.padding_left,
    )?;
    let should_reorder = f32::from(drag_state.drag_left_x - drag_state.last_reorder_left_x).abs()
        >= f32::from(TAB_DRAG_REORDER_THRESHOLD);

    if should_reorder {
        drag_state.last_reorder_left_x = drag_state.drag_left_x;
    }

    Some(TabDragUpdate {
        from_index,
        to_index,
        should_reorder,
    })
}

pub fn tab_drag_autoscroll_offset_x(
    pointer_viewport_x: Pixels,
    viewport_width: Pixels,
    current_offset_x: Pixels,
    max_offset_x: Pixels,
    elapsed: Duration,
) -> Option<Pixels> {
    let max_offset = f32::from(max_offset_x).max(0.0);
    if max_offset <= 0.0 {
        return None;
    }

    let viewport_width = f32::from(viewport_width).max(0.0);
    let pointer_x = f32::from(pointer_viewport_x).clamp(0.0, viewport_width);
    let edge_width = f32::from(TAB_DRAG_AUTOSCROLL_EDGE_WIDTH).min(viewport_width / 2.0);
    if edge_width <= 0.0 {
        return None;
    }

    let edge_distance = if pointer_x < edge_width {
        edge_width - pointer_x
    } else if pointer_x > viewport_width - edge_width {
        viewport_width - edge_width - pointer_x
    } else {
        0.0
    };
    let velocity = f32::from(TAB_DRAG_AUTOSCROLL_MAX_SPEED) * edge_distance / edge_width;
    if velocity.abs() < 0.5 {
        return None;
    }

    let current_x = f32::from(current_offset_x);
    let next_x = (current_x + velocity * elapsed.as_secs_f32()).clamp(-max_offset, 0.0);
    (next_x != current_x).then_some(px(next_x))
}

fn tab_drag_target_index<T: TabDragEntry>(
    tabs: &[T],
    layout: &TabDragLayout,
    dragged_tab_id: TabId,
    drag_left_x: Pixels,
    padding_left: Pixels,
) -> Option<usize> {
    let mut candidate_x = padding_left;
    let mut best_index = 0;
    let mut best_distance = f32::MAX;
    let mut candidate_index = 0;

    tab_drag_consider_candidate(
        candidate_index,
        candidate_x,
        drag_left_x,
        &mut best_index,
        &mut best_distance,
    );

    for entry in tabs {
        let tab_id = entry.tab_id();
        if tab_id == dragged_tab_id {
            continue;
        }

        let width = layout.width_for(tab_id)?;
        candidate_x += width;
        candidate_index += 1;
        tab_drag_consider_candidate(
            candidate_index,
            candidate_x,
            drag_left_x,
            &mut best_index,
            &mut best_distance,
        );
    }

    Some(best_index)
}

fn tab_drag_consider_candidate(
    candidate_index: usize,
    candidate_x: Pixels,
    drag_left_x: Pixels,
    best_index: &mut usize,
    best_distance: &mut f32,
) {
    let distance = f32::from(candidate_x - drag_left_x).abs();
    if distance < *best_distance {
        *best_distance = distance;
        *best_index = candidate_index;
    }
}

pub fn tab_visual_style(entry: &TabEntry, full_width: Pixels, now: Instant) -> TabVisualStyle {
    let reorder_offset_x = reorder_offset_x(entry, now);

    match entry.phase {
        TabAnimationPhase::Opening { start, duration } => {
            let progress = tab_animation_progress(start, duration, now);
            TabVisualStyle {
                slot_width: full_width,
                surface_offset_x: reorder_offset_x + TAB_OPEN_REVEAL_SHIFT * (1.0 - progress),
                surface_opacity: progress,
            }
        }
        TabAnimationPhase::Open => TabVisualStyle {
            slot_width: full_width,
            surface_offset_x: reorder_offset_x,
            surface_opacity: 1.0,
        },
        TabAnimationPhase::Closing { start, duration } => {
            let progress = tab_animation_progress(start, duration, now);
            TabVisualStyle {
                slot_width: full_width,
                surface_offset_x: reorder_offset_x,
                surface_opacity: 1.0 - progress,
            }
        }
    }
}

pub fn tab_visual_position(entry: &TabEntry, layout_x: Pixels, now: Instant) -> Pixels {
    layout_x + reorder_offset_x(entry, now)
}

pub fn offset_is_effectively_zero(offset_x: Pixels) -> bool {
    f32::from(offset_x).abs() < 0.5
}

pub fn render_tab_visual(
    profile_icon: ProfileIconKind,
    title: &str,
    is_active: bool,
    is_hovered: bool,
    show_separator: bool,
    visual_style: TabVisualStyle,
    full_width: Pixels,
    metrics: TitleBarMetrics,
    palette: TitleBarPalette,
    close_button: AnyElement,
) -> AnyElement {
    let paint_width = if is_active {
        full_width + ACTIVE_TAB_SHOULDER_WIDTH * 2.0
    } else {
        full_width
    };
    let paint_offset_x = if is_active {
        visual_style.surface_offset_x - ACTIVE_TAB_SHOULDER_WIDTH
    } else {
        visual_style.surface_offset_x
    };

    let background_layer = canvas(
        |_bounds, _window, _cx| (),
        move |bounds, (), window, _cx: &mut App| {
            paint_tab_background(bounds, is_active, is_hovered, palette, window);
        },
    )
    .absolute()
    .top(px(0.))
    .left(paint_offset_x)
    .w(paint_width)
    .h(metrics.tab_surface_height)
    .opacity(visual_style.surface_opacity);

    let separator_layer = show_separator.then(|| {
        div()
            .absolute()
            .top(TAB_SEPARATOR_INSET_Y)
            .left(visual_style.surface_offset_x + full_width - TAB_SEPARATOR_WIDTH)
            .w(TAB_SEPARATOR_WIDTH)
            .h(metrics.tab_surface_height - TAB_SEPARATOR_INSET_Y * 2.0)
            .bg(palette.tab_separator)
            .opacity(visual_style.surface_opacity)
    });

    div()
        .relative()
        .w(visual_style.slot_width)
        .h_full()
        .child(background_layer)
        .when_some(separator_layer, |tab, separator| tab.child(separator))
        .child(
            div()
                .relative()
                .left(visual_style.surface_offset_x)
                .w(full_width)
                .h(metrics.tab_surface_height)
                .px(TAB_HORIZONTAL_PADDING_LEFT)
                .pr(TAB_HORIZONTAL_PADDING_RIGHT)
                .flex()
                .items_center()
                .gap(TAB_LABEL_GAP)
                .opacity(visual_style.surface_opacity)
                .child(
                    div()
                        .flex_grow()
                        .flex()
                        .items_center()
                        .gap(TAB_ICON_LABEL_GAP)
                        .min_w_0()
                        .child(render_profile_icon(profile_icon))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.))
                                .text_color(if is_active {
                                    palette.active_tab_foreground
                                } else {
                                    palette.inactive_tab_foreground
                                })
                                .font_weight(if is_active {
                                    FontWeight::SEMIBOLD
                                } else {
                                    FontWeight::MEDIUM
                                })
                                .child(title.to_string()),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .w(TAB_CLOSE_BUTTON_SLOT_WIDTH)
                        .h(TAB_CLOSE_BUTTON_SIZE)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(close_button),
                ),
        )
        .into_any_element()
}

pub fn render_tab_close_button(is_active: bool, is_closing: bool) -> Div {
    if is_closing {
        div()
            .flex_none()
            .w(TAB_CLOSE_BUTTON_SIZE)
            .h(TAB_CLOSE_BUTTON_SIZE)
    } else {
        let foreground: Hsla = if is_active {
            rgba(ACTIVE_CLOSE_BUTTON_FOREGROUND).into()
        } else {
            rgba(INACTIVE_CLOSE_BUTTON_FOREGROUND).into()
        };

        div()
            .flex_none()
            .w(TAB_CLOSE_BUTTON_SIZE)
            .h(TAB_CLOSE_BUTTON_SIZE)
            .rounded(TAB_BUTTON_RADIUS)
            .flex()
            .items_center()
            .justify_center()
            .text_size(TAB_CLOSE_GLYPH_SIZE)
            .text_color(foreground)
            .child("\u{2715}")
    }
}

pub fn render_new_tab_button(palette: TitleBarPalette, metrics: TitleBarMetrics) -> Div {
    div()
        .w(metrics.new_tab_button_size)
        .h(metrics.new_tab_button_size)
        .rounded(TAB_BUTTON_RADIUS)
        .font_family(windows_symbol_font())
        .flex()
        .items_center()
        .justify_center()
        .text_size(NEW_TAB_GLYPH_SIZE)
        .text_color(palette.inactive_tab_foreground)
        .child("\u{e710}")
}

pub fn render_tab_scroll_button(
    direction: TabScrollDirection,
    enabled: bool,
    palette: TitleBarPalette,
) -> Div {
    let glyph = match direction {
        TabScrollDirection::Left => "\u{e76b}",
        TabScrollDirection::Right => "\u{e76c}",
    };

    div()
        .w(TAB_SCROLL_BUTTON_SIZE)
        .h(px(24.))
        .rounded(px(4.))
        .font_family(windows_symbol_font())
        .flex()
        .items_center()
        .justify_center()
        .text_size(TAB_SCROLL_GLYPH_SIZE)
        .text_color(if enabled {
            palette.inactive_tab_foreground
        } else {
            blend_color(palette.inactive_tab_foreground, rgba(0xffffff80).into())
        })
        .opacity(if enabled { 1.0 } else { 0.55 })
        .child(glyph)
}

pub fn tab_scroll_target_offset(
    handle: &ScrollHandle,
    direction: TabScrollDirection,
) -> Option<Pixels> {
    let max_offset_x = f32::from(handle.max_offset().x);
    if max_offset_x <= 0.0 {
        return None;
    }

    let current_offset = handle.offset();
    let current_x = f32::from(current_offset.x);
    let next_x = match direction {
        TabScrollDirection::Left => {
            (current_x + f32::from(TAB_SCROLL_AMOUNT)).clamp(-max_offset_x, 0.0)
        }
        TabScrollDirection::Right => {
            (current_x - f32::from(TAB_SCROLL_AMOUNT)).clamp(-max_offset_x, 0.0)
        }
    };

    if (next_x - current_x).abs() < 0.5 {
        None
    } else {
        Some(px(next_x))
    }
}

pub fn scroll_tab_into_view(
    handle: &ScrollHandle,
    active_index: usize,
    tab_count: usize,
) -> TabScrollUpdate {
    if active_index >= tab_count {
        return TabScrollUpdate::Applied;
    }

    if active_index == 0 {
        let current_offset = handle.offset();
        handle.set_offset(point(px(0.), current_offset.y));
        return TabScrollUpdate::Applied;
    }

    if active_index + 1 == tab_count {
        let current_offset = handle.offset();
        handle.set_offset(point(-handle.max_offset().x, current_offset.y));
        return TabScrollUpdate::Applied;
    }

    handle.scroll_to_item(active_index);
    TabScrollUpdate::Deferred
}

fn render_profile_icon(profile_icon: ProfileIconKind) -> impl IntoElement {
    let (background, glyph, foreground): (Hsla, &str, Hsla) = match profile_icon {
        ProfileIconKind::PowerShell => (rgba(0x2f7df6ff).into(), ">", rgba(0xfffffff2).into()),
        ProfileIconKind::CommandPrompt => (rgba(0x2f2f2fff).into(), "_", rgba(0xfffffff2).into()),
        ProfileIconKind::Linux => (rgba(0x2f8f57ff).into(), ">", rgba(0xfffffff2).into()),
        ProfileIconKind::Terminal => (rgba(0x5a5a5aff).into(), ">", rgba(0xfffffff2).into()),
    };

    div()
        .flex_none()
        .w(TAB_ICON_SIZE)
        .h(TAB_ICON_SIZE)
        .rounded(px(3.))
        .bg(background)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(9.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(foreground)
        .child(glyph)
}

fn paint_tab_background(
    bounds: Bounds<Pixels>,
    is_active: bool,
    is_hovered: bool,
    palette: TitleBarPalette,
    window: &mut Window,
) {
    if is_active {
        let body_bounds = active_tab_body_bounds(bounds);
        window.paint_quad(
            fill(body_bounds, palette.active_tab_background)
                .corner_radii(active_tab_body_corners()),
        );

        if let Some(path) = build_active_tab_shoulder_path(bounds, TabShoulderSide::Left) {
            window.paint_path(path, Background::from(palette.active_tab_background));
        }
        if let Some(path) = build_active_tab_shoulder_path(bounds, TabShoulderSide::Right) {
            window.paint_path(path, Background::from(palette.active_tab_background));
        }

        // WinUI draws these tiny border-brush arcs over the selected tab shoulder.
        // They cover fringe pixels at the titlebar baseline and make the shoulder
        // read like part of the surrounding tab-view chrome rather than a blob.
        if let Some(path) = build_radius_render_arc(bounds, TabShoulderSide::Left) {
            window.paint_path(path, Background::from(palette.tab_border_brush));
        }
        if let Some(path) = build_radius_render_arc(bounds, TabShoulderSide::Right) {
            window.paint_path(path, Background::from(palette.tab_border_brush));
        }
    } else {
        // Inactive Tab
        if is_hovered {
            window.paint_quad(
                fill(
                    Bounds::new(
                        point(
                            bounds.origin.x + INACTIVE_TAB_HOVER_INSET_X,
                            bounds.origin.y,
                        ),
                        size(
                            (bounds.size.width - INACTIVE_TAB_HOVER_INSET_X * 2.0).max(px(0.)),
                            (bounds.size.height - INACTIVE_TAB_HOVER_INSET_BOTTOM).max(px(0.)),
                        ),
                    ),
                    palette.inactive_tab_hover_background,
                )
                .corner_radii(Corners::all(TAB_RADIUS)),
            );
        } else {
            window.paint_quad(
                fill(bounds, palette.inactive_tab_background)
                    .corner_radii(Corners::all(TAB_RADIUS)),
            );
        }
    }
}

fn active_tab_body_bounds(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    let width = (bounds.size.width - ACTIVE_TAB_SHOULDER_WIDTH * 2.0).max(px(0.));
    Bounds::new(
        point(bounds.origin.x + ACTIVE_TAB_SHOULDER_WIDTH, bounds.origin.y),
        size(width, bounds.size.height),
    )
}

fn active_tab_body_corners() -> Corners<Pixels> {
    Corners {
        top_left: TAB_RADIUS,
        top_right: TAB_RADIUS,
        bottom_right: px(0.),
        bottom_left: px(0.),
    }
}

fn path_builder_fill() -> PathBuilder {
    PathBuilder::fill().with_style(PathStyle::Fill(
        FillOptions::default().with_tolerance(ACTIVE_TAB_PATH_TOLERANCE),
    ))
}

fn build_active_tab_shoulder_path(
    bounds: Bounds<Pixels>,
    side: TabShoulderSide,
) -> Option<gpui::Path<Pixels>> {
    let mut builder = path_builder_fill();
    let edge = match side {
        TabShoulderSide::Left => bounds.origin.x,
        TabShoulderSide::Right => bounds.top_right().x,
    };
    let shoulder_width = ACTIVE_TAB_SHOULDER_WIDTH;
    let shoulder_height = ACTIVE_TAB_SHOULDER_HEIGHT;
    let bottom = bounds.bottom_left().y;

    match side {
        TabShoulderSide::Left => {
            builder.move_to(point(edge, bottom));
            builder.arc_to(
                point(shoulder_width, shoulder_height),
                px(0.),
                false,
                false,
                point(edge + shoulder_width, bottom - shoulder_height),
            );
            builder.line_to(point(edge + shoulder_width, bottom));
        }
        TabShoulderSide::Right => {
            builder.move_to(point(edge - shoulder_width, bottom - shoulder_height));
            builder.arc_to(
                point(shoulder_width, shoulder_height),
                px(0.),
                false,
                false,
                point(edge, bottom),
            );
            builder.line_to(point(edge - shoulder_width, bottom));
        }
    }
    builder.close();

    builder.build().ok()
}

fn radius_arc_point(
    edge: Pixels,
    top: Pixels,
    side: TabShoulderSide,
    x: Pixels,
    y: Pixels,
) -> gpui::Point<Pixels> {
    match side {
        TabShoulderSide::Left => point(edge + x, top + y),
        TabShoulderSide::Right => point(edge - x, top + y),
    }
}

fn build_radius_render_arc(
    bounds: Bounds<Pixels>,
    side: TabShoulderSide,
) -> Option<gpui::Path<Pixels>> {
    let mut builder = path_builder_fill();
    let edge = match side {
        TabShoulderSide::Left => bounds.origin.x,
        TabShoulderSide::Right => bounds.top_right().x,
    };
    let top = bounds.bottom_left().y - ACTIVE_TAB_SHOULDER_HEIGHT;

    builder.move_to(radius_arc_point(edge, top, side, px(4.), px(0.)));
    builder.cubic_bezier_to(
        radius_arc_point(edge, top, side, px(2.64582), px(3.)),
        radius_arc_point(edge, top, side, px(4.), px(1.19469)),
        radius_arc_point(edge, top, side, px(3.47624), px(2.26706)),
    );
    builder.line_to(radius_arc_point(edge, top, side, px(0.), px(3.)));
    builder.line_to(radius_arc_point(edge, top, side, px(3.), px(3.)));
    builder.cubic_bezier_to(
        radius_arc_point(edge, top, side, px(3.), px(0.)),
        radius_arc_point(edge, top, side, px(1.34315), px(3.)),
        radius_arc_point(edge, top, side, px(3.), px(1.65685)),
    );
    builder.line_to(radius_arc_point(edge, top, side, px(4.), px(0.)));
    builder.close();

    builder.build().ok()
}

#[derive(Clone, Copy)]
pub enum TabScrollDirection {
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TabScrollUpdate {
    Applied,
    Deferred,
}

fn contrast_foreground(background: Hsla) -> Hsla {
    let background_rgba: gpui::Rgba = background.into();
    let luminance =
        0.2126 * background_rgba.r + 0.7152 * background_rgba.g + 0.0722 * background_rgba.b;

    if luminance > 0.58 {
        rgba(0x161616e4).into()
    } else {
        rgba(0xfffffff2).into()
    }
}

fn blend_color(base: Hsla, overlay: Hsla) -> Hsla {
    let base_rgba: gpui::Rgba = base.into();
    let overlay_rgba: gpui::Rgba = overlay.into();
    base_rgba.blend(overlay_rgba).into()
}

fn reorder_offset_x(entry: &TabEntry, now: Instant) -> Pixels {
    let Some(reorder_animation) = entry.reorder_animation else {
        return px(0.);
    };

    let progress = tab_animation_progress(reorder_animation.start, TAB_REORDER_DURATION, now);
    reorder_animation.offset_x * (1.0 - progress)
}

pub(crate) fn tab_animation_progress(start: Instant, duration: Duration, now: Instant) -> f32 {
    let elapsed = now.duration_since(start).as_secs_f32();
    let duration_s = duration.as_secs_f32();
    let raw_progress = if duration_s == 0.0 {
        1.0
    } else {
        (elapsed / duration_s).clamp(0.0, 1.0)
    };

    quint_ease_out(raw_progress)
}

fn quint_ease_out(delta: f32) -> f32 {
    1.0 - (1.0 - delta).powi(5)
}

#[cfg(test)]
mod tests {
    use super::{
        TabDragLayout, tab_drag_autoscroll_offset_x, tab_drag_target_index, tab_layout_positions,
        tab_strip_layout_plan, tab_target_width, update_tab_drag_state,
    };
    use crate::types::TabId;
    use gpui::px;
    use std::time::Duration;

    #[test]
    fn tab_widths_shrink_to_minimum_before_scrolling() {
        assert_eq!(tab_target_width(3, px(360.), false), px(120.));
        assert_eq!(tab_target_width(3, px(240.), false), px(100.));
        assert_eq!(tab_target_width(3, px(240.), true), px(100.));
    }

    #[test]
    fn tab_strip_becomes_scrollable_when_min_widths_do_not_fit() {
        let plan = tab_strip_layout_plan(5, px(650.), px(45.));

        assert!(plan.scrollable);
        assert_eq!(plan.tab_strip_width, px(420.));
        assert_eq!(plan.tabs_available_width, px(352.));
    }

    #[test]
    fn tab_drag_target_index_ignores_dragged_tab_slot() {
        let first = TabId::new();
        let second = TabId::new();
        let third = TabId::new();
        let tabs = vec![first, second, third];
        let layout = drag_layout(&tabs, vec![px(100.), px(100.), px(100.)], px(4.));

        let target_index = tab_drag_target_index(&tabs, &layout, second, px(215.), px(4.));

        assert_eq!(target_index, Some(2));
    }

    #[test]
    fn tab_drag_target_index_handles_unequal_widths() {
        let first = TabId::new();
        let second = TabId::new();
        let third = TabId::new();
        let tabs = vec![first, second, third];
        let layout = drag_layout(&tabs, vec![px(140.), px(80.), px(120.)], px(4.));

        let target_index = tab_drag_target_index(&tabs, &layout, third, px(10.), px(4.));

        assert_eq!(target_index, Some(0));
    }

    #[test]
    fn tab_drag_autoscroll_moves_toward_right_edge() {
        let next_offset = tab_drag_autoscroll_offset_x(
            px(340.),
            px(360.),
            px(-100.),
            px(500.),
            Duration::from_millis(100),
        );

        assert!(next_offset.is_some_and(|offset| offset < px(-100.)));
    }

    #[test]
    fn tab_drag_autoscroll_stops_at_left_edge() {
        let next_offset = tab_drag_autoscroll_offset_x(
            px(4.),
            px(360.),
            px(0.),
            px(500.),
            Duration::from_millis(100),
        );

        assert_eq!(next_offset, None);
    }

    #[test]
    fn tab_drag_content_position_keeps_reordering_when_visual_position_is_pinned() {
        let first = TabId::new();
        let second = TabId::new();
        let third = TabId::new();
        let fourth = TabId::new();
        let fifth = TabId::new();
        let tabs = vec![first, second, third, fourth, fifth];
        let layout = drag_layout(
            &tabs,
            vec![px(100.), px(100.), px(100.), px(100.), px(100.)],
            px(4.),
        );
        let mut dragging = None;

        update_tab_drag_state(&tabs, &mut dragging, &layout, second, px(114.), None);
        let update = update_tab_drag_state(
            &tabs,
            &mut dragging,
            &layout,
            second,
            px(650.),
            Some((px(4.), px(304.))),
        )
        .expect("drag update");

        let drag_state = dragging.expect("drag state");
        assert_eq!(drag_state.drag_left_x, px(404.));
        assert_eq!(drag_state.visual_left_x, px(304.));
        assert_eq!(update.to_index, 4);
    }

    fn drag_layout(
        tab_ids: &[TabId],
        widths: Vec<gpui::Pixels>,
        padding_left: gpui::Pixels,
    ) -> TabDragLayout {
        let mut positions = tab_layout_positions(&widths);
        for position in &mut positions {
            *position += padding_left;
        }

        TabDragLayout {
            tab_ids: tab_ids.to_vec(),
            widths,
            positions,
            padding_left,
        }
    }
}
