//! Palettes, semantic colour roles and glyphs (`src/tui/colors.ts`).
//!
//! The TS module keeps one mutable `tuiColors` object so already-rendered rows
//! observe a palette switch, and it has to re-map every colour *string* stored
//! on a message when the palette changes ([`remap`]). Rust stores a semantic
//! [`Token`] everywhere instead and resolves it against the live [`Palette`] at
//! draw time, which makes the remap a no-op for app state; [`remap`] is still
//! ported because it is part of the asserted surface
//! (docs/parity/tui.md §9).

use orch_domain::{GoalStatus, TaskStatus, TuiPaletteName};
use ratatui::style::Color;

/// The 26 palette tokens, in TS declaration order. The order is load-bearing:
/// [`remap`] resolves a hex to the *first* matching token, and the `light`
/// palette's `gray` / `ghost` must not collide (docs/parity/tui.md §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    Amber,
    AmberDim,
    Green,
    Red,
    Blue,
    Yellow,
    Cyan,
    Purple,
    White,
    Silver,
    Gray,
    Dim,
    Ghost,
    Void,
    ErrorBg,
    WarnBg,
    SuccessBg,
    InfoBg,
    ToolBg,
    AccentBg,
    NeutralBg,
    AlternatingRowBg,
    SolidText,
    Pink,
    Olive,
    Orange,
}

impl Token {
    pub const ALL: [Token; 26] = [
        Token::Amber,
        Token::AmberDim,
        Token::Green,
        Token::Red,
        Token::Blue,
        Token::Yellow,
        Token::Cyan,
        Token::Purple,
        Token::White,
        Token::Silver,
        Token::Gray,
        Token::Dim,
        Token::Ghost,
        Token::Void,
        Token::ErrorBg,
        Token::WarnBg,
        Token::SuccessBg,
        Token::InfoBg,
        Token::ToolBg,
        Token::AccentBg,
        Token::NeutralBg,
        Token::AlternatingRowBg,
        Token::SolidText,
        Token::Pink,
        Token::Olive,
        Token::Orange,
    ];

    pub fn index(self) -> usize {
        Token::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    /// The TS property name — used by the palette-parity tests.
    pub fn name(self) -> &'static str {
        match self {
            Token::Amber => "amber",
            Token::AmberDim => "amberDim",
            Token::Green => "green",
            Token::Red => "red",
            Token::Blue => "blue",
            Token::Yellow => "yellow",
            Token::Cyan => "cyan",
            Token::Purple => "purple",
            Token::White => "white",
            Token::Silver => "silver",
            Token::Gray => "gray",
            Token::Dim => "dim",
            Token::Ghost => "ghost",
            Token::Void => "void",
            Token::ErrorBg => "errorBg",
            Token::WarnBg => "warnBg",
            Token::SuccessBg => "successBg",
            Token::InfoBg => "infoBg",
            Token::ToolBg => "toolBg",
            Token::AccentBg => "accentBg",
            Token::NeutralBg => "neutralBg",
            Token::AlternatingRowBg => "alternatingRowBg",
            Token::SolidText => "solidText",
            Token::Pink => "pink",
            Token::Olive => "olive",
            Token::Orange => "orange",
        }
    }
}

