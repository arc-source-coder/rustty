use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, CompositionSlot, DefiniteLength, DevicePixels, Element, ElementId, Entity,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId, Length,
    Pixels, Size, Style, Window, point, size,
};
use terminal::TerminalSession;

use crate::gpu::RendererCellMetrics;

/// Grid dimensions derived from bounds + cell metrics.
#[derive(Debug, Clone, Copy)]
pub struct GridDimensions {
    pub cols: u16,
    pub rows: u16,
    pub metrics: RendererCellMetrics,
}

/// The layout state produced by prepaint, consumed by paint.
pub struct LayoutState {
    /// Retained to keep the hitbox alive for GPUI's hit-test ordering.
    _hitbox: Hitbox,
}

/// State persisted across frames via with_element_state.
struct TerminalElementState {
    /// Grid dimensions at last prepaint.
    last_cols: u16,
    last_rows: u16,
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

    /// Compute grid dimensions from available bounds + cell metrics.
    fn compute_grid(bounds_size: Size<Pixels>, metrics: RendererCellMetrics) -> GridDimensions {
        let cols = (f32::from(bounds_size.width) / metrics.cell_width)
            .floor()
            .max(1.0) as u16;
        let rows = (f32::from(bounds_size.height) / metrics.line_height)
            .floor()
            .max(1.0) as u16;
        GridDimensions {
            cols,
            rows,
            metrics,
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

            let grid = Self::compute_grid(bounds.size, metrics);
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

            let size_changed = prev_state
                .as_ref()
                .is_none_or(|s| s.last_cols != grid.cols || s.last_rows != grid.rows);
            if size_changed {
                let cell_w = metrics.cell_width.max(1.0).round() as u16;
                let cell_h = metrics.line_height.max(1.0).round() as u16;
                session.update(cx, |s, _cx| {
                    s.request_resize(grid.cols, grid.rows, cell_w, cell_h);
                });
            }

            let layout = LayoutState { _hitbox: hitbox };
            let new_state = TerminalElementState {
                last_cols: grid.cols,
                last_rows: grid.rows,
                last_device_bounds: device_bounds,
            };

            (layout, new_state)
        };

        if let Some(id) = id {
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
