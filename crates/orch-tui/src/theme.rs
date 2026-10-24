//! Palettes, semantic colour roles and glyphs.
//!
//! Messages store a semantic [`Token`] rather than a colour string and resolve
//! it against the live [`Palette`] at draw time, which makes the remap of
//! stored colours ([`remap`]) a no-op for app state; [`remap`] is kept because
//! it is part of the asserted surface.

use orch_domain::{GoalStatus, TaskStatus, TuiPaletteName};
use ratatui::style::Color;

/// The 26 palette tokens, in declaration order. The order is load-bearing:
/// [`remap`] resolves a hex to the *first* matching token, and the `light`
/// palette's `gray` / `ghost` must not collide.
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

    /// The camelCase token name — used by the palette tests.
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

// Theme-derived palettes. The theme's documented colours fill the foreground
// tokens and `void` (the theme background). The tinted backgrounds (`errorBg`
// ... `alternatingRowBg`) mix a theme colour into that background; every other
// token not taken from the theme is listed as "Derived" above its table.
// Upstream sources and licences are in THIRD-PARTY-NOTICES.md.
//
// Catppuccin Latte (catppuccin/palette `palette.json`, MIT). Derived: `amberDim`,
// `olive`; `solidText` is white. `green` is Latte green mixed with 16% black
// (#40a02b -> #368624) so `solidText` reaches 4.5:1 on it (3.3:1 before).
const CATPPUCCIN_LATTE: [u32; 26] = [
    0x8839ef, 0xb183f1, 0x368624, 0xd20f39, 0x1e66f5, 0xdf8e1d, 0x04a5e5, 0x7287fd, 0x4c4f69,
    0x5c5f77, 0x6c6f85, 0x7c7f93, 0x9ca0b0, 0xeff1f5, 0xecd6de, 0xede5db, 0xd9e4dc, 0xdee6f5,
    0xd8e9f3, 0xe3dbf4, 0xe5e7ed, 0xeaecf1, 0xffffff, 0xea76cb, 0x7d8f24, 0xfe640b,
];
// Catppuccin Mocha (catppuccin/palette `palette.json`, MIT). Derived:
// `amberDim`, `olive`. `amber` and `purple` are both Mauve.
const CATPPUCCIN_MOCHA: [u32; 26] = [
    0xcba6f7, 0x7d699d, 0xa6e3a1, 0xf38ba8, 0x89b4fa, 0xf9e2af, 0x89dceb, 0xcba6f7, 0xcdd6f4,
    0xbac2de, 0xa6adc8, 0x7f849c, 0x585b70, 0x1e1e2e, 0x493446, 0x4a4548, 0x394545, 0x2d334b,
    0x2f3c4c, 0x4b4162, 0x303042, 0x272738, 0x11111b, 0xf5c2e7, 0xc6d289, 0xfab387,
];
// Dracula (dracula/dracula-theme `README.md`, MIT). Derived: `amberDim`,
// `silver`, `gray`, `olive`, `solidText` (the background darkened), and `blue`
// (Dracula has none: the midpoint of its cyan and purple). `amber` and
// `purple` are both Purple. `dim` mixes Comment with 42% white for TODO chips.
const DRACULA: [u32; 26] = [
    0xbd93f9, 0x7a64a1, 0x50fa7b, 0xff5555, 0xa4befb, 0xf1fa8c, 0x8be9fd, 0xbd93f9, 0xf8f8f2,
    0xcecfcc, 0xa9aaab, 0xa4adca, 0x44475a, 0x282a36, 0x53333c, 0x505447, 0x305444, 0x393f52,
    0x384956, 0x4f4569, 0x3d3f49, 0x32343f, 0x191a21, 0xff79c6, 0xb8c76b, 0xffb86c,
];
// Gruvbox dark (gruvbox-community/gruvbox `colors/gruvbox.vim`, MIT). `purple`
// is neutral_purple and `pink` bright_purple, so the two stay apart.
const GRUVBOX_DARK: [u32; 26] = [
    0xfabd2f, 0xb57614, 0xb8bb26, 0xfb4934, 0x83a598, 0xd79921, 0x8ec07c, 0xb16286, 0xfbf1c7,
    0xd5c4a1, 0xa89984, 0x928374, 0x665c54, 0x282828, 0x522f2a, 0x4b3f27, 0x454528, 0x353a38,
    0x384035, 0x524629, 0x3c3a36, 0x32312f, 0x1d2021, 0xd3869b, 0x98971a, 0xfe8019,
];
// Nord (nordtheme/nord `src/nord.css`, MIT). Derived: `gray`, `dim`, `pink`,
// `olive`, and `red` (nord11 mixed with 41% white for FAIL chips).
// `solidText` is black: no single Nord colour reaches 4.5:1 on all of
// `amber`, `green`, `red` and `blue`.
const NORD: [u32; 26] = [
    0x88c0d0, 0x81a1c1, 0xa3be8c, 0xd9a2a7, 0x5e81ac, 0xebcb8b, 0x8fbcbb, 0xb48ead, 0xeceff4,
    0xd8dee9, 0x979da9, 0x7a808c, 0x4c566a, 0x2e3440, 0x4b3d48, 0x54524f, 0x45504f, 0x353f4f,
    0x3e4a54, 0x40505d, 0x3f4551, 0x363c48, 0x000000, 0xe0aec3, 0xaca553, 0xd08770,
];
// One Dark (joshdick/onedark.vim `autoload/onedark.vim`, MIT). Derived:
// `amberDim`, `white` (its foreground lightened), `gray`, `pink`, `olive`.
// `amber` and `blue` are both Blue; `solidText` is black (the background is
// below 4.5:1 on Red). `gray` mixes its derived #797f8a with 36% white; `dim` mixes
// #5c6370 with 42% white for labels and TODO chips.
const ONE_DARK: [u32; 26] = [
    0x61afef, 0x47749b, 0x98c379, 0xe06c75, 0x61afef, 0xe5c07b, 0x56b6c2, 0xc678dd, 0xcdd1d9,
    0xabb2bf, 0xa9adb4, 0xa0a5ac, 0x4b5263, 0x282c34, 0x4d3941, 0x4e4a42, 0x3e4a42, 0x303e4e,
    0x2f424b, 0x374e65, 0x353942, 0x2f333b, 0x000000, 0xe996bf, 0xacac53, 0xd19a66,
];
// Solarized dark (altercation/solarized `README.md`, MIT). Derived: `amberDim`,
// `olive`. `amber` and `yellow` are both Yellow; `neutralBg` is base02.
// `solidText` is black: no single Solarized colour reaches 4.5:1 on all four
// chip colours.
const SOLARIZED_DARK: [u32; 26] = [
    0xb58900, 0x6d6316, 0x859900, 0xdc322f, 0x268bd2, 0xb58900, 0x2aa198, 0x6c71c4, 0xeee8d5,
    0x93a1a1, 0x839496, 0x657b83, 0x586e75, 0x002b36, 0x2c2c35, 0x243e2b, 0x1b412b, 0x05384c,
    0x073e46, 0x364726, 0x073642, 0x04303c, 0x000000, 0xd33682, 0x849550, 0xcb4b16,
];
// Solarized light (altercation/solarized `README.md`, MIT): the dark variant's
// accents and black `solidText`, base02..base1 as the text ramp, base3 as the
// background and base2 as `neutralBg`. Derived: `amberDim`, `olive`,
// `gray` (base00 mixed with 11% black), `dim` (base0 with 30% black).
const SOLARIZED_LIGHT: [u32; 26] = [
    0xb58900, 0xd2b55b, 0x859900, 0xdc322f, 0x268bd2, 0xb58900, 0x2aa198, 0x6c71c4, 0x073642,
    0x586e75, 0x5a6d75, 0x5c6869, 0x93a1a1, 0xfdf6e3, 0xf9decd, 0xf4e9c8, 0xefebc8, 0xecede2,
    0xe8eedc, 0xefe0b6, 0xeee8d5, 0xf6efdc, 0x000000, 0xd33682, 0x849550, 0xcb4b16,
];
// Tokyo Night (enkia/tokyo-night-vscode-theme `README.md` and
// `themes/tokyo-night-color-theme.json`, MIT). Derived: `pink`. `olive` is the
// theme's teal (#73daca); `amber` and `blue` are both Blue; `solidText` is the
// side-bar background (#16161e). `dim` mixes Comment with 33% white
// for labels and TODO chips.
const TOKYO_NIGHT: [u32; 26] = [
    0x7aa2f7, 0x3d59a1, 0x9ece6a, 0xf7768e, 0x7aa2f7, 0xe0af68, 0x7dcfff, 0xbb9af7, 0xc0caf5,
    0xa9b1d6, 0x9aa5ce, 0x8e94b0, 0x414868, 0x1a1b26, 0x462d3b, 0x423933, 0x343f34, 0x272e43,
    0x2a3849, 0x333e5c, 0x282a38, 0x21222f, 0x16161e, 0xf49ac7, 0x73daca, 0xff9e64,
];