/// `#rrggbb` values in [`Token::ALL`] order, one row per palette.
const AMBER: [u32; 26] = [
    0xffaf00, 0xaf8700, 0x5faf87, 0xd75f5f, 0x5fafd7, 0xd7af00, 0x5fd7d7, 0xaf87ff, 0xeeeeee,
    0xbcbcbc, 0x808080, 0x585858, 0x3a3a3a, 0x262626, 0x3d1515, 0x3d2e0a, 0x0f2d1f, 0x1a1a22,
    0x0f1f2d, 0x2d1f0a, 0x1a1a22, 0x1a1a1a, 0x0a0a0c, 0xd787af, 0xafaf5f, 0xd7875f,
];
const OCEAN: [u32; 26] = [
    0x5fafff, 0x5f87af, 0x5fd7af, 0xff6b6b, 0x87afff, 0xffd75f, 0x5fd7ff, 0xaf87ff, 0xf0f6ff,
    0xb8c7d9, 0x7890a8, 0x52677a, 0x304252, 0x17212b, 0x3a171d, 0x352e12, 0x102d29, 0x14283a,
    0x102f46, 0x12304a, 0x182631, 0x141d2b, 0x07131d, 0xff87d7, 0xafd787, 0xff9f5f,
];
const FOREST: [u32; 26] = [
    0x5fd787, 0x5f9f6f, 0x87d75f, 0xe06c75, 0x5fafd7, 0xd7d75f, 0x87d7af, 0xaf87d7, 0xedf5ed,
    0xb8c9b8, 0x7f967f, 0x536b57, 0x34483a, 0x1b271e, 0x3a191c, 0x303013, 0x12351d, 0x1a2b20,
    0x15302a, 0x14351f, 0x1c2920, 0x172219, 0x09150c, 0xd787af, 0xafd75f, 0xd79f5f,
];
const VIOLET: [u32; 26] = [
    0xaf87ff, 0x7f5faf, 0x5fd7a7, 0xff6b87, 0x8787ff, 0xffd75f, 0x5fd7d7, 0xd787ff, 0xf4efff,
    0xc7bbd9, 0x9380a8, 0x655477, 0x433552, 0x251b2d, 0x401724, 0x382c12, 0x123026, 0x271d36,
    0x1c2342, 0x30204a, 0x261e30, 0x1d1826, 0x130a1d, 0xff87d7, 0xafd787, 0xff9f7f,
];
const LIGHT: [u32; 26] = [
    0x9a6700, 0x7a5200, 0x1a7f37, 0xb42318, 0x175cd3, 0x946200, 0x007a85, 0x6941c6, 0x1d2939,
    0x344054, 0x5d6678, 0x475467, 0x667085, 0xf2f4f7, 0xfef3f2, 0xfffaeb, 0xecfdf3, 0xeff8ff,
    0xeef4ff, 0xfff7e0, 0xf2f4f7, 0xf8fafc, 0xffffff, 0xc11574, 0x5f6f13, 0xb54708,
];

/// Raw `#rrggbb` values for a palette, in [`Token::ALL`] order.
pub fn palette_hexes(name: TuiPaletteName) -> &'static [u32; 26] {
    match name {
        TuiPaletteName::Amber => &AMBER,
        TuiPaletteName::Ocean => &OCEAN,
        TuiPaletteName::Forest => &FOREST,
        TuiPaletteName::Violet => &VIOLET,
        TuiPaletteName::Light => &LIGHT,
    }
}

/// The live palette. Swapping it is the Rust equivalent of `applyTuiPalette`:
/// nothing stores resolved colours, so every row picks up the new values on the
/// next draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub name: TuiPaletteName,
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            name: TuiPaletteName::Amber,
        }
    }
}

impl Palette {
    pub fn new(name: TuiPaletteName) -> Self {
        Palette { name }
    }

    pub fn hex(&self, token: Token) -> u32 {
        palette_hexes(self.name)[token.index()]
    }

    pub fn hex_string(&self, token: Token) -> String {
        format!("#{:06x}", self.hex(token))
    }

    pub fn get(&self, token: Token) -> Color {
        let hex = self.hex(token);
        Color::Rgb(
            ((hex >> 16) & 0xff) as u8,
            ((hex >> 8) & 0xff) as u8,
            (hex & 0xff) as u8,
        )
    }

    /// `getAgentColors()` — eight identity colours, assigned by the agent's
    /// position in the live agent list (`index % 8`).
    pub const AGENT_TOKENS: [Token; 8] = [
        Token::Green,
        Token::Blue,
        Token::Purple,
        Token::Yellow,
        Token::Cyan,
        Token::Pink,
        Token::Olive,
        Token::Orange,
    ];

    pub fn agent_token(index: usize) -> Token {
        Palette::AGENT_TOKENS[index % 8]
    }
}

/// `remapTuiColor(hex, from, to)` — resolve a stored hex back to its token in
/// the `from` palette and return the `to` palette's value; an unknown hex
/// passes through unchanged.
pub fn remap(hex: u32, from: TuiPaletteName, to: TuiPaletteName) -> u32 {
    let from_hexes = palette_hexes(from);
    match from_hexes.iter().position(|h| *h == hex) {
        Some(idx) => palette_hexes(to)[idx],
        None => hex,
    }
}

