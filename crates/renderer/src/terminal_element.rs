use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, DefiniteLength, Element, ElementId, Entity, ExternalSurfaceHost,
    ExternalSurfaceState, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement,
    LayoutId, Length, Pixels, Style, Window,
};
use terminal::{TerminalDimensions, TerminalSession};

use crate::gpu::RendererCellMetrics;

/// The layout state produced by prepaint, consumed by paint.
pub struct LayoutState {
    /// Retained to keep the hitbox alive for GPUI's hit-test ordering.
    _hitbox: Hitbox,
}

/// State persisted across frames via with_element_state.
struct TerminalElementState {
    last_dimensions: TerminalDimensions,
    last_surface_state: ExternalSurfaceState,
}

/// The GPUI Element that renders the terminal surface.
pub struct TerminalElement {
    session: Entity<TerminalSession>,
    element_id: ElementId,
    /// Written during prepaint so `TerminalView` can read the surface bounds for
    /// mouse-coordinate conversion and for publishing scroll info to the scrollbar.
    bounds_out: Rc<Cell<Option<Bounds<Pixels>>>>,
    surface_host: ExternalSurfaceHost,
    /// Renderer-authoritative metrics pushed from the renderer thread.
    cell_metrics: Option<RendererCellMetrics>,
}

impl TerminalElement {
    pub fn new(
        session: Entity<TerminalSession>,
        element_id: ElementId,
        bounds_out: Rc<Cell<Option<Bounds<Pixels>>>>,
        surface_host: ExternalSurfaceHost,
        cell_metrics: Option<RendererCellMetrics>,
    ) -> Self {
        Self {
            session,
            element_id,
            bounds_out,
            surface_host,
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
            let content_mask = window.content_mask();
            let clipped_bounds = bounds.intersect(&content_mask.bounds);

            let next_surface_state = ExternalSurfaceState {
                logical_size: bounds.size,
                device_size: bounds.size.to_device_pixels(scale_factor),
                window_scale_factor: scale_factor,
                visible: !clipped_bounds.is_empty(),
            };

            let surface_state_changed = prev_state
                .as_ref()
                .is_none_or(|state| state.last_surface_state != next_surface_state);

            if surface_state_changed {
                self.surface_host.update_state(next_surface_state);
            }

            let dimensions = TerminalDimensions {
                screen_width_px: next_surface_state.device_size.width.0.max(1) as u32,
                screen_height_px: next_surface_state.device_size.height.0.max(1) as u32,
                cell_width_px: (metrics.cell_width.max(1.0) * scale_factor).round() as u32,
                cell_height_px: (metrics.line_height.max(1.0) * scale_factor).round() as u32,
            };

            let dimensions_changed = prev_state
                .as_ref()
                .is_none_or(|state| state.last_dimensions != dimensions);
            if dimensions_changed {
                session.update(cx, |s, _cx| {
                    s.apply_resize(dimensions);
                });
            }

            let layout = LayoutState { _hitbox: hitbox };
            let new_state = TerminalElementState {
                last_dimensions: dimensions,
                last_surface_state: next_surface_state,
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
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _layout: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        window.paint_external_surface(&self.surface_host, bounds);
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