/// Raw `#rrggbb` values for a palette, in [`Token::ALL`] order.
pub fn palette_hexes(name: TuiPaletteName) -> &'static [u32; 26] {
    match name {
        TuiPaletteName::Amber => &AMBER,
        TuiPaletteName::Ocean => &OCEAN,
        TuiPaletteName::Forest => &FOREST,
        TuiPaletteName::Violet => &VIOLET,
        TuiPaletteName::Light => &LIGHT,
        TuiPaletteName::CatppuccinLatte => &CATPPUCCIN_LATTE,
        TuiPaletteName::CatppuccinMocha => &CATPPUCCIN_MOCHA,
        TuiPaletteName::Dracula => &DRACULA,
        TuiPaletteName::GruvboxDark => &GRUVBOX_DARK,
        TuiPaletteName::Nord => &NORD,
        TuiPaletteName::OneDark => &ONE_DARK,
        TuiPaletteName::SolarizedDark => &SOLARIZED_DARK,
        TuiPaletteName::SolarizedLight => &SOLARIZED_LIGHT,
        TuiPaletteName::TokyoNight => &TOKYO_NIGHT,
    }
}

/// The live palette. Swapping it takes effect on the next draw:
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

    /// Eight identity colours, assigned by the agent's
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

/// Resolve a stored hex back to its token in
/// the `from` palette and return the `to` palette's value; an unknown hex
/// passes through unchanged.
pub fn remap(hex: u32, from: TuiPaletteName, to: TuiPaletteName) -> u32 {
    let from_hexes = palette_hexes(from);
    match from_hexes.iter().position(|h| *h == hex) {
        Some(idx) => palette_hexes(to)[idx],
        None => hex,
    }
}

