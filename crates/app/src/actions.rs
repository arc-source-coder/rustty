use gpui::{Action, actions};

actions!(
    terminal_tabs,
    [NewTab, CloseActiveTab, SelectNextTab, SelectPreviousTab,]
);

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = terminal_tabs, no_json)]
pub struct SelectTab {
    pub index: usize,
}
