use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, PromptButton,
    PromptHandle, PromptLevel, PromptResponse, Render, RenderablePromptHandle,
    StatefulInteractiveElement as _, Styled as _, Window, WindowAppearance, actions, div, font, px,
    rgba,
};

const PROMPT_KEY_CONTEXT: &str = "ClosePrompt";
const DEFAULT_ACCENT_COLOR: u32 = 0x005FB8FF;

actions!(
    close_prompt,
    [ActivatePromptButton, DismissPrompt, FocusNextPromptButton, FocusPreviousPromptButton,]
);

/// Register the Fluent-style close prompt renderer as the global prompt builder.
pub fn register(cx: &mut App) {
    cx.set_prompt_builder(render_prompt);
    cx.bind_keys([
        KeyBinding::new("enter", ActivatePromptButton, Some(PROMPT_KEY_CONTEXT)),
        KeyBinding::new("space", ActivatePromptButton, Some(PROMPT_KEY_CONTEXT)),
        KeyBinding::new("escape", DismissPrompt, Some(PROMPT_KEY_CONTEXT)),
        KeyBinding::new("left", FocusPreviousPromptButton, Some(PROMPT_KEY_CONTEXT)),
        KeyBinding::new("right", FocusNextPromptButton, Some(PROMPT_KEY_CONTEXT)),
        KeyBinding::new("tab", FocusNextPromptButton, Some(PROMPT_KEY_CONTEXT)),
        KeyBinding::new("shift-tab", FocusPreviousPromptButton, Some(PROMPT_KEY_CONTEXT)),
    ]);
}

fn render_prompt(
    _level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    actions: &[PromptButton],
    handle: PromptHandle,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    let accent = system_button_accent_color();
    let keyboard_focus_visible = window.last_input_was_keyboard();

    let renderer = cx.new(|cx| {
        let button_focus_handles = (0..actions.len())
            .map(|index| cx.focus_handle().tab_index(index as isize).tab_stop(true))
            .collect::<Vec<_>>();
        let focus = button_focus_handles.first().cloned().unwrap_or_else(|| cx.focus_handle());

        ClosePromptRenderer {
            message: message.to_string(),
            detail: detail.map(String::from),
            actions: actions.to_vec(),
            accent,
            button_focus_handles,
            keyboard_focus_visible,
            focus,
        }
    });

    handle.with_view(renderer, window, cx)
}

#[cfg(target_os = "windows")]
fn system_button_accent_color() -> u32 {
    use windows::UI::ViewManagement::{UIColorType, UISettings};

    UISettings::new()
        .and_then(|settings| {
            // WinUI accent buttons are darker than the raw accent swatch.
            settings
                .GetColorValue(UIColorType::AccentDark1)
                .or_else(|_| settings.GetColorValue(UIColorType::Accent))
        })
        .map(|c| ((c.R as u32) << 24) | ((c.G as u32) << 16) | ((c.B as u32) << 8) | (c.A as u32))
        .unwrap_or(DEFAULT_ACCENT_COLOR)
}

#[cfg(not(target_os = "windows"))]
fn system_button_accent_color() -> u32 {
    DEFAULT_ACCENT_COLOR
}

struct ClosePromptRenderer {
    message: String,
    detail: Option<String>,
    actions: Vec<PromptButton>,
    accent: u32,
    button_focus_handles: Vec<FocusHandle>,
    keyboard_focus_visible: bool,
    focus: FocusHandle,
}

impl EventEmitter<PromptResponse> for ClosePromptRenderer {}

#[derive(Clone, Copy)]
struct PromptPalette {
    overlay: gpui::Hsla,
    surface: gpui::Hsla,
    footer: gpui::Hsla,
    surface_border: gpui::Hsla,
    divider: gpui::Hsla,
    dialog_bottom_edge: gpui::Hsla,
    standard_button: gpui::Hsla,
    standard_button_edge: gpui::Hsla,
    accent_button_edge: gpui::Hsla,
    text_primary: gpui::Hsla,
    text_secondary: gpui::Hsla,
    text_on_accent: gpui::Hsla,
    text_on_accent_pressed: gpui::Hsla,
    focus_outer: gpui::Hsla,
    focus_inner: gpui::Hsla,
    standard_hover: gpui::Hsla,
    standard_active: gpui::Hsla,
}

