use std::time::{Duration, Instant};

use gpui::{
    App, BorderStyle, Bounds, Context, Corners, CursorStyle, DispatchPhase, Edges, Element,
    ElementId, Entity, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId,
    IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, Position, Render, Size, Style, Task, Window, px, quad, relative, size,
};
use terminal::ScrollbarInfo;
use terminal::TerminalSession;

/// Delay before the scrollbar starts fading out after the last scroll event.
const HIDE_DELAY: Duration = Duration::from_millis(800);
/// Duration of the fade-out animation.
const FADE_OUT_DURATION: Duration = Duration::from_millis(400);
/// Duration of the fade-in animation.
const FADE_IN_DURATION: Duration = Duration::from_millis(50);
/// Duration of the widening animation on hover.
const WIDEN_DURATION: Duration = Duration::from_millis(100);
/// Duration of the narrowing animation when hover exits.
const NARROW_DURATION: Duration = Duration::from_millis(200);

/// Width of the thin scrollbar when not interacting.
const THIN_WIDTH: f32 = 3.0;
/// Width of the scrollbar when hovered or dragging.
const WIDE_WIDTH: f32 = 8.0;
/// Minimum thumb height so the scrollbar is always grabbable.
const MIN_THUMB_HEIGHT: Pixels = px(20.0);
/// Padding from the right edge of the terminal surface.
const RIGHT_PADDING: Pixels = px(4.0);
/// Padding from the top and bottom edges of the track.
const VERTICAL_PADDING: Pixels = px(4.0);
/// Horizontal width of the hover detection zone (left of the right edge).
const HOVER_ZONE_WIDTH: Pixels = px(16.0);

/// Thumb color (normal state, semi-transparent).
const THUMB_COLOR: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 0.7,
    a: 0.40,
};
/// Thumb color when hovered.
const THUMB_HOVER_COLOR: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 0.75,
    a: 0.55,
};
/// Thumb color when being dragged.
const THUMB_DRAG_COLOR: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 0.80,
    a: 0.70,
};

/// A value that linearly interpolates from `from` to `to` over `duration`.
/// Call `current()` each frame. Returns `true` from `is_animating` while
/// the animation is still running.
#[derive(Clone, Copy)]
struct AnimatedFloat {
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
}

