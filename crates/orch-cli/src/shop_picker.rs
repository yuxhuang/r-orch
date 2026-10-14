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

/// `Math.max(1, Math.min(templates.length, rows - 4))`.
pub fn page_size(total: usize, rows: u16) -> usize {
    let available = usize::from(rows).saturating_sub(4);
    available.min(total).max(1)
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
    rows: u16,
    selected: usize,
}

impl Picker {
    pub fn new(total: usize, rows: u16) -> Self {
        Picker {
            total,
            rows,
            selected: 0,
        }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn page_size(&self) -> usize {
        page_size(self.total, self.rows)
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

    /// The body of one frame: one line per visible template, plus the
    /// `n-m of total` footer when the list does not fit.
    pub fn render(&self, templates: &[AgentShopTemplate]) -> String {
        let mut out = String::new();
        for (index, template) in templates
            .iter()
            .enumerate()
            .take(self.end())
            .skip(self.start())
        {
            let cursor = if index == self.selected {
                "  ▸ "
            } else {
                "    "
            };
            out.push_str(&format!(
                "{cursor}{}{}{}\n",
                template.name,
                format_args!(" — {}", template.description),
                format_args!(" [{}]", template.tier.as_str()),
            ));
        }
        if templates.len() > self.page_size() {
            out.push_str(&format!(
                "\n  {}-{} of {}\n",
                self.start() + 1,
                self.end(),
                templates.len()
            ));
        }
        out
    }
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
    if !stdin_is_tty {
        return None;
    }

    let rows = crossterm::terminal::size()
        .map(|(_, rows)| rows)
        .unwrap_or(24);
    let mut picker = Picker::new(templates.len(), rows);

    let _raw = RawMode::enable()?;
    loop {
        draw(&picker, templates);

        let Ok(event) = crossterm::event::read() else {
            return None;
        };
        let key = match event {
            crossterm::event::Event::Key(key) => translate_key(key),
            // A resize changes the page size; redraw with the new one.
            crossterm::event::Event::Resize(_, rows) => {
                picker.rows = rows;
                continue;
            }
            _ => continue,
        };

        match picker.on_key(key) {
            PickerAction::Selected(index) => {
                clear_screen();
                return templates.get(index);
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

fn draw(picker: &Picker, templates: &[AgentShopTemplate]) {
    clear_screen();
    println!("\n  AGENT SHOP  — arrow keys to navigate, enter to select, q to cancel\n");
    print!("{}", picker.render(templates));
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
    fn the_page_size_leaves_four_rows_for_the_chrome() {
        assert_eq!(page_size(15, 24), 15);
        assert_eq!(page_size(15, 10), 6);
        // Never zero, however small the terminal claims to be.
        assert_eq!(page_size(15, 4), 1);
        assert_eq!(page_size(15, 0), 1);
        // Never more than there are templates.
        assert_eq!(page_size(3, 100), 3);
    }

    #[test]
    fn the_arrow_keys_wrap_in_both_directions() {
        let mut picker = Picker::new(3, 24);
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
        let mut picker = Picker::new(3, 24);
        picker.on_key(PickerKey::Down);
        assert_eq!(picker.on_key(PickerKey::Enter), PickerAction::Selected(1));
        assert_eq!(picker.on_key(PickerKey::Cancel), PickerAction::Cancelled);
        assert_eq!(picker.on_key(PickerKey::Other), PickerAction::Ignored);
    }

    #[test]
    fn an_empty_list_can_only_be_cancelled() {
        let mut picker = Picker::new(0, 24);
        assert_eq!(picker.on_key(PickerKey::Down), PickerAction::Ignored);
        assert_eq!(picker.on_key(PickerKey::Enter), PickerAction::Cancelled);
        assert_eq!(picker.on_key(PickerKey::Cancel), PickerAction::Cancelled);
    }

    #[test]
    fn the_window_keeps_the_selection_centred_until_it_hits_an_end() {
        // 15 templates, a page of 6.
        let mut picker = Picker::new(15, 10);
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
        let picker = Picker::new(AGENT_SHOP_TEMPLATES.len(), 10);
        let frame = picker.render(&AGENT_SHOP_TEMPLATES);
        let lines: Vec<&str> = frame.lines().collect();

        assert!(lines[0].starts_with("  ▸ "), "{:?}", lines[0]);
        assert!(lines[1].starts_with("    "), "{:?}", lines[1]);
        assert!(
            lines[0].contains(" — "),
            "the description follows an em dash"
        );
        assert!(lines[0].ends_with(']'), "the tier is bracketed");
        assert!(
            frame.contains(&format!("of {}", AGENT_SHOP_TEMPLATES.len())),
            "the footer counts the whole list"
        );
    }

    #[test]
    fn a_list_that_fits_has_no_footer() {
        let picker = Picker::new(AGENT_SHOP_TEMPLATES.len(), 100);
        let frame = picker.render(&AGENT_SHOP_TEMPLATES);
        assert!(!frame.contains(" of "), "{frame}");
        assert_eq!(frame.lines().count(), AGENT_SHOP_TEMPLATES.len());
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

    #[test]
    fn a_non_tty_stdin_cancels_immediately() {
        assert!(pick_from_shop(&AGENT_SHOP_TEMPLATES, false).is_none());
    }
}