impl PromptPalette {
    fn for_window(window: &Window) -> Self {
        let is_dark =
            matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);

        if is_dark {
            Self {
                overlay: rgba(0x0000004d).into(),
                surface: rgba(0x2c2c2cff).into(),
                footer: rgba(0x0000001a).into(),
                surface_border: rgba(0xffffff15).into(),
                divider: rgba(0xffffff15).into(),
                dialog_bottom_edge: rgba(0x0000002e).into(),
                standard_button: rgba(0x2d2d2dff).into(),
                standard_button_edge: rgba(0x0000000f).into(),
                accent_button_edge: rgba(0x00000047).into(),
                text_primary: rgba(0xffffffe4).into(),
                text_secondary: rgba(0xffffff9a).into(),
                text_on_accent: rgba(0xffffffff).into(),
                text_on_accent_pressed: rgba(0xffffffb3).into(),
                focus_outer: rgba(0xffffffff).into(),
                focus_inner: rgba(0x000000ff).into(),
                standard_hover: rgba(0x323232ff).into(),
                standard_active: rgba(0xffffff0b).into(),
            }
        } else {
            Self {
                overlay: rgba(0x0000004d).into(),
                surface: rgba(0xffffffff).into(),
                footer: rgba(0x0000000a).into(),
                surface_border: rgba(0x0000000f).into(),
                divider: rgba(0x00000014).into(),
                dialog_bottom_edge: rgba(0x00000014).into(),
                standard_button: rgba(0xffffffff).into(),
                standard_button_edge: rgba(0x0000000f).into(),
                accent_button_edge: rgba(0x00000047).into(),
                text_primary: rgba(0x000000e4).into(),
                text_secondary: rgba(0x0000009a).into(),
                text_on_accent: rgba(0xffffffff).into(),
                text_on_accent_pressed: rgba(0xffffffb3).into(),
                focus_outer: rgba(0x000000ff).into(),
                focus_inner: rgba(0xffffffff).into(),
                standard_hover: rgba(0xf6f6f6ff).into(),
                standard_active: rgba(0x00000005).into(),
            }
        }
    }
}

impl ClosePromptRenderer {
    fn respond(&mut self, response_index: usize, cx: &mut Context<Self>) {
        if response_index < self.actions.len() {
            cx.emit(PromptResponse(response_index));
            cx.stop_propagation();
        }
    }

    fn dismiss_prompt(&mut self, _: &DismissPrompt, _window: &mut Window, cx: &mut Context<Self>) {
        self.respond(self.actions.len().saturating_sub(1), cx);
    }

    fn focus_next_prompt_button(
        &mut self,
        _: &FocusNextPromptButton,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.keyboard_focus_visible = true;
        window.focus_next(cx);
        cx.notify();
        cx.stop_propagation();
    }

    fn focus_previous_prompt_button(
        &mut self,
        _: &FocusPreviousPromptButton,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.keyboard_focus_visible = true;
        window.focus_prev(cx);
        cx.notify();
        cx.stop_propagation();
    }
}

