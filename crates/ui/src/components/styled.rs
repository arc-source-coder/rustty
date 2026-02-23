use gpui::{Div, Styled, div};

/// Returns a `Div` as horizontal flex layout (flex-row with items centered).
#[inline(always)]
pub fn h_flex() -> Div {
    div().flex().flex_row().items_center()
}

/// Returns a `Div` as vertical flex layout (flex-column).
#[inline(always)]
pub fn v_flex() -> Div {
    div().flex().flex_col()
}
