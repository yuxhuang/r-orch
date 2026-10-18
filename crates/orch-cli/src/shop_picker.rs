//! The agent shop picker — ported from `src/cli/shop-picker.ts`
//! (`docs/parity/cli.md` §21.2).
//!
//! Reached only when `--list` is absent **and** stdout is a TTY; the picker
//! itself returns `None` immediately when stdin is not a TTY, which is how
//! `orch agent shop` degrades in a pipe.
//!
//! The scrolling, the key map and the rendered lines are a state machine
//! ([`Picker`]) so they can be tested without a terminal; only
//! [`pick_from_shop`] touches raw mode.

use orch_domain::AgentShopTemplate;
use orch_tui::format::cap_display;
use orch_tui::text::text_display_width;

/// Borrowed labels let authored entries reuse the exact legacy renderer.
pub struct ShopEntryView<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub summary: &'a str,
}
pub trait ShopEntry {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn summary(&self) -> &str;
}
impl ShopEntry for AgentShopTemplate {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        self.description
    }
    fn summary(&self) -> &str {
        self.tier.as_str()
    }
}
impl ShopEntry for ShopEntryView<'_> {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        self.description
    }
    fn summary(&self) -> &str {
        self.summary
    }
}

/// The heading line, and the one used when it does not fit: the keys are
/// the part a narrow terminal must not lose.
const HEADING: &str = "  AGENT SHOP  — arrow keys to navigate, enter to select, q to cancel";
const HEADING_SHORT: &str = "  AGENT SHOP  ↑↓ move · enter select · q cancel";

/// The selection cursor and the blank gutter of the other rows; both four
/// columns wide.
const CURSOR: &str = "  ▸ ";
const GUTTER: &str = "    ";

/// Display width of the ` — ` between a name and its description.
const SEPARATOR_W: usize = 3;

/// How a frame spends the terminal's rows.
///
/// Every line of the frame occupies exactly one terminal row: lines are
/// fitted to the width (so nothing wraps) and joined with `\r\n` without a
/// trailing newline (so nothing scrolls). The page size is what is left once
/// the chrome is counted:
///
/// - `blanks`: a blank line above and below the heading and above the footer.
/// - `footer`: the `n-m of total` line, present only when the list is paged.
///
/// A terminal too short for the full chrome drops the blank lines before it
/// drops rows of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub page: usize,
    pub blanks: bool,
    pub footer: bool,
}

impl Layout {
    pub fn new(total: usize, rows: u16) -> Self {
        let rows = usize::from(rows);
        // blank, heading, blank — then the list.
        if total + 3 <= rows {
            return Layout {
                page: total.max(1),
                blanks: true,
                footer: false,
            };
        }
        // blank, heading, blank, page, blank, footer.
        if rows >= 6 {
            return Layout {
                page: (rows - 5).min(total).max(1),
                blanks: true,
                footer: true,
            };
        }
        // heading, list.
        if total < rows {
            return Layout {
                page: total.max(1),
                blanks: false,
                footer: false,
            };
        }
        // heading, page, footer.
        Layout {
            page: rows.saturating_sub(2).min(total).max(1),
            blanks: false,
            footer: true,
        }
    }

    /// Rows the frame occupies.
    pub fn height(&self) -> usize {
        1 + self.page
            + usize::from(self.footer)
            + if self.blanks {
                2 + usize::from(self.footer)
            } else {
                0
            }
    }
}

/// How many templates one page shows on a terminal `rows` tall, once the
/// heading, its blank lines and (when paged) the footer are counted.
pub fn page_size(total: usize, rows: u16) -> usize {
    Layout::new(total, rows).page
}

/// What a keypress does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerAction {
    Redraw,
    Selected(usize),
    Cancelled,
    Ignored,
}

/// The keys the picker understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKey {
    Up,
    Down,
    Enter,
    Cancel,
    Other,
}

#[derive(Debug, Clone)]
pub struct Picker {
    total: usize,
    cols: u16,
    rows: u16,
    selected: usize,
}

impl Picker {
    pub fn new(total: usize, cols: u16, rows: u16) -> Self {
        Picker {
            total,
            cols,
            rows,
            selected: 0,
        }
    }