impl Focusable for ClosePromptRenderer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ClosePromptRenderer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = PromptPalette::for_window(window);
        let accent_color = rgba(self.accent);
        let accent_hover = rgba(0xc42b1cff);
        let accent_active = rgba(0xa3231aff);
        let focus_outer_color = palette.focus_outer;
        let focus_inner_color = palette.focus_inner;

        let card = div()
            .font(font(".SystemUIFont"))
            .text_size(px(14.))
            .cursor_default()
            .relative()
            .bg(palette.surface)
            .rounded(px(8.))
            .w(px(320.))
            .flex()
            .flex_col()
            .border_1()
            .border_color(palette.surface_border)
            .overflow_hidden()
            .key_context(PROMPT_KEY_CONTEXT)
            .on_action(cx.listener(Self::dismiss_prompt))
            .on_action(cx.listener(Self::focus_next_prompt_button))
            .on_action(cx.listener(Self::focus_previous_prompt_button))
            .child(
                div()
                    .px(px(16.))
                    .pt(px(24.))
                    .pb(px(12.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(palette.text_primary)
                            .text_center()
                            .child(self.message.clone()),
                    )
                    .children(self.detail.clone().map(|detail| {
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(palette.text_secondary)
                            .text_center()
                            .child(detail)
                    })),
            )
            .child(
                div()
                    .px(px(16.))
                    .py(px(20.))
                    .rounded_b(px(8.))
                    .border_t_1()
                    .border_color(palette.divider)
                    .bg(palette.footer)
                    .flex()
                    .flex_row()
                    .justify_center()
                    .gap(px(10.))
                    .children(
                        self.actions.iter().zip(self.button_focus_handles.iter()).enumerate().map(
                            |(ix, (action, focus_handle))| {
                                let is_primary = ix == 0;
                                let focus_handle = focus_handle.clone();
                                let label = action.label().clone();
                                let shows_focus_ring =
                                    focus_handle.is_focused(window) && self.keyboard_focus_visible;

                                let button = div()
                                    .id(("close-prompt-button", ix))
                                    .track_focus(&focus_handle)
                                    .relative()
                                    .cursor_pointer()
                                    .h(px(32.))
                                    .min_w(px(130.))
                                    .px(px(16.))
                                    .flex()
                                    .justify_center()
                                    .items_center()
                                    .rounded(px(4.))
                                    .border_1()
                                    .text_size(px(14.))
                                    .line_height(px(14.))
                                    .when(shows_focus_ring, |button| {
                                        button
                                            .child(
                                                div()
                                                    .absolute()
                                                    .top(px(-5.))
                                                    .left(px(-5.))
                                                    .right(px(-5.))
                                                    .bottom(px(-5.))
                                                    .rounded(px(9.))
                                                    .border_2()
                                                    .border_color(focus_outer_color),
                                            )
                                            .child(
                                                div()
                                                    .absolute()
                                                    .top(px(-2.))
                                                    .left(px(-2.))
                                                    .right(px(-2.))
                                                    .bottom(px(-2.))
                                                    .rounded(px(6.))
                                                    .border_1()
                                                    .border_color(focus_inner_color),
                                            )
                                    });

                                let edge_color = if is_primary {
                                    palette.accent_button_edge
                                } else {
                                    palette.standard_button_edge
                                };

                                let button = if is_primary {
                                    button
                                        .border_color(rgba(0x00000000))
                                        .bg(accent_color)
                                        .text_color(palette.text_on_accent)
                                        .hover(|style| style.bg(accent_hover))
                                        .active(|style| {
                                            style
                                                .bg(accent_active)
                                                .text_color(palette.text_on_accent_pressed)
                                        })
                                } else {
                                    button
                                        .border_color(palette.surface_border)
                                        .bg(palette.standard_button)
                                        .text_color(palette.text_primary)
                                        .hover(|style| style.bg(palette.standard_hover))
                                        .active(|style| {
                                            style
                                                .bg(palette.standard_active)
                                                .text_color(palette.text_secondary)
                                        })
                                };

                                button
                                    .child(
                                        div()
                                            .absolute()
                                            .bottom(px(-1.))
                                            .left(px(-1.))
                                            .right(px(-1.))
                                            .h(px(1.))
                                            .bg(edge_color),
                                    )
                                    .child(label)
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.respond(ix, cx)),
                                    )
                                    .on_action(cx.listener(
                                        move |this, _: &ActivatePromptButton, _, cx| {
                                            this.respond(ix, cx);
                                        },
                                    ))
                            },
                        ),
                    ),
            );

        let card = card.child(
            div()
                .absolute()
                .bottom(px(0.))
                .left(px(0.))
                .right(px(0.))
                .h(px(1.))
                .bg(palette.dialog_bottom_edge),
        );

        div()
            .size_full()
            .occlude()
            .bg(palette.overlay)
            .flex()
            .justify_center()
            .items_center()
            .child(card)
    }
}