/// `getTaskStatusColor`.
pub fn task_status_token(status: TaskStatus) -> Token {
    match status {
        TaskStatus::InProgress | TaskStatus::Done => Token::Green,
        TaskStatus::Retrying => Token::Yellow,
        TaskStatus::Review => Token::Blue,
        TaskStatus::Todo | TaskStatus::Cancelled => Token::Dim,
        TaskStatus::Failed => Token::Red,
    }
}

/// `getGoalStatusColor`.
pub fn goal_status_token(status: GoalStatus) -> Token {
    match status {
        GoalStatus::Active => Token::Green,
        GoalStatus::Paused => Token::Dim,
        GoalStatus::Achieved => Token::Amber,
        GoalStatus::Abandoned => Token::Ghost,
    }
}

/// Agent-status colour (docs/parity/tui.md §9).
pub fn agent_status_token(status: orch_domain::AgentStatus) -> Token {
    match status {
        orch_domain::AgentStatus::Idle => Token::Dim,
        orch_domain::AgentStatus::Running => Token::Green,
        orch_domain::AgentStatus::Error => Token::Red,
        orch_domain::AgentStatus::Disabled | orch_domain::AgentStatus::Offline => Token::Ghost,
    }
}

/// Priority pip colour: 1 red, 2 yellow, 3 dim, 4 (and anything unknown) ghost.
pub fn priority_token(priority: i64) -> Token {
    match priority {
        1 => Token::Red,
        2 => Token::Yellow,
        3 => Token::Dim,
        _ => Token::Ghost,
    }
}

/// Priority pips: `!!!`, `!!`, `!`, `·`; an unknown priority falls back to P4.
pub fn priority_pips(priority: i64) -> &'static str {
    match priority {
        1 => "!!!",
        2 => "!!",
        3 => "!",
        _ => "·",
    }
}

// ── Glyphs (docs/parity/tui.md §9) ──────────────────────────────────────────

pub const HEAVY_RULE: char = '━';
pub const LIGHT_RULE: char = '─';
pub const DOT: &str = "·";
pub const LOZENGE: &str = "◈";
pub const STAR: &str = "★";
pub const LOOP: &str = "⟳";
pub const DIAMOND: &str = "◆";
pub const DIAMOND_OUTLINE: &str = "◇";
pub const BULLSEYE: &str = "◉";
pub const PAUSE_BARS: &str = "‖";
pub const CIRCLE_FILLED: &str = "●";
pub const CIRCLE: &str = "○";
pub const CHECK: &str = "✓";
pub const CROSS: &str = "✕";
pub const BALLOT_X: &str = "✗";
pub const RETRY_ARROW: &str = "↻";
pub const UNDO_ARROW: &str = "↶";
pub const TRIANGLE_RIGHT: &str = "▶";
pub const CURSOR: &str = "▸";
pub const TRIANGLE_DOWN: &str = "▼";
pub const PIPE: &str = "│";
pub const ARROW_LEFT: &str = "←";
pub const ARROW_RIGHT: &str = "→";
pub const GEAR: &str = "⚙";
pub const PENCIL: &str = "✎";
pub const BLOCK: &str = "█";
pub const SHADE: &str = "░";
pub const ACCENT_BAR: &str = "▍";
pub const THIN_BAR: &str = "▏";
pub const PLUS_CIRCLE: &str = "⊕";
pub const WARN: &str = "⚠";
pub const HOURGLASS: &str = "⏳";
pub const EM_DASH: &str = "—";
/// A newline inside a command-bar value — pasted bodies keep their line
/// structure, and the bar is a single *visual* line (design-v2 §3.9.3).
pub const RETURN_GLYPH: &str = "⏎";
pub const ELLIPSIS: char = '…';
pub const PAPERCLIP: &str = "📎";
pub const BRAIN: &str = "🧠";
pub const SIGMA: &str = "Σ";

