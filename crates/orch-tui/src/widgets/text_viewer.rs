//! Immutable document contract. B1 replaces the inert render/input seeds.
use crate::{
    app::App, backend::TuiBackend, clock::Clock, keys::UiEvent, state::EntityType, views::RenderCtx,
};
use ratatui::{Frame, layout::Rect};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerSource {
    Message(u64),
    Entity {
        kind: EntityType,
        id: String,
        field: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Representation {
    Human,
    Raw,
    Field,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completeness {
    Complete,
    Unavailable { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerDocument {
    pub source: ViewerSource,
    pub title: String,
    pub body: Arc<str>,
    pub representation: Representation,
    pub raw_body: Option<Arc<str>>,
    pub completeness: Completeness,
    pub original_bytes: usize,
    pub captured_at_ms: i64,
    pub initial_anchor: usize,
}
#[derive(Debug, Clone)]
pub struct TextViewer {
    document: ViewerDocument,
}
impl TextViewer {
    pub fn new(document: ViewerDocument) -> Self {
        Self { document }
    }
    pub fn document(&self) -> &ViewerDocument {
        &self.document
    }
    /// I0 inert seed: no viewer is opened by the foundation.
    pub fn render(_frame: &mut Frame, _area: Rect, _ctx: &RenderCtx) {}
}
/// I0 inert seed: B1 owns viewer input and the single Tick completion drain.
pub(crate) fn handle_ui_event<B: TuiBackend, C: Clock>(
    _app: &mut App<B, C>,
    _event: &UiEvent,
) -> bool {
    false
}