    /// A terminal resize: both dimensions change the frame.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
    }

    pub fn layout(&self) -> Layout {
        Layout::new(self.total, self.rows)
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn page_size(&self) -> usize {
        self.layout().page
    }

    /// `Math.max(0, Math.min(selected - floor(pageSize / 2), total - pageSize))`
    /// — the selection is kept centred until it reaches either end.
    pub fn start(&self) -> usize {
        let page = self.page_size();
        let centred = self.selected as isize - (page / 2) as isize;
        let max_start = self.total.saturating_sub(page) as isize;
        centred.min(max_start).max(0) as usize
    }

    pub fn end(&self) -> usize {
        (self.start() + self.page_size()).min(self.total)
    }

    /// Up and down **wrap** modulo the list length.
    pub fn on_key(&mut self, key: PickerKey) -> PickerAction {
        if self.total == 0 {
            return match key {
                PickerKey::Cancel | PickerKey::Enter => PickerAction::Cancelled,
                _ => PickerAction::Ignored,
            };
        }
        match key {
            PickerKey::Up => {
                self.selected = (self.selected + self.total - 1) % self.total;
                PickerAction::Redraw
            }
            PickerKey::Down => {
                self.selected = (self.selected + 1) % self.total;
                PickerAction::Redraw
            }
            PickerKey::Enter => PickerAction::Selected(self.selected),
            PickerKey::Cancel => PickerAction::Cancelled,
            PickerKey::Other => PickerAction::Ignored,
        }
    }

    /// The whole frame, one entry per terminal row: the heading, the
    /// visible templates and, when the list is paged, the `n-m of total`
    /// footer. Every line is at most `cols` display columns wide.
    pub fn frame<T: ShopEntry>(&self, templates: &[T]) -> Vec<String> {
        let layout = self.layout();
        let width = usize::from(self.cols);
        let mut lines = Vec::with_capacity(layout.height());
        if layout.blanks {
            lines.push(String::new());
        }
        let heading = if text_display_width(HEADING) <= width {
            HEADING
        } else {
            HEADING_SHORT
        };
        lines.push(cap_display(heading, width));
        if layout.blanks {
            lines.push(String::new());
        }
        for (index, template) in templates
            .iter()
            .enumerate()
            .take(self.end())
            .skip(self.start())
        {
            lines.push(entry_line(template, index == self.selected, width));
        }
        if layout.footer {
            if layout.blanks {
                lines.push(String::new());
            }
            let footer = format!(
                "  {}-{} of {}",
                self.start() + 1,
                self.end(),
                templates.len()
            );
            lines.push(cap_display(&footer, width));
        }
        lines
    }
}

/// One template as `▸ Name — description [tier]`, fitted to `width` columns.
///
/// The description gives way first, then the tier, then the name, so the
/// part that tells two roles apart stays readable on a narrow terminal. Every
/// cut is marked with an ellipsis; nothing wraps.
fn entry_line<T: ShopEntry>(template: &T, selected: bool, width: usize) -> String {
    let prefix = if selected { CURSOR } else { GUTTER };
    let name = sanitize(template.name());
    let description = sanitize(template.description());
    let tier = format!(" [{}]", sanitize(template.summary()));

    let budget = width.saturating_sub(text_display_width(prefix));
    let name_w = text_display_width(&name);
    let tier_w = text_display_width(&tier);
    let full = format!("{name} — {description}{tier}");
    let body = if text_display_width(&full) <= budget {
        full
    } else if name_w + SEPARATOR_W + tier_w + 2 <= budget {
        // At least one column of description plus its ellipsis fits.
        let room = budget - name_w - SEPARATOR_W - tier_w;
        format!("{name} — {}{tier}", cap_display(&description, room))
    } else if name_w + tier_w <= budget {
        format!("{name}{tier}")
    } else {
        cap_display(&name, budget)
    };
    cap_display(&format!("{prefix}{body}"), width)
}

/// Control characters (including ESC, CR and LF) and bidi overrides would
/// move the cursor or restyle the terminal mid-frame; a template's name and
/// description are data, never terminal input.
pub(crate) fn sanitize(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                '\u{fffd}'
            } else {
                ch
            }
        })
        .collect()
}

