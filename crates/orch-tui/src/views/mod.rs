//! Frame composition (docs/parity/tui.md §6).
//!
//! Regions, top to bottom: Header (5 rows), a blank spacer, the help overlay
//! **or** the main content region, a blank spacer, the bottom panel, a flexible
//! spacer, toasts, the undo banner, and the command bar.
//!
//! Every view and widget takes a [`RenderCtx`] — the state plus the backend
//! capability flags plus the frame clock — so rendering never touches the
//! backend.

pub mod activity;
pub mod agents;
pub mod detail;
pub mod goals;
pub mod logs;
pub mod tasks;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::app::App;
use crate::backend::TuiBackend;
use crate::clock::Clock;
use crate::layout::{MIN_HEIGHT, MIN_WIDTH, regions};
use crate::state::{AppState, DetailPanel, InputMode, ViewId};
use crate::theme::{HEAVY_RULE, Palette, Token};
use crate::widgets;

/// Which optional actions the backend supports — the Rust form of the TS
/// "was this callback passed?" checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub can_create_task: bool,
    pub can_add_agent: bool,
    pub can_create_goal: bool,
    pub can_force_stop_agent: bool,
}

impl Caps {
    /// The capability relevant to the active view's `+ add …` pseudo-row.
    pub fn can_create_for(&self, view: ViewId) -> bool {
        match view {
            ViewId::Goals => self.can_create_goal,
            ViewId::Tasks => self.can_create_task,
            ViewId::Agents => self.can_add_agent,
            ViewId::Logs => false,
        }
    }
}

pub struct RenderCtx<'a> {
    pub state: &'a AppState,
    pub caps: Caps,
    pub now_ms: i64,
}

impl<'a> RenderCtx<'a> {
    pub fn palette(&self) -> &Palette {
        &self.state.palette
    }

    pub fn style(&self, token: Token) -> Style {
        Style::default().fg(self.state.palette.get(token))
    }

    pub fn bold(&self, token: Token) -> Style {
        self.style(token).add_modifier(Modifier::BOLD)
    }

    /// Foreground on a saturated fill.
    pub fn chip(&self, fg: Token, bg: Token) -> Style {
        Style::default()
            .fg(self.state.palette.get(fg))
            .bg(self.state.palette.get(bg))
    }
}

/// Draw one frame.
pub fn draw<B: TuiBackend, C: Clock>(frame: &mut Frame, app: &App<B, C>) {
    let caps = Caps {
        can_create_task: app.backend.can_create_task(),
        can_add_agent: app.backend.can_add_agent(),
        can_create_goal: app.backend.can_create_goal(),
        can_force_stop_agent: app.backend.can_force_stop_agent(),
    };
    let ctx = RenderCtx {
        state: &app.state,
        caps,
        now_ms: app.now(),
    };
    render(frame, frame.area(), &ctx);
}

/// Render into an explicit area — the entry point the tests drive.
pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        render_too_small(frame, area, ctx);
        return;
    }

    let state = ctx.state;
    let can_create = ctx.caps.can_create_for(state.view);
    let r = regions(
        area.width,
        area.height,
        state.view,
        state.list_item_count(can_create),
        state.split_offset,
        state.detail_maximized,
    );

    let mut y = area.y;
    // Header — exactly five rows.
    widgets::header::render(frame, Rect::new(area.x, y, area.width, 5), ctx);
    y += 5;
    // Blank spacer.
    y += 1;

    if state.show_help {
        // The overlay replaces the content area and the bottom panel.
        let overlay_h = area.height.saturating_sub(7);
        widgets::help_overlay::render(frame, Rect::new(area.x, y, area.width, overlay_h), ctx);
        y += overlay_h;
    } else {
        if r.main_h > 0 {
            let main = Rect::new(area.x, y, area.width, r.main_h.min(area.height - y));
            render_main(frame, main, ctx);
            y += main.height;
        }
        // Blank spacer between the regions.
        if y < area.bottom() {
            y += 1;
        }
        if r.feed_h > 0 && y < area.bottom() {
            let feed = Rect::new(area.x, y, area.width, r.feed_h.min(area.bottom() - y));
            render_bottom(frame, feed, ctx);
            y += feed.height;
        }
    }

    // Toasts, the undo banner and the command bar are bottom-anchored.
    let mut bottom = area.bottom();
    bottom -= 1;
    widgets::command_bar::render(frame, Rect::new(area.x, bottom, area.width, 1), ctx);

    let undo_rows = state.pending_deletions.len() as u16;
    if undo_rows > 0 && bottom > y {
        let rows = undo_rows.min(bottom - y);
        bottom -= rows;
        widgets::undo_banner::render(frame, Rect::new(area.x, bottom, area.width, rows), ctx);
    }

    let toast_rows = state.visible_toasts().len() as u16;
    if toast_rows > 0 && bottom > y {
        let rows = toast_rows.min(bottom - y);
        bottom -= rows;
        widgets::toast::render(frame, Rect::new(area.x, bottom, area.width, rows), ctx);
    }
}