/// The colour token for a task status.
pub fn task_status_token(status: TaskStatus) -> Token {
    match status {
        TaskStatus::InProgress | TaskStatus::Done => Token::Green,
        TaskStatus::Retrying => Token::Yellow,
        TaskStatus::Review => Token::Blue,
        TaskStatus::Todo | TaskStatus::Cancelled => Token::Dim,
        TaskStatus::Failed => Token::Red,
    }
}

/// The colour of a remote phase, which is **not** the task's status colour:
/// a dispatched or queued task is `in_progress` locally, and painting it
/// green is the same lie as calling it `RUN`.
pub fn remote_phase_token(phase: orch_domain::RemotePhase) -> Token {
    match phase {
        orch_domain::RemotePhase::Dispatched => Token::Cyan,
        orch_domain::RemotePhase::Queued => Token::Dim,
        orch_domain::RemotePhase::Running => Token::Green,
        orch_domain::RemotePhase::Unknown => Token::Yellow,
        orch_domain::RemotePhase::Terminal => Token::Silver,
    }
}

/// The colour token for a goal status.
pub fn goal_status_token(status: GoalStatus) -> Token {
    match status {
        GoalStatus::Active => Token::Green,
        GoalStatus::Paused => Token::Dim,
        GoalStatus::Achieved => Token::Amber,
        GoalStatus::Abandoned => Token::Ghost,
    }
}