/// The bytes that paint one frame: clear, home, then the lines joined by
/// `\r\n`.
///
/// Raw mode turns off output post-processing (`-opost`), so a bare `\n`
/// moves down without returning to column 0 and every row would start where
/// the previous one ended. There is no newline after the last line: on the
/// bottom row it would scroll the heading off the screen.
pub fn encode_frame(lines: &[String]) -> String {
    let mut out = String::from("\x1b[2J\x1b[H");
    out.push_str(&lines.join("\r\n"));
    out
}

/// The non-interactive table (`--list`, or a non-TTY stdout).
///
/// The headers are mixed case here — `Key Name Tier Skills` — unlike the
/// upper-case headers everywhere else, and only the **first two** skills are
/// shown.
pub fn render_list(styles: &crate::output::Styles, templates: &[AgentShopTemplate]) -> String {
    let rows: Vec<Vec<String>> = templates
        .iter()
        .map(|template| {
            vec![
                template.key.to_owned(),
                template.name.to_owned(),
                template.tier.as_str().to_owned(),
                template
                    .skills
                    .iter()
                    .take(2)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", "),
            ]
        })
        .collect();
    crate::output::render_table(styles, &["Key", "Name", "Tier", "Skills"], &rows, 2)
}

/// Drive the picker against the terminal.
///
/// `None` means cancelled — including the immediate `None` when stdin is not
/// a TTY, which is the documented degradation.
pub fn pick_from_shop(
    templates: &'static [AgentShopTemplate],
    stdin_is_tty: bool,
) -> Option<&'static AgentShopTemplate> {
    pick_index(templates, stdin_is_tty).and_then(|index| templates.get(index))
}

pub fn pick_index<T: ShopEntry>(templates: &[T], stdin_is_tty: bool) -> Option<usize> {
    if !stdin_is_tty {
        return None;
    }

    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let mut picker = Picker::new(templates.len(), cols, rows);

    let _raw = RawMode::enable()?;
    loop {
        draw(&picker, templates);

        let Ok(event) = crossterm::event::read() else {
            return None;
        };
        let key = match event {
            crossterm::event::Event::Key(key) => translate_key(key),
            // A resize changes the page size and the line width; redraw.
            crossterm::event::Event::Resize(cols, rows) => {
                picker.resize(cols, rows);
                continue;
            }
            _ => continue,
        };

        match picker.on_key(key) {
            PickerAction::Selected(index) => {
                clear_screen();
                return Some(index);
            }
            PickerAction::Cancelled => {
                clear_screen();
                return None;
            }
            PickerAction::Redraw | PickerAction::Ignored => {}
        }
    }
}

/// `up`/`Ctrl+P` and `down`/`Ctrl+N`; `q`, `escape` and `Ctrl+C` cancel.
fn translate_key(key: crossterm::event::KeyEvent) -> PickerKey {
    use crossterm::event::{KeyCode, KeyModifiers};
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Up => PickerKey::Up,
        KeyCode::Down => PickerKey::Down,
        KeyCode::Char('p') if ctrl => PickerKey::Up,
        KeyCode::Char('n') if ctrl => PickerKey::Down,
        KeyCode::Enter => PickerKey::Enter,
        KeyCode::Char('c') if ctrl => PickerKey::Cancel,
        KeyCode::Char('q') | KeyCode::Esc => PickerKey::Cancel,
        _ => PickerKey::Other,
    }
}