fn render_too_small(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    // Deliberate addition, not parity (PARITY.md §12.2).
    let text = format!("terminal too small — need at least {MIN_WIDTH}×{MIN_HEIGHT}");
    let line = Line::from(Span::styled(text, ctx.bold(Token::Amber)));
    let y = area.y + area.height / 2;
    Paragraph::new(line)
        .centered()
        .render(Rect::new(area.x, y, area.width, 1), frame.buffer_mut());
}

fn render_main(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let label_area = Rect::new(area.x, area.y, area.width, 1);
    let list_area = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    match ctx.state.view {
        ViewId::Goals => {
            section_label(frame, label_area, ctx, "GOALS", None);
            goals::render(frame, list_area, ctx);
        }
        ViewId::Tasks => {
            section_label(frame, label_area, ctx, "TASKS", None);
            tasks::render(frame, list_area, ctx);
        }
        ViewId::Agents => {
            section_label(frame, label_area, ctx, "AGENTS", None);
            agents::render(frame, list_area, ctx);
        }
        ViewId::Logs => {
            section_label(frame, label_area, ctx, "ACTIONS", None);
            logs::render(frame, list_area, ctx);
        }
    }
}

fn render_bottom(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    if state.input_mode == InputMode::Wizard
        && let Some(wizard) = state.wizard.as_ref()
    {
        widgets::wizard::render(frame, area, ctx, wizard);
        return;
    }
    if state.input_mode == InputMode::Command {
        let label_area = Rect::new(area.x, area.y, area.width, 1);
        section_label(frame, label_area, ctx, "COMMANDS", None);
        widgets::command_bar::render_suggestions(
            frame,
            Rect::new(
                area.x,
                area.y + 1,
                area.width,
                area.height.saturating_sub(1),
            ),
            ctx,
        );
        return;
    }
    if state.show_agent_picker {
        widgets::pickers::render_agent_picker(frame, area, ctx);
        return;
    }
    if state.show_type_picker {
        widgets::pickers::render_type_picker(frame, area, ctx);
        return;
    }
    if state.detail != DetailPanel::None {
        detail::render(frame, area, ctx);
        return;
    }
    if state.messages.is_empty()
        && let Some(_config) = crate::onboarding::config_for(state.view)
    {
        widgets::onboarding::render(frame, area, ctx);
        return;
    }
    activity::render(frame, area, ctx);
}

/// `SectionLabel`: `━━━` + ` {LABEL} ` + a trailing rule, with an optional
/// suffix before the rule.
pub fn section_label(
    frame: &mut Frame,
    area: Rect,
    ctx: &RenderCtx,
    label: &str,
    suffix: Option<Vec<Span<'static>>>,
) {
    let mut spans = vec![
        Span::styled(HEAVY_RULE.to_string().repeat(3), ctx.style(Token::Ghost)),
        Span::styled(
            format!(" {label} "),
            ctx.chip(Token::Dim, Token::NeutralBg)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let mut used = 3 + label.chars().count() + 2;
    if let Some(suffix) = suffix {
        spans.push(Span::styled(
            HEAVY_RULE.to_string().repeat(2),
            ctx.style(Token::Ghost),
        ));
        used += 2;
        for span in suffix {
            used += span.content.chars().count();
            spans.push(span);
        }
    }
    let remaining = (area.width as usize).saturating_sub(used + 1);
    spans.push(Span::styled(
        HEAVY_RULE.to_string().repeat(remaining),
        ctx.style(Token::Ghost),
    ));
    Paragraph::new(Line::from(spans)).render(area, frame.buffer_mut());
}

/// The detail-panel resize hint.
pub fn resize_hint(maximized: bool) -> &'static str {
    if maximized {
        "+/- exit max"
    } else {
        "+/- resize │ M max"
    }
}

/// Render a single line of spans at `y`, clipped to `area`.
pub fn put_line(frame: &mut Frame, area: Rect, y: u16, spans: Vec<Span<'_>>) {
    if y >= area.bottom() {
        return;
    }
    Paragraph::new(Line::from(spans))
        .render(Rect::new(area.x, y, area.width, 1), frame.buffer_mut());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::FakeBackend;
    use crate::clock::TestClock;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    pub fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn a_tiny_terminal_renders_the_minimum_size_guard() {
        let app = App::new(FakeBackend::new(), TestClock::new(0));
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("terminal too small"), "{text}");
    }

    #[test]
    fn a_normal_terminal_renders_the_dashboard_chrome() {
        let app = App::new(FakeBackend::new(), TestClock::new(0));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(!text.contains("terminal too small"), "{text}");
        assert!(text.contains("ORCH"), "{text}");
        assert!(text.contains("TASKS"), "{text}");
    }
}