/// The ten braille spinner frames; frame 0 is `⠋`.
pub const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// `⠋` … — `tick % 10`.
pub fn spinner_frame(tick: u64) -> &'static str {
    SPINNER_FRAMES[(tick % 10) as usize]
}

pub const SPARKLINE_RAMP: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

pub fn heavy_rule(len: usize) -> String {
    HEAVY_RULE.to_string().repeat(len)
}

pub fn light_rule(len: usize) -> String {
    LIGHT_RULE.to_string().repeat(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_palette_defines_every_token() {
        for name in orch_domain::TUI_PALETTE_NAMES {
            let p = Palette::new(name);
            for token in Token::ALL {
                // All 26 tokens resolve; a missing row would panic on index.
                let _ = p.get(token);
                assert_eq!(p.hex_string(token).len(), 7, "{}", token.name());
            }
        }
    }

    #[test]
    fn light_palette_concrete_values() {
        // docs/parity/tui.md §11.1 (colors.test.ts).
        let light = Palette::new(TuiPaletteName::Light);
        assert_eq!(light.hex_string(Token::White), "#1d2939");
        assert_eq!(light.hex_string(Token::Ghost), "#667085");
        assert_eq!(light.hex_string(Token::AlternatingRowBg), "#f8fafc");
        assert_eq!(light.hex_string(Token::Gray), "#5d6678");
    }

    #[test]
    fn light_gray_and_ghost_stay_distinct() {
        let light = Palette::new(TuiPaletteName::Light);
        assert_ne!(light.hex(Token::Gray), light.hex(Token::Ghost));
    }

    #[test]
    fn remap_round_trips_semantic_tokens() {
        let amber = Palette::new(TuiPaletteName::Amber);
        let ocean = Palette::new(TuiPaletteName::Ocean);
        let green = amber.hex(Token::Green);
        assert_eq!(
            remap(green, TuiPaletteName::Amber, TuiPaletteName::Ocean),
            ocean.hex(Token::Green)
        );
        // Unknown hex passes through.
        assert_eq!(
            remap(0x123456, TuiPaletteName::Amber, TuiPaletteName::Ocean),
            0x123456
        );
    }

    #[test]
    fn switching_palette_changes_derived_lookups() {
        let amber = Palette::new(TuiPaletteName::Amber);
        let forest = Palette::new(TuiPaletteName::Forest);
        let token = task_status_token(TaskStatus::Done);
        assert_ne!(amber.get(token), forest.get(token));
        assert_eq!(
            Palette::agent_token(0),
            Token::Green,
            "first agent colour is green in every palette"
        );
    }

    #[test]
    fn solid_text_meets_wcag_contrast_in_the_light_palette() {
        // colors.test.ts: solidText against amber/green/red/blue >= 4.5:1.
        fn luminance(hex: u32) -> f64 {
            let channel = |c: u32| {
                let s = c as f64 / 255.0;
                if s <= 0.03928 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel((hex >> 16) & 0xff)
                + 0.7152 * channel((hex >> 8) & 0xff)
                + 0.0722 * channel(hex & 0xff)
        }
        let light = Palette::new(TuiPaletteName::Light);
        let fg = luminance(light.hex(Token::SolidText));
        for token in [Token::Amber, Token::Green, Token::Red, Token::Blue] {
            let bg = luminance(light.hex(token));
            let (hi, lo) = if fg > bg { (fg, bg) } else { (bg, fg) };
            let ratio = (hi + 0.05) / (lo + 0.05);
            assert!(ratio >= 4.5, "{} contrast {ratio}", token.name());
        }
    }

    #[test]
    fn spinner_frames_cycle_from_the_first_braille_frame() {
        assert_eq!(spinner_frame(0), "⠋");
        assert_eq!(spinner_frame(10), "⠋");
        assert_eq!(spinner_frame(3), "⠸");
    }

    #[test]
    fn priority_pips_fall_back_to_p4() {
        assert_eq!(priority_pips(1), "!!!");
        assert_eq!(priority_pips(2), "!!");
        assert_eq!(priority_pips(3), "!");
        assert_eq!(priority_pips(4), "·");
        assert_eq!(priority_pips(99), "·");
        assert_eq!(priority_token(99), Token::Ghost);
    }
}
