use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, CompositionSlot, DefiniteLength, DevicePixels, Element, ElementId, Entity,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId, Length,
    Pixels, Style, Window, point, size,
};
use terminal::TerminalSession;

use crate::gpu::RendererCellMetrics;

/// The layout state produced by prepaint, consumed by paint.
pub struct LayoutState {
    /// Retained to keep the hitbox alive for GPUI's hit-test ordering.
    _hitbox: Hitbox,
}

/// State persisted across frames via with_element_state.
struct TerminalElementState {
    /// Grid dimensions at last prepaint.
    last_cell_width_px: u32,
    last_cell_height_px: u32,
    last_device_bounds: Bounds<DevicePixels>,
}

/// The GPUI Element that renders the terminal surface.
pub struct TerminalElement {
    session: Entity<TerminalSession>,
    element_id: ElementId,
    /// Written during prepaint so `TerminalView` can read the surface bounds for
    /// mouse-coordinate conversion and for publishing scroll info to the scrollbar.
    bounds_out: Rc<Cell<Option<Bounds<Pixels>>>>,
    composition_slot: CompositionSlot,
    /// Renderer-authoritative metrics pushed from the renderer thread.
    cell_metrics: Option<RendererCellMetrics>,
}

impl TerminalElement {
    pub fn new(
        session: Entity<TerminalSession>,
        element_id: ElementId,
        bounds_out: Rc<Cell<Option<Bounds<Pixels>>>>,
        composition_slot: CompositionSlot,
        cell_metrics: Option<RendererCellMetrics>,
    ) -> Self {
        Self {
            session,
            element_id,
            bounds_out,
            composition_slot,
            cell_metrics,
        }
    }

    fn fallback_metrics(font_size: f32) -> RendererCellMetrics {
        RendererCellMetrics {
            cell_width: (font_size * 0.6).max(1.0),
            line_height: (font_size * 1.3).max(1.0),
        }
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = LayoutState;

    fn id(&self) -> Option<ElementId> {
        Some(self.element_id.clone())
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
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = Length::Definite(DefiniteLength::Fraction(1.0));
        style.size.height = Length::Definite(DefiniteLength::Fraction(1.0));
        let layout_id = window.request_layout(style, None, cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.bounds_out.set(Some(bounds));
        let render_config = {
            let entity = self.session.read(cx).render_config().clone();
            entity.read(cx).clone()
        };

        let session = self.session.clone();
        let renderer_metrics = self.cell_metrics;

        let build_layout = |prev_state: Option<TerminalElementState>, window: &mut Window| {
            let metrics = renderer_metrics
                .filter(|m| m.cell_width > 0.0 && m.line_height > 0.0)
                .unwrap_or_else(|| Self::fallback_metrics(render_config.font_size));

            let scale_factor = window.scale_factor();
            let device_bounds = Bounds::new(
                point(
                    DevicePixels((f32::from(bounds.origin.x) * scale_factor).round() as i32),
                    DevicePixels((f32::from(bounds.origin.y) * scale_factor).round() as i32),
                ),
                size(
                    DevicePixels((f32::from(bounds.size.width) * scale_factor).round() as i32),
                    DevicePixels((f32::from(bounds.size.height) * scale_factor).round() as i32),
                ),
            );
            let bounds_changed = prev_state
                .as_ref()
                .is_none_or(|s| s.last_device_bounds != device_bounds);
            if bounds_changed {
                self.composition_slot.set_bounds(device_bounds);
            }

            let cell_w = (metrics.cell_width.max(1.0) * scale_factor).round() as u32;
            let cell_h = (metrics.line_height.max(1.0) * scale_factor).round() as u32;

            let size_changed = prev_state
                .as_ref()
                .is_none_or(|s| s.last_device_bounds.size != device_bounds.size);
            let metrics_changed = prev_state
                .as_ref()
                .is_none_or(|s| s.last_cell_width_px != cell_w || s.last_cell_height_px != cell_h);
            if size_changed || metrics_changed {
                session.update(cx, |s, _cx| {
                    s.request_resize(
                        device_bounds.size.width.0 as u32,
                        device_bounds.size.height.0 as u32,
                        cell_w,
                        cell_h,
                    );
                });
            }

            let layout = LayoutState { _hitbox: hitbox };
            let new_state = TerminalElementState {
                last_cell_width_px: cell_w,
                last_cell_height_px: cell_h,
                last_device_bounds: device_bounds,
            };

            (layout, new_state)
        };

        if let Some(id) = id {
            // Closure required: with_element_state needs a specific closure signature
            // that doesn't match build_layout's inferred types when passed directly.
            #[allow(clippy::redundant_closure)]
            window.with_element_state(id, |prev_state, window| build_layout(prev_state, window))
        } else {
            build_layout(None, window).0
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _layout: &mut Self::PrepaintState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