fn draw<T: ShopEntry>(picker: &Picker, templates: &[T]) {
    print!("{}", encode_frame(&picker.frame(templates)));
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn clear_screen() {
    print!("\x1b[2J\x1b[H");
}

/// Restores cooked mode on every exit path, including a panic.
struct RawMode;

impl RawMode {
    fn enable() -> Option<Self> {
        crossterm::terminal::enable_raw_mode().ok()?;
        Some(RawMode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::AGENT_SHOP_TEMPLATES;

    #[test]
    fn owned_labels_share_width_control_and_resize_guards() {
        let entries = [ShopEntryView {
            name: "worker",
            description: "long\n\x1b[2Jdescription界界界界界界界界界界界界界界",
            summary: "codex\x1b[31m/model",
        }];
        let mut picker = Picker::new(1, 40, 12);
        for width in [40, 20, 8] {
            picker.resize(width, 12);
            let frame = picker.frame(&entries);
            assert!(frame.iter().all(|line| !line.contains('\x1b')
                && !line.contains('\n')
                && !line.contains('\r')));
            assert!(
                frame
                    .iter()
                    .all(|line| text_display_width(line) <= usize::from(width))
            );
        }
    }

    #[test]
    fn the_page_size_leaves_room_for_the_heading_and_the_footer() {
        // Unpaged: blank, heading, blank, 15 rows.
        assert_eq!(page_size(15, 24), 15);
        assert_eq!(page_size(15, 18), 15);
        // Paged: blank, heading, blank, page, blank, footer.
        assert_eq!(page_size(15, 17), 12);
        assert_eq!(page_size(15, 10), 5);
        // Too short for the blank lines: heading, page, footer.
        assert_eq!(page_size(15, 4), 2);
        // Never zero, however small the terminal claims to be.
        assert_eq!(page_size(15, 0), 1);
        // Never more than there are templates.
        assert_eq!(page_size(3, 100), 3);
    }

    #[test]
    fn the_arrow_keys_wrap_in_both_directions() {
        let mut picker = Picker::new(3, 120, 24);
        assert_eq!(picker.selected(), 0);

        assert_eq!(picker.on_key(PickerKey::Down), PickerAction::Redraw);
        assert_eq!(picker.selected(), 1);
        picker.on_key(PickerKey::Down);
        picker.on_key(PickerKey::Down);
        assert_eq!(picker.selected(), 0, "down wraps past the end");

        picker.on_key(PickerKey::Up);
        assert_eq!(picker.selected(), 2, "up wraps past the start");
    }

    #[test]
    fn enter_selects_and_q_cancels() {
        let mut picker = Picker::new(3, 120, 24);
        picker.on_key(PickerKey::Down);
        assert_eq!(picker.on_key(PickerKey::Enter), PickerAction::Selected(1));
        assert_eq!(picker.on_key(PickerKey::Cancel), PickerAction::Cancelled);
        assert_eq!(picker.on_key(PickerKey::Other), PickerAction::Ignored);
    }

    #[test]
    fn an_empty_list_can_only_be_cancelled() {
        let mut picker = Picker::new(0, 120, 24);
        assert_eq!(picker.on_key(PickerKey::Down), PickerAction::Ignored);
        assert_eq!(picker.on_key(PickerKey::Enter), PickerAction::Cancelled);
        assert_eq!(picker.on_key(PickerKey::Cancel), PickerAction::Cancelled);
    }

    #[test]
    fn the_window_keeps_the_selection_centred_until_it_hits_an_end() {
        // 15 templates, a page of 6.
        let mut picker = Picker::new(15, 120, 11);
        assert_eq!(picker.page_size(), 6);
        assert_eq!((picker.start(), picker.end()), (0, 6));

        for _ in 0..5 {
            picker.on_key(PickerKey::Down);
        }
        // selected = 5, centred start = 5 - 3 = 2.
        assert_eq!(picker.selected(), 5);
        assert_eq!((picker.start(), picker.end()), (2, 8));

        for _ in 0..9 {
            picker.on_key(PickerKey::Down);
        }
        // selected = 14, clamped to the last page.
        assert_eq!(picker.selected(), 14);
        assert_eq!((picker.start(), picker.end()), (9, 15));
    }

    #[test]
    fn the_rendered_frame_marks_the_selection_and_shows_the_window_footer() {
        let picker = Picker::new(AGENT_SHOP_TEMPLATES.len(), 200, 10);
        let frame = picker.frame(&AGENT_SHOP_TEMPLATES);
        let lines: Vec<&str> = frame[3..].iter().map(String::as_str).collect();

        assert!(lines[0].starts_with("  ▸ "), "{:?}", lines[0]);
        assert!(lines[1].starts_with("    "), "{:?}", lines[1]);
        assert!(
            lines[0].contains(" — "),
            "the description follows an em dash"
        );
        assert!(lines[0].ends_with(']'), "the tier is bracketed");
        assert!(
            frame
                .last()
                .unwrap()
                .ends_with(&format!("of {}", AGENT_SHOP_TEMPLATES.len())),
            "the footer counts the whole list"
        );
    }

    #[test]
    fn a_list_that_fits_has_no_footer() {
        let picker = Picker::new(AGENT_SHOP_TEMPLATES.len(), 200, 100);
        let frame = picker.frame(&AGENT_SHOP_TEMPLATES);
        assert!(!frame.iter().any(|line| line.contains(" of ")), "{frame:?}");
        assert_eq!(frame.len(), AGENT_SHOP_TEMPLATES.len() + 3);
    }

    #[test]
    fn the_list_table_matches_the_reference_columns() {
        let rendered = render_list(&crate::output::Styles::plain(), &AGENT_SHOP_TEMPLATES);
        let lines: Vec<&str> = rendered.lines().collect();

        assert!(
            lines[0].starts_with("  Key "),
            "mixed-case headers: {:?}",
            lines[0]
        );
        assert!(lines[0].contains("Name"));
        assert!(lines[0].contains("Tier"));
        assert!(lines[0].contains("Skills"));
        assert_eq!(lines.len(), AGENT_SHOP_TEMPLATES.len() + 1);

        // The first row is the first template, with at most two skills.
        let first = &AGENT_SHOP_TEMPLATES[0];
        assert!(lines[1].contains(first.key));
        assert!(lines[1].contains(first.name));
        let shown = first.skills.iter().take(2).copied().collect::<Vec<_>>();
        assert!(lines[1].contains(&shown.join(", ")));
        if first.skills.len() > 2 {
            assert!(!lines[1].contains(first.skills[2]), "only two skills");
        }
    }

    /// Hostile data: a long name, a long description, CJK, a combining
    /// accent, ZWJ emoji, right-to-left text and an escape sequence.
    const HOSTILE: [AgentShopTemplate; 4] = [
        AgentShopTemplate {
            key: "long",
            name: "A Very Long Role Name That Keeps Going Well Past Any Reasonable Column Budget",
            description: "a description that repeats itself, a description that repeats itself, a description that repeats itself",
            tier: orch_domain::ModelTier::Capable,
            approval_policy: orch_domain::ApprovalPolicy::Auto,
            skills: &[],
            role: "",
        },
        AgentShopTemplate {
            key: "cjk",
            name: "研究员 👩‍👩‍👧 Café",
            description: "分析市场数据并撰写报告 — עברית טקסט 👍🏽 résumé",
            tier: orch_domain::ModelTier::Balanced,
            approval_policy: orch_domain::ApprovalPolicy::Auto,
            skills: &[],
            role: "",
        },
        AgentShopTemplate {
            key: "esc",
            name: "Escaper\x1b[2J",
            description: "line one\nline two\r\u{202e}reversed",
            tier: orch_domain::ModelTier::Fast,
            approval_policy: orch_domain::ApprovalPolicy::Auto,
            skills: &[],
            role: "",
        },
        AgentShopTemplate {
            key: "short",
            name: "QA",
            description: "tests",
            tier: orch_domain::ModelTier::Fast,
            approval_policy: orch_domain::ApprovalPolicy::Auto,
            skills: &[],
            role: "",
        },
    ];

    fn every_size() -> impl Iterator<Item = (u16, u16)> {
        [20u16, 40, 60, 80, 120]
            .into_iter()
            .flat_map(|cols| [4u16, 6, 8, 12, 14, 18, 24, 40].map(|rows| (cols, rows)))
    }

    #[test]
    fn no_line_is_wider_than_the_terminal() {
        for templates in [&AGENT_SHOP_TEMPLATES[..], &HOSTILE[..]] {
            for (cols, rows) in every_size() {
                let mut picker = Picker::new(templates.len(), cols, rows);
                for _ in 0..templates.len() {
                    for line in picker.frame(templates) {
                        assert!(
                            text_display_width(&line) <= usize::from(cols),
                            "{cols}x{rows}: {} columns: {line:?}",
                            text_display_width(&line)
                        );
                    }
                    picker.on_key(PickerKey::Down);
                }
            }
        }
    }

    #[test]
    fn the_frame_fits_the_terminal_and_keeps_the_heading_and_the_selection() {
        for (cols, rows) in every_size() {
            let mut picker = Picker::new(AGENT_SHOP_TEMPLATES.len(), cols, rows);
            for _ in 0..AGENT_SHOP_TEMPLATES.len() {
                let frame = picker.frame(&AGENT_SHOP_TEMPLATES);
                if rows >= 3 {
                    assert!(
                        frame.len() <= usize::from(rows),
                        "{cols}x{rows}: {} lines: {frame:#?}",
                        frame.len()
                    );
                }
                assert_eq!(frame.len(), picker.layout().height());
                assert!(
                    frame.iter().any(|line| line.starts_with("  AGENT SHOP")),
                    "{cols}x{rows}: heading missing: {frame:#?}"
                );
                assert_eq!(
                    frame.iter().filter(|line| line.starts_with(CURSOR)).count(),
                    1,
                    "{cols}x{rows}: one selected row: {frame:#?}"
                );
                picker.on_key(PickerKey::Down);
            }
        }
    }

    #[test]
    fn a_short_wide_terminal_pages_without_losing_the_heading() {
        // T0 F3: 120x12 used to show 8 rows plus 5 rows of chrome.
        let picker = Picker::new(AGENT_SHOP_TEMPLATES.len(), 120, 12);
        let frame = picker.frame(&AGENT_SHOP_TEMPLATES);
        assert_eq!(frame.len(), 12, "{frame:#?}");
        assert_eq!(frame[1], HEADING);
        assert_eq!(frame.last().unwrap(), "  1-7 of 15");
    }

    #[test]
    fn the_encoded_frame_returns_to_column_zero_and_never_scrolls() {
        let picker = Picker::new(AGENT_SHOP_TEMPLATES.len(), 80, 24);
        let encoded = encode_frame(&picker.frame(&AGENT_SHOP_TEMPLATES));
        let body = encoded
            .strip_prefix("\x1b[2J\x1b[H")
            .expect("clear and home first");
        // Raw mode has no ONLCR: a bare LF would keep the column.
        for (at, _) in body.match_indices('\n') {
            assert_eq!(&body[at - 1..at], "\r", "bare LF at byte {at}");
        }
        assert!(
            !body.ends_with('\n'),
            "a trailing LF scrolls the bottom row"
        );
        assert_eq!(body.matches("\r\n").count(), 18 - 1);
    }

    #[test]
    fn a_narrow_terminal_cuts_the_description_and_keeps_the_name_and_tier() {
        let picker = Picker::new(AGENT_SHOP_TEMPLATES.len(), 60, 24);
        let frame = picker.frame(&AGENT_SHOP_TEMPLATES);
        let qa = frame
            .iter()
            .find(|line| line.contains("QA Engineer"))
            .expect("QA Engineer is listed");
        let tier = orch_domain::get_shop_template_by_key("qa-engineer")
            .unwrap()
            .tier
            .as_str();
        assert_eq!(
            frame[1], HEADING_SHORT,
            "the keys survive a narrow terminal"
        );
        assert!(qa.contains("QA Engineer — "), "{qa:?}");
        assert!(qa.contains("…"), "{qa:?}");
        assert!(qa.ends_with(&format!("… [{tier}]")), "{qa:?}");
        assert!(text_display_width(qa) <= 60);
    }

    #[test]
    fn a_name_too_long_for_the_line_is_cut_with_an_ellipsis() {
        let picker = Picker::new(HOSTILE.len(), 40, 24);
        let frame = picker.frame(&HOSTILE);
        let long = &frame[3];
        assert!(long.starts_with("  ▸ A Very Long"), "{long:?}");
        assert!(long.ends_with('…'), "{long:?}");
        assert_eq!(text_display_width(long), 40);
    }

    #[test]
    fn template_text_cannot_inject_terminal_control() {
        let picker = Picker::new(HOSTILE.len(), 200, 24);
        let frame = picker.frame(&HOSTILE);
        for line in &frame {
            assert!(
                !line.chars().any(|ch| ch.is_control() || ch == '\u{202e}'),
                "{line:?}"
            );
        }
        let escaper = frame.iter().find(|line| line.contains("Escaper")).unwrap();
        assert!(escaper.contains("Escaper\u{fffd}[2J"), "{escaper:?}");
    }

    #[test]
    fn a_resize_reflows_the_frame() {
        let mut picker = Picker::new(AGENT_SHOP_TEMPLATES.len(), 120, 40);
        assert_eq!(picker.frame(&AGENT_SHOP_TEMPLATES).len(), 18);
        picker.resize(60, 12);
        let frame = picker.frame(&AGENT_SHOP_TEMPLATES);
        assert_eq!(frame.len(), 12);
        assert!(frame.iter().all(|line| text_display_width(line) <= 60));
    }

    #[test]
    fn a_non_tty_stdin_cancels_immediately() {
        assert!(pick_from_shop(&AGENT_SHOP_TEMPLATES, false).is_none());
    }
}