impl AnimatedFloat {
    fn immediate(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            start: Instant::now(),
            duration: Duration::ZERO,
        }
    }

    fn transition(from: f32, to: f32, duration: Duration) -> Self {
        Self {
            from,
            to,
            start: Instant::now(),
            duration,
        }
    }

    /// Current interpolated value.
    fn current(&self) -> f32 {
        if self.duration.is_zero() {
            return self.to;
        }
        let elapsed = self.start.elapsed().as_secs_f32();
        let t = (elapsed / self.duration.as_secs_f32()).clamp(0.0, 1.0);
        self.from + (self.to - self.from) * t
    }

    fn is_animating(&self) -> bool {
        !self.duration.is_zero() && self.start.elapsed() < self.duration
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum InteractionState {
    Inactive,
    Hovered,
    /// Grab offset = distance from thumb top to click position (in pixels).
    Dragging {
        grab_offset: Pixels,
    },
}

impl InteractionState {
    fn is_dragging(self) -> bool {
        matches!(self, InteractionState::Dragging { .. })
    }
}

/// Persistent scrollbar state that lives as a GPUI entity.
///
/// `TerminalView` holds `Entity<ScrollbarState>` and forwards scroll events.
/// `ScrollbarElement` holds the same entity and handles all mouse interaction.
pub struct ScrollbarState {
    session: Entity<TerminalSession>,

    /// Current opacity animation (0.0 = hidden, 1.0 = fully visible).
    opacity_anim: AnimatedFloat,
    /// Current width animation (THIN_WIDTH..WIDE_WIDTH).
    width_anim: AnimatedFloat,

    interaction: InteractionState,

    /// Pending auto-hide task. Dropped (and thus cancelled) when a newer one is scheduled
    /// or when the scrollbar is interacted with. Dropping a GPUI `Task` cancels it.
    hide_task: Option<Task<()>>,

    /// Most-recent scrollbar geometry, updated each frame by `TerminalView`.
    scrollbar_info: ScrollbarInfo,
}

impl ScrollbarState {
    pub fn new(session: Entity<TerminalSession>) -> Self {
        Self {
            session,
            opacity_anim: AnimatedFloat::immediate(0.0),
            width_anim: AnimatedFloat::immediate(THIN_WIDTH),
            interaction: InteractionState::Inactive,
            hide_task: None,
            scrollbar_info: ScrollbarInfo::default(),
        }
    }

    // --- Public API (used by TerminalView) ---

    /// Update the scrollbar geometry snapshot. Called every frame from `TerminalView::render`.
    pub fn sync_snapshot(&mut self, info: ScrollbarInfo) {
        self.scrollbar_info = info;
    }

    /// Call whenever the viewport is scrolled (wheel, keyboard, etc.).
    pub fn on_scroll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show(cx);
        self.schedule_hide(window, cx);
    }

    /// Whether the scrollbar thumb is currently being dragged.
    /// `TerminalView` checks this to suppress selection logic during drag.
    pub fn is_dragging(&self) -> bool {
        self.interaction.is_dragging()
    }

    // --- Visibility ---

    /// Fade the scrollbar in from its current opacity.
    fn show(&mut self, cx: &mut Context<Self>) {
        self.animate_opacity_to(1.0, FADE_IN_DURATION, cx);
    }

    fn start_fade_out(&mut self, cx: &mut Context<Self>) {
        self.animate_opacity_to(0.0, FADE_OUT_DURATION, cx);
    }

    /// Drop the pending hide task (cancels it) and spawn a new one.
    fn schedule_hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Dropping the old Task cancels it — no generation counter needed.
        self.hide_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(HIDE_DELAY).await;
            let _ = this.update(cx, |state, cx| {
                if !state.interaction.is_dragging() {
                    state.start_fade_out(cx);
                }
            });
        }));
    }

    fn cancel_hide(&mut self) {
        self.hide_task.take();
    }

    // --- Hover ---

    fn set_hovered(&mut self, hovered: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.interaction.is_dragging() {
            return;
        }

        let new_state = if hovered {
            InteractionState::Hovered
        } else {
            InteractionState::Inactive
        };

        if self.interaction == new_state {
            return;
        }

        self.interaction = new_state;

        if hovered {
            self.animate_width_to(WIDE_WIDTH, WIDEN_DURATION);
            self.show(cx);
            self.cancel_hide();
        } else {
            self.animate_width_to(THIN_WIDTH, NARROW_DURATION);
            self.schedule_hide(window, cx);
        }

        cx.notify();
    }

    // --- Drag ---

    fn start_drag(&mut self, grab_offset: Pixels, cx: &mut Context<Self>) {
        self.interaction = InteractionState::Dragging { grab_offset };
        self.cancel_hide();
        self.animate_width_to(WIDE_WIDTH, WIDEN_DURATION);
        cx.notify();
    }

    fn end_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.interaction = InteractionState::Inactive;
        self.animate_width_to(THIN_WIDTH, NARROW_DURATION);
        self.schedule_hide(window, cx);
        cx.notify();
    }

    fn grab_offset(&self) -> Option<Pixels> {
        match self.interaction {
            InteractionState::Dragging { grab_offset } => Some(grab_offset),
            _ => None,
        }
    }

    // --- Animation helpers ---

    fn animate_opacity_to(&mut self, to: f32, duration: Duration, cx: &mut Context<Self>) {
        let from = self.opacity_anim.current();
        if (from - to).abs() > f32::EPSILON {
            self.opacity_anim = AnimatedFloat::transition(from, to, duration);
            cx.notify();
        }
    }

    fn animate_width_to(&mut self, to: f32, duration: Duration) {
        let from = self.width_anim.current();
        self.width_anim = AnimatedFloat::transition(from, to, duration);
    }

    // --- Computed values (used by ScrollbarElement) ---

    fn opacity(&self) -> f32 {
        self.opacity_anim.current()
    }

    fn thumb_width(&self) -> Pixels {
        px(self.width_anim.current())
    }

    fn thumb_color(&self) -> Hsla {
        match self.interaction {
            InteractionState::Dragging { .. } => THUMB_DRAG_COLOR,
            InteractionState::Hovered => THUMB_HOVER_COLOR,
            InteractionState::Inactive => THUMB_COLOR,
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.opacity_anim.is_animating() || self.width_anim.is_animating()
    }

    // --- Geometry ---

    /// Compute the scrollbar thumb geometry from the prepaint `bounds`.
    /// Returns `None` when there is nothing to scroll (viewport covers all content).
    fn compute_layout(&self, bounds: Bounds<Pixels>, width: Pixels) -> Option<ScrollbarLayout> {
        let info = self.scrollbar_info;
        let max_top = info.total_rows.saturating_sub(info.viewport_rows);
        if max_top == 0 {
            return None;
        }

        let track_top = bounds.origin.y + VERTICAL_PADDING;
        let track_bottom = bounds.origin.y + bounds.size.height - VERTICAL_PADDING;
        let track_height = track_bottom - track_top;
        if track_height <= px(0.0) {
            return None;
        }

        let visible_ratio = info.viewport_rows as f32 / info.total_rows as f32;
        let thumb_height = (track_height * visible_ratio).max(MIN_THUMB_HEIGHT);
        if thumb_height >= track_height {
            return None;
        }

        let travel = track_height - thumb_height;
        let thumb_top_offset = travel * (info.top_row as f32 / max_top as f32);

        // Right-anchor: right edge minus padding, left edge minus width.
        let right_edge = bounds.origin.x + bounds.size.width - RIGHT_PADDING;
        let thumb_left = right_edge - width;
        let thumb_origin = Point::new(thumb_left, track_top + thumb_top_offset);
        let thumb_size = Size::new(width, thumb_height);
        let thumb_bounds = Bounds::new(thumb_origin, thumb_size);

        // Track bounds: same width as thumb, full padded height.
        let track_bounds = Bounds::new(
            Point::new(thumb_left, track_top),
            Size::new(width, track_height),
        );

        // Hover zone: wider detection area anchored to the right edge.
        // Intentionally extends the full surface height so hover can wake the scrollbar
        // even when it is fully faded out.
        let hover_zone_left = right_edge - HOVER_ZONE_WIDTH;
        let hover_zone = Bounds::new(
            Point::new(hover_zone_left, bounds.origin.y),
            Size::new(HOVER_ZONE_WIDTH, bounds.size.height),
        );

        Some(ScrollbarLayout {
            thumb_bounds,
            track_bounds,
            hover_zone,
            max_top_row: max_top,
            track_top,
            track_height,
            thumb_height,
        })
    }

    /// Convert a y pixel position + grab offset to a scrollback row.
    fn row_for_pointer_y(
        &self,
        layout: &ScrollbarLayout,
        mouse_y: Pixels,
        grab_offset: Pixels,
    ) -> u64 {
        let thumb_start = (mouse_y - layout.track_top - grab_offset)
            .clamp(px(0.0), layout.track_height - layout.thumb_height);
        let travel = layout.track_height - layout.thumb_height;
        let ratio = if travel > px(0.0) {
            f32::from(thumb_start) / f32::from(travel)
        } else {
            0.0
        };
        (ratio * layout.max_top_row as f32).round() as u64
    }
}

