//! The five-row header.
//!
//! Row 0 blank · row 1 BrandBar · row 2 blank · row 3 StatsBar · row 4
//! ScannerBar.

use orch_domain::TaskStatus;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;
use ratatui::widgets::{Paragraph, Widget};

use crate::format::{format_duration, format_tokens, parse_timestamp_ms};
use crate::hit::{ZoneId, ZoneKind};
use crate::state::ViewId;
use crate::theme::{
    CHECK, CIRCLE, CIRCLE_FILLED, CROSS, DIAMOND, DOT, HEAVY_RULE, LOZENGE, PIPE, RETRY_ARROW,
    SIGMA, TRIANGLE_RIGHT, Token, heavy_rule, spinner_frame,
};
use crate::views::{RenderCtx, put_line};

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    if area.height == 0 {
        return;
    }
    let prompt_inline = render_brand_bar(frame, area, ctx);
    if area.height > 2 {
        let mut spans = Vec::new();
        // Narrow layout: the prompt mode leads the second line, so it is the
        // last thing clipped there.
        if !prompt_inline {
            spans.push(prompt_mode_span(ctx));
            if !ctx.state.peer_status.is_empty() {
                spans.push(Span::styled(format!(" {DOT} "), ctx.style(Token::Ghost)));
            }
        }
        spans.push(Span::styled(
            ctx.state.peer_status.clone(),
            ctx.style(Token::Dim),
        ));
        put_line(frame, area, area.y + 2, spans);
    }
    if area.height > 3 {
        render_stats_bar(frame, area, ctx);
    }
    if area.height > 4 {
        render_scanner_bar(frame, area, ctx);
    }
}

fn running(ctx: &RenderCtx) -> usize {
    ctx.state.running_count()
}

/// The project prompt mode (design D1): the stored value, never an
/// optimistic one; `…` while a switch is being written.
fn prompt_mode_span(ctx: &RenderCtx) -> Span<'static> {
    let state = ctx.state;
    Span::styled(
        format!(
            "{}{}",
            state.prompt_mode,
            if state.prompt_mode_pending { "…" } else { "" }
        ),
        match state.prompt_mode {
            orch_domain::PromptMode::Eco => ctx.style(Token::Green),
            orch_domain::PromptMode::Normal => ctx.style(Token::Dim),
        },
    )
}

fn separator(ctx: &RenderCtx, glyph: &str) -> Span<'static> {
    Span::styled(format!(" {glyph} "), ctx.style(Token::Ghost))
}

/// Blank cells between the last tab pill and the right group, at least: one
/// reads as a continuation (`L ACTIONS 0 running`), two match the gap before
/// the tabs.
const MIN_GAP: u16 = 2;

