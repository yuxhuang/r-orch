//! Shared navigation contract. B0 replaces the inert arithmetic seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavAction {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollMode {
    Selectable,
    Text,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScrollState {
    pub selected: Option<usize>,
    pub offset: usize,
}
#[derive(Debug, Clone, Copy)]
pub struct ScrollMetrics<'a> {
    pub items: usize,
    pub display_rows: usize,
    pub viewport_rows: usize,
    pub selection_rows: &'a [usize],
    pub mode: ScrollMode,
}
/// I0 inert seed; existing navigation remains in use until B0 integration.
pub fn navigate(_state: &mut ScrollState, _action: NavAction, _metrics: ScrollMetrics<'_>) {}