impl Render for ScrollbarState {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ScrollbarElement { state: cx.entity() }
    }
}

/// Per-frame geometry - not persisted
struct ScrollbarLayout {
    thumb_bounds: Bounds<Pixels>,
    track_bounds: Bounds<Pixels>,
    hover_zone: Bounds<Pixels>,
    /// Maximum value of `top_row` (= total_rows - viewport_rows).
    max_top_row: u64,
    track_top: Pixels,
    track_height: Pixels,
    thumb_height: Pixels,
}

/// The GPUI element that paints and handles input for the scrollbar overlay.
/// Renders as an absolute, full-size overlay over the terminal surface.
struct ScrollbarElement {
    state: Entity<ScrollbarState>,
}

struct ScrollbarHitboxes {
    thumb: Hitbox,
    /// Retained to participate in GPUI's hit-test ordering; not queried directly.
    _hover_zone: Hitbox,
}

struct ScrollbarPrepaintState {
    layout: Option<ScrollbarLayout>,
    hitboxes: Option<ScrollbarHitboxes>,
}

impl Element for ScrollbarElement {
    type RequestLayoutState = ();
    type PrepaintState = ScrollbarPrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style {
            position: Position::Absolute,
            inset: Edges::default(),
            size: size(relative(1.), relative(1.)).map(Into::into),
            ..Default::default()
        };
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> ScrollbarPrepaintState {
        let state = self.state.read(cx);
        let width = state.thumb_width();
        let layout = state.compute_layout(bounds, width);

        let hitboxes = layout.as_ref().map(|l| {
            // Thumb hitbox blocks scroll events so the cursor change is clean.
            let thumb =
                window.insert_hitbox(l.thumb_bounds, HitboxBehavior::BlockMouseExceptScroll);
            // Hover zone hitbox participates in hit-test ordering; geometry
            // is checked directly in the mouse-move handler.
            let _hover_zone = window.insert_hitbox(l.hover_zone, HitboxBehavior::Normal);
            ScrollbarHitboxes { thumb, _hover_zone }
        });

        ScrollbarPrepaintState { layout, hitboxes }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        prepaint: &mut ScrollbarPrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(ref layout) = prepaint.layout else {
            return;
        };

        // --- Mouse event handlers ---

        // Paint is skipped below when invisible.
        // Hover zone: detect enter/exit to widen/narrow (and show when faded out).
        window.on_mouse_event({
            let state = self.state.clone();
            let hover_zone = layout.hover_zone;
            move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let in_zone = hover_zone.contains(&event.position);
                state.update(cx, |s, cx| {
                    if !s.interaction.is_dragging() {
                        let currently_hovered = s.interaction == InteractionState::Hovered;
                        if in_zone != currently_hovered {
                            s.set_hovered(in_zone, window, cx);
                        }
                    }
                });
            }
        });