/// Row 1. Returns whether the prompt mode is on it.
///
/// Left: brand, version, update chip, project and the tab pills, in that
/// order and at the columns they have always had. Right, flush to the last
/// column: `{uptime} · {x} running │ {y} todo · {mode} · {chip}`.
///
/// The right group must leave at least [`MIN_GAP`] blank cells after the
/// tabs. When it would not, it gives up parts from its left end, least useful first: the
/// uptime, then the counts (both repeat what the stats bar and the scanner
/// already show), then the prompt mode, which moves to the start of row 2
/// ahead of the peer status (QA-4: at 80 columns it used to be clipped away).
/// The watch chip is never dropped; when even it does not fit it follows the
/// tabs after the same gap and clips at the frame edge, as it always has. The
/// tabs never move.
fn render_brand_bar(frame: &mut Frame, area: Rect, ctx: &RenderCtx) -> bool {
    let state = ctx.state;
    let mut spans = Vec::new();

    // PulsingDiamond: alternates amber ⇄ amberDim every 10 ticks, and only
    // animates while something is running.
    let pulsing = running(ctx) > 0 && (state.tick / 10) % 2 == 1;
    spans.push(Span::styled(
        DIAMOND,
        ctx.style(if pulsing {
            Token::AmberDim
        } else {
            Token::Amber
        }),
    ));
    spans.push(Span::styled(" ORCH", ctx.bold(Token::Amber)));
    spans.push(Span::styled(
        format!(" {}", state.version),
        ctx.style(Token::Ghost),
    ));
    if let Some(latest) = state.latest_version.as_deref()
        && latest != state.version
    {
        let chip = if state.install_started {
            format!("  v{latest} INSTALLED — RESTART TO APPLY ")
        } else {
            format!("  UPDATE {latest} ")
        };
        spans.push(Span::styled(
            chip,
            ctx.chip(Token::Green, Token::SuccessBg)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(separator(ctx, DOT));
    spans.push(Span::styled(
        state.project_name.clone(),
        ctx.style(Token::Silver),
    ));

    // Centre: the tab pills. They end with one blank cell; the rest of the
    // minimum gap to the right group is counted here.
    spans.push(Span::raw("  "));
    spans.extend(tab_pills(area, ctx, &spans));
    let left_width = crate::views::spans_width(&spans) + MIN_GAP - 1;

    // Right: the group's parts, in drop order. Each part but the chip carries
    // its trailing separator, so dropping a part drops its dot with it.
    let uptime: Vec<Span<'static>> = state
        .orch_state
        .started_at
        .as_deref()
        .and_then(parse_timestamp_ms)
        .map(|started_ms| {
            vec![
                Span::styled(
                    format_duration(ctx.now_ms - started_ms),
                    ctx.style(Token::Ghost),
                ),
                separator(ctx, DOT),
            ]
        })
        .unwrap_or_default();
    let todo = state
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Todo)
        .count();
    let counts = vec![
        Span::styled(
            format!("{} running", running(ctx)),
            ctx.style(if running(ctx) > 0 {
                Token::Green
            } else {
                Token::Dim
            }),
        ),
        separator(ctx, PIPE),
        Span::styled(format!("{todo} todo"), ctx.style(Token::Dim)),
        separator(ctx, DOT),
    ];
    let prompt = vec![prompt_mode_span(ctx), separator(ctx, DOT)];
    let (mode_text, fg, bg) = if state.watch_active {
        (
            format!("{CIRCLE_FILLED} WATCHING"),
            Token::Green,
            Token::SuccessBg,
        )
    } else if state.observer_mode {
        (
            format!("{CIRCLE_FILLED} OBSERVING"),
            Token::Amber,
            Token::AccentBg,
        )
    } else {
        (format!("{CIRCLE} IDLE"), Token::Dim, Token::NeutralBg)
    };
    let chip = vec![Span::styled(
        mode_text,
        ctx.chip(fg, bg).add_modifier(Modifier::BOLD),
    )];

    let mut parts = vec![uptime, counts, prompt, chip];
    let fits = |parts: &[Vec<Span<'static>>]| {
        let group: u16 = parts.iter().map(|p| crate::views::spans_width(p)).sum();
        u32::from(left_width) + u32::from(group) <= u32::from(area.width)
    };
    // Drop from the left until the group fits; the chip (the last part) is
    // never dropped.
    let mut dropped = 0;
    while dropped < parts.len() - 1 && !fits(&parts[dropped..]) {
        dropped += 1;
    }
    // The prompt mode is the third part: it stays on row 1 unless it went.
    let inline = dropped <= 2;
    let fitted = fits(&parts[dropped..]);
    let group: Vec<Span<'static>> = parts.drain(dropped..).flatten().collect();
    let gap = if fitted {
        area.width - left_width - crate::views::spans_width(&group)
    } else {
        0
    };
    spans.push(Span::raw(" ".repeat(usize::from(MIN_GAP - 1 + gap))));
    spans.extend(group);
    put_line(frame, area, area.y + 1, spans);
    inline
}

/// The tab pills: the active tab is an inverted amber pill, the others are
/// `{key} {LABEL}` in ghost/dim. Every state shows the upper-case label, so
/// only the pill, not the letter case, marks the current view.
///
/// Registers each pill's zone as it emits it, because a pill's start column
/// is only knowable here: it follows the pulsing diamond, `" ORCH"`, the
/// version, an optional update chip, a separator and the project name, so it
/// depends on the project name's length and on `state.latest_version`
/// (design §3.2). `before` is everything already in the line.
fn tab_pills(area: Rect, ctx: &RenderCtx, before: &[Span<'_>]) -> Vec<Span<'static>> {
    let state = ctx.state;
    let y = area.y + 1;
    let mut x = area.x + crate::views::spans_width(before);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for view in ViewId::ALL {
        let pill_start = spans.len();
        let badge = if view == ViewId::Tasks && state.hidden_task_count() > 0 {
            format!(" ({})", state.sorted_tasks().len())
        } else {
            String::new()
        };
        if view == state.view {
            spans.push(Span::styled(
                format!(" {} {}{badge} ", view.hotkey(), view.label()),
                ctx.chip(Token::SolidText, Token::Amber)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            let flashing = state
                .flash
                .filter(|f| f.tab == view && f.is_bright(state.tick));
            if let Some(flash) = flashing {
                spans.push(Span::styled(
                    format!(" {} {}{badge} ", view.hotkey(), view.label()),
                    ctx.chip(Token::SolidText, flash.color)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(
                    view.hotkey().to_string(),
                    ctx.style(Token::Ghost),
                ));
                spans.push(Span::styled(
                    format!(" {}{badge}", view.label()),
                    ctx.style(Token::Dim),
                ));
            }
        }
        let width = crate::views::spans_width(&spans[pill_start..]);
        ctx.zone(
            Rect::new(x, y, width, 1),
            area,
            ZoneKind::Tab(view),
            ZoneId::Tab(view),
        );
        x += width;
        spans.push(Span::raw(" "));
        x += 1;
    }
    spans
}

fn render_stats_bar(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let count = |status: TaskStatus| state.tasks.iter().filter(|t| t.status == status).count();
    let mut spans = Vec::new();

    let mut chip = |label: String, fg: Token, bg: Token, bold: bool| {
        let mut style = ctx.chip(fg, bg);
        if bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    };

    let run = count(TaskStatus::InProgress);
    if run > 0 {
        chip(
            format!(" {TRIANGLE_RIGHT}{} RUN {run} ", spinner_frame(state.tick)),
            Token::Green,
            Token::SuccessBg,
            true,
        );
    }
    let retry = count(TaskStatus::Retrying);
    if retry > 0 {
        chip(
            format!(" {RETRY_ARROW} RETRY {retry} "),
            Token::Yellow,
            Token::WarnBg,
            false,
        );
    }
    let review = count(TaskStatus::Review);
    if review > 0 {
        chip(
            format!(" {LOZENGE} REVIEW {review} "),
            Token::Blue,
            Token::ToolBg,
            false,
        );
    }
    let todo = count(TaskStatus::Todo);
    if todo > 0 {
        chip(
            format!(" {CIRCLE} TODO {todo} "),
            Token::Dim,
            Token::NeutralBg,
            false,
        );
    }
    let done = count(TaskStatus::Done);
    if done > 0 {
        chip(
            format!(" {CHECK} DONE {done} "),
            Token::Green,
            Token::SuccessBg,
            false,
        );
    }
    let failed = count(TaskStatus::Failed);
    if failed > 0 {
        chip(
            format!(" {CROSS} FAIL {failed} "),
            Token::Red,
            Token::ErrorBg,
            true,
        );
    }
    let teams = state.teams.len();
    if teams > 0 {
        chip(
            format!(" {DIAMOND} TEAMS {teams} "),
            Token::Amber,
            Token::AccentBg,
            false,
        );
    }
    if spans.is_empty() {
        spans.push(Span::styled(
            " NO TASKS ",
            ctx.chip(Token::Dim, Token::NeutralBg),
        ));
    }

    let tokens = &state.orch_state.stats.total_tokens;
    if tokens.total > 0 {
        let reasoning = if tokens.reasoning > 0 {
            format!(" 🧠{}", format_tokens(tokens.reasoning))
        } else {
            String::new()
        };
        spans.push(Span::styled(
            format!(
                " ↑{} ↓{}{reasoning} {DOT} {SIGMA}{} ",
                format_tokens(tokens.input),
                format_tokens(tokens.output),
                format_tokens(tokens.total)
            ),
            ctx.chip(Token::Cyan, Token::AccentBg),
        ));
    }

    put_line(frame, area, area.y + 3, spans);
}

/// The scanner bar: a flat ghost rule when idle, a bouncing bright segment
/// while something is running.
fn render_scanner_bar(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let width = (area.width as usize).saturating_sub(2).max(10);
    let y = area.y + 4;
    if running(ctx) == 0 {
        put_line(
            frame,
            area,
            y,
            vec![Span::styled(heavy_rule(width), ctx.style(Token::Ghost))],
        );
        return;
    }

    let seg_len = ((width as f64 * 0.08).floor() as usize).max(4);
    let step = 2usize;
    let total_frames = ((width + seg_len) as f64 / step as f64).ceil() as usize;
    let cycle = (total_frames * 2).max(1);
    let phase = (ctx.state.tick as usize) % cycle;
    // Triangular bounce.
    let frame_index = if phase < total_frames {
        phase
    } else {
        cycle - phase
    };
    let start = (frame_index * step).min(width);
    let bright_len = seg_len.min(width.saturating_sub(start));

    let spans = vec![
        Span::styled(heavy_rule(start), ctx.style(Token::Ghost)),
        Span::styled(heavy_rule(bright_len), ctx.style(Token::Amber)),
        Span::styled(
            heavy_rule(width.saturating_sub(start + bright_len)),
            ctx.style(Token::Ghost),
        ),
    ];
    put_line(frame, area, y, spans);
}

/// A heavy rule sized for the frame — used by the detail panels too.
pub fn rule_span(ctx: &RenderCtx, width: usize) -> Span<'static> {
    Span::styled(
        HEAVY_RULE.to_string().repeat(width),
        ctx.style(Token::Ghost),
    )
}

/// Render an empty line (used to pad fixed-height regions).
pub fn blank(frame: &mut Frame, area: Rect, y: u16) {
    if y < area.bottom() {
        Paragraph::new("").render(Rect::new(area.x, y, area.width, 1), frame.buffer_mut());
    }
}
