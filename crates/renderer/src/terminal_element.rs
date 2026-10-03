use ghostty::ScreenSize;
use gpui::{
    App, Bounds, DefiniteLength, DispatchPhase, Element, ElementId, ElementInputHandler, Entity,
    ExternalSurfaceId, ExternalSurfaceState, Focusable as _, GlobalElementId, HitboxBehavior,
    HitboxId, InspectorElementId, IntoElement, LayoutId, Length, MouseButton, MouseMoveEvent,
    MouseUpEvent, Pixels, Style, Window,
};

use crate::TerminalView;

/// The layout state produced by prepaint, consumed by paint.
pub struct LayoutState {
    hitbox: HitboxId,
    external_surface_id: ExternalSurfaceId,
}

/// The GPUI Element that represents the terminal surface.
pub struct TerminalElement {
    pub terminal_view: Entity<TerminalView>,
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = LayoutState;

    fn id(&self) -> Option<ElementId> {
        Some(ElementId::View(self.terminal_view.entity_id()))
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
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal).id;

        window.with_element_state(id.unwrap(), |prev_state, window| {
            let scale_factor = window.scale_factor();
            let next_surface_state = ExternalSurfaceState {
                logical_size: bounds.size,
                window_scale_factor: scale_factor,
                occluded: window.content_mask().bounds.intersect(&bounds).is_empty(),
            };

            let surface_changed = prev_state != Some(next_surface_state);

            let screen_size = ScreenSize::new(
                bounds.size.scale(scale_factor).width.0.max(1.0) as u32,
                bounds.size.scale(scale_factor).height.0.max(1.0) as u32,
            )
            .unwrap();

            let external_surface_id = self.terminal_view.update(cx, |view, cx| {
                view.session.surface_bounds = bounds;
                let cell_size = view.update_font(scale_factor, cx);
                view.session.apply_resize(screen_size, cell_size);

                let host = view.host.as_ref().unwrap();
                if surface_changed {
                    host.update_state(next_surface_state);
                }
                host.id
            });

            let layout = LayoutState { hitbox, external_surface_id };
            (layout, next_surface_state)
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        layout: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.terminal_view.focus_handle(cx);
        if focus_handle.is_focused(window) {
            let input_handler = ElementInputHandler::new(bounds, self.terminal_view.clone());
            window.handle_input(&focus_handle, input_handler, cx);
        }

        // Owned left presses continue outside the hitbox, before ordinary bubble listeners.
        let hitbox = layout.hitbox;
        window.on_mouse_event({
            let view = self.terminal_view.clone();
            move |event: &MouseMoveEvent, phase, window, cx| {
                let captured = view.read(cx).owns_left_press();
                let dispatch = match phase {
                    DispatchPhase::Capture => captured,
                    DispatchPhase::Bubble => !captured && hitbox.is_hovered(window),
                };
                if dispatch {
                    view.update(cx, |view, cx| view.handle_mouse_move(event, window, cx));
                }
            }
        });
        window.on_mouse_event({
            let view = self.terminal_view.clone();
            move |event: &MouseUpEvent, phase, window, cx| {
                let captured = event.button == MouseButton::Left && view.read(cx).owns_left_press();
                let dispatch = match phase {
                    DispatchPhase::Capture => captured,
                    DispatchPhase::Bubble => !captured && hitbox.is_hovered(window),
                };
                if dispatch {
                    view.update(cx, |view, cx| view.handle_mouse_up(event, window, cx));
                }
            }
        });
        window.paint_external_surface(layout.external_surface_id, bounds);
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