        // Mouse down: start drag or track-click.
        window.on_mouse_event({
            let state = self.state.clone();
            let layout = layout.clone_geometry();
            move |event: &MouseDownEvent, phase, _window, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                    return;
                }
                if !layout.hover_zone.contains(&event.position) {
                    return;
                }

                state.update(cx, |s, cx| {
                    if layout.thumb_bounds.contains(&event.position) {
                        let grab_offset = event.position.y - layout.thumb_bounds.origin.y;
                        s.start_drag(grab_offset, cx);
                    } else if layout.track_bounds.contains(&event.position) {
                        // Track click: jump thumb center to click position.
                        let grab_offset = layout.thumb_height / 2.0;
                        let row = s.row_for_pointer_y(&layout, event.position.y, grab_offset);
                        let session = s.session.clone();
                        session.read(cx).scroll_to_row(row);
                        s.start_drag(grab_offset, cx);
                    }
                });

                cx.stop_propagation();
            }
        });

        // Mouse move while dragging (capture phase so it fires globally).
        window.on_mouse_event({
            let state = self.state.clone();
            let layout = layout.clone_geometry();
            move |event: &MouseMoveEvent, phase, _window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                state.update(cx, |s, cx| {
                    if let Some(grab_offset) = s.grab_offset() {
                        let row = s.row_for_pointer_y(&layout, event.position.y, grab_offset);
                        let session = s.session.clone();
                        session.read(cx).scroll_to_row(row);
                        // Keep visible during drag; do not reschedule the hide timer.
                        s.show(cx);
                        // Notify unconditionally: show() skips notify when already at full
                        // opacity, but the thumb position still needs to re-render.
                        cx.notify();
                    }
                });
            }
        });

        // Mouse up: end drag (capture phase — fires even outside the thumb bounds).
        window.on_mouse_event({
            let state = self.state.clone();
            move |_event: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                state.update(cx, |s, cx| {
                    if s.is_dragging() {
                        s.end_drag(window, cx);
                    }
                });
            }
        });

        // --- Paint (skipped when invisible) ---

        let opacity = self.state.read(cx).opacity();
        if opacity <= 0.0 {
            // Still request animation frames if a fade-in is already in progress.
            if self.state.read(cx).needs_animation_frame() {
                window.request_animation_frame();
            }
            return;
        }

        let state = self.state.read(cx);
        let is_dragging = state.is_dragging();
        let mut color = state.thumb_color();
        color.a *= opacity;

        // Paint rounded thumb quad.
        window.paint_quad(quad(
            layout.thumb_bounds,
            Corners::all(px(f32::from(layout.thumb_bounds.size.width) / 2.0)),
            color,
            Edges::default(),
            Hsla::transparent_black(),
            BorderStyle::default(),
        ));

        // Cursor: Arrow over thumb; window-level Arrow when dragging so it stays
        // even if the mouse leaves the thumb bounds mid-drag.
        if is_dragging {
            window.set_window_cursor_style(CursorStyle::Arrow);
        } else if let Some(ref hb) = prepaint.hitboxes {
            window.set_cursor_style(CursorStyle::Arrow, &hb.thumb);
        }

        if self.state.read(cx).needs_animation_frame() {
            window.request_animation_frame();
        }
    }
}

impl IntoElement for ScrollbarElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl ScrollbarLayout {
    /// Clone the geometry fields needed by event-handler closures.
    /// Avoids deriving `Clone` on the full layout (which also has Pixels fields).
    fn clone_geometry(&self) -> Self {
        Self {
            thumb_bounds: self.thumb_bounds,
            track_bounds: self.track_bounds,
            hover_zone: self.hover_zone,
            max_top_row: self.max_top_row,
            track_top: self.track_top,
            track_height: self.track_height,
            thumb_height: self.thumb_height,
        }
    }
}