/// Agent-status colour.
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

// ── Glyphs ──────────────────────────────────────────

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

    /// WCAG 2 relative luminance of a `#rrggbb` value.
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

    fn contrast(a: u32, b: u32) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn solid_text_meets_wcag_contrast_in_every_palette() {
        // solidText is drawn on amber (active tab) and on green/red/blue
        // (tab flash) chips: >= 4.5:1 on each.
        for name in orch_domain::TUI_PALETTE_NAMES {
            let p = Palette::new(name);
            for token in [Token::Amber, Token::Green, Token::Red, Token::Blue] {
                let ratio = contrast(p.hex(Token::SolidText), p.hex(token));
                assert!(
                    ratio >= 4.5,
                    "{name}: solidText on {} {ratio:.2}",
                    token.name()
                );
            }
        }
    }

    #[test]
    fn corrected_operational_text_stays_readable_on_its_surfaces() {
        // F1: solidText alone misses TODO/FAIL chips and supporting labels.
        // Keep a 4.5:1 floor for these text pairs on the intended theme base
        // and the backgrounds actually used by the table/chip renderers.
        let mut failures = Vec::new();
        for (name, foreground, backgrounds) in [
            (TuiPaletteName::Dracula, Token::Dim, &[Token::NeutralBg][..]),
            (TuiPaletteName::Nord, Token::Red, &[Token::ErrorBg][..]),
            (
                TuiPaletteName::OneDark,
                Token::Dim,
                &[Token::NeutralBg, Token::Void, Token::AlternatingRowBg][..],
            ),
            (
                TuiPaletteName::OneDark,
                Token::Gray,
                &[Token::Void, Token::AlternatingRowBg][..],
            ),
            (
                TuiPaletteName::SolarizedLight,
                Token::Dim,
                &[Token::NeutralBg, Token::Void, Token::AlternatingRowBg][..],
            ),
            (
                TuiPaletteName::SolarizedLight,
                Token::Gray,
                &[Token::Void, Token::AlternatingRowBg][..],
            ),
            (
                TuiPaletteName::TokyoNight,
                Token::Dim,
                &[Token::NeutralBg, Token::Void, Token::AlternatingRowBg][..],
            ),
        ] {
            let p = Palette::new(name);
            for background in backgrounds {
                let ratio = contrast(p.hex(foreground), p.hex(*background));
                if ratio < 4.5 {
                    failures.push(format!(
                        "{name}: {} on {} {ratio:.2}",
                        foreground.name(),
                        background.name()
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn gray_and_ghost_stay_distinct_in_every_palette() {
        for name in orch_domain::TUI_PALETTE_NAMES {
            let p = Palette::new(name);
            assert_ne!(p.hex(Token::Gray), p.hex(Token::Ghost), "{name}");
        }
    }

    #[test]
    fn the_eight_agent_colours_are_pairwise_distinct_in_every_palette() {
        for name in orch_domain::TUI_PALETTE_NAMES {
            let p = Palette::new(name);
            for (i, a) in Palette::AGENT_TOKENS.iter().enumerate() {
                for b in &Palette::AGENT_TOKENS[i + 1..] {
                    assert_ne!(p.hex(*a), p.hex(*b), "{name}: {} = {}", a.name(), b.name());
                }
            }
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
