//! Copy routing, fixed guidance and one-row summaries (design D5, D10.4, D11).
//!
//! This module is the single source of truth shared by the transport (the
//! CLI's `EngineBackend`, which owns the native worker and OSC 52 framing) and
//! the surfaces that render a result (viewer, editor, direct field copy). It
//! holds no clipboard, no terminal and no process: routing is a pure function
//! of the requested [`CopyTarget`] and a per-request [`CopyEnv`] snapshot, and
//! the guidance strings are fixed constants, so an environment change can
//! never relabel a request that was already made.
//!
//! Every string here is ASCII or plain punctuation with no control
//! characters, so it can go through the D11 sanitizer unchanged and be
//! recognised again by exact comparison.

use super::{CopyOutcome, CopyTarget};
use crate::state::CopyFeedback;

/// Largest decoded body a terminal (OSC 52) request may carry. The base64
/// payload is then at most 87,384 bytes.
pub const TERMINAL_COPY_MAX_BYTES: usize = 64 * 1024;
/// Largest body the native worker accepts: the admitted document limit (D4).
pub const NATIVE_COPY_MAX_BYTES: usize = 1024 * 1024;
/// How long the UI waits for a native write before reporting failure. The OS
/// call itself cannot be killed; see D5.
pub const NATIVE_COPY_TIMEOUT_MS: u64 = 3_000;
/// Accepted tickets the transport may hold across queued, running and
/// completed-but-undrained states (D11 two-ticket bound).
pub const MAX_COPY_TICKETS: usize = 2;

/// The environment hints that decide Auto routing, captured once per request.
///
/// Hints are routing information only. Nothing here authorises a copy; only
/// an explicit key or footer gesture enqueues text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopyEnv {
    /// `ORCH_TUI_CLIENT=web`: the PTY behind `orch web`.
    pub web: bool,
    /// Any of `SSH_CONNECTION`, `SSH_CLIENT`, `SSH_TTY`.
    pub ssh: bool,
    /// `TMUX` is set.
    pub tmux: bool,
    /// `STY` is set (GNU screen).
    pub sty: bool,
}

impl CopyEnv {
    /// Read the hints through `get`, treating an empty value as unset.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Self {
        let set = |key: &str| get(key).is_some_and(|value| !value.is_empty());
        CopyEnv {
            web: get("ORCH_TUI_CLIENT").is_some_and(|value| value == "web"),
            ssh: set("SSH_CONNECTION") || set("SSH_CLIENT") || set("SSH_TTY"),
            tmux: set("TMUX"),
            sty: set("STY"),
        }
    }

    /// The hints of the running process.
    pub fn current() -> Self {
        CopyEnv::from_vars(|key| std::env::var(key).ok())
    }

    /// Whether Auto should ask the terminal rather than this host.
    pub fn prefers_terminal(&self) -> bool {
        self.web || self.ssh || self.tmux || self.sty
    }
}

/// Where one request goes. No route writes to two destinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyRoute {
    /// Only the host clipboard, never OSC 52 — including on failure.
    Native,
    /// Only an OSC 52 terminal request.
    Terminal,
    /// Auto on a local session: the host clipboard, falling back to the
    /// terminal only when the host clipboard is already *known* unavailable
    /// before this request is accepted. An uncertain or later native failure
    /// never falls back.
    NativeFirst,
}

/// Routing precedence (D5): explicit targets are absolute; Auto goes to the
/// terminal under any web, SSH, tmux or screen hint, otherwise native-first.
/// TMUX/STY choose the terminal on their own, whatever stale SSH variables say.
pub fn route(target: CopyTarget, env: CopyEnv) -> CopyRoute {
    match target {
        CopyTarget::Native => CopyRoute::Native,
        CopyTarget::Terminal => CopyRoute::Terminal,
        CopyTarget::Auto if env.prefers_terminal() => CopyRoute::Terminal,
        CopyTarget::Auto => CopyRoute::NativeFirst,
    }
}

// ── Fixed per-request guidance (CopyResult.guidance) ─────────────────────────

/// Terminal request without a multiplexer.
pub const GUIDANCE_TERMINAL: &str = "Copy requested from terminal; delivery depends on the terminal's OSC 52 support and is not confirmed. Or use Copy on this host";
/// Terminal request inside tmux.
pub const GUIDANCE_TMUX: &str = "Copy requested from terminal \u{2014} tmux needs set-clipboard on and terminal OSC support; or use Copy on this host";
/// Terminal request inside GNU screen.
pub const GUIDANCE_STY: &str = "Copy requested from terminal \u{2014} GNU screen delivery is unsupported by this unwrapped OSC path; use Copy on this host, or retry outside screen with a supporting terminal";
/// Terminal request with both STY and TMUX: screen wins, the tmux caveat stays.
pub const GUIDANCE_STY_TMUX: &str = "Copy requested from terminal \u{2014} GNU screen delivery is unsupported by this unwrapped OSC path, and nested tmux also needs set-clipboard on and terminal OSC support. This is a request only, never confirmed delivery; use Copy on this host, or retry outside screen with a supporting terminal";
/// Auto fell back to the terminal because this host's clipboard was already
/// known to be unavailable. Nothing was written on this host.
pub const GUIDANCE_AUTO_FALLBACK: &str = "This host's clipboard is unavailable, so copy was requested from the terminal instead; delivery depends on the terminal's OSC 52 support and is not confirmed";
/// A native write while a remote/web hint is present: "this host" is the
/// machine running orch, which may not be the attaching client.
pub const GUIDANCE_NATIVE_REMOTE: &str = "Copied on this host, the machine running orch; over SSH or orch web that may not be your local clipboard";

/// Guidance for an OSC 52 request under `env`.
pub fn terminal_guidance(env: CopyEnv) -> &'static str {
    match (env.sty, env.tmux) {
        (true, true) => GUIDANCE_STY_TMUX,
        (true, false) => GUIDANCE_STY,
        (false, true) => GUIDANCE_TMUX,
        (false, false) => GUIDANCE_TERMINAL,
    }
}

/// Guidance for a native write under `env`, if any.
pub fn native_guidance(env: CopyEnv) -> Option<&'static str> {
    (env.ssh || env.web).then_some(GUIDANCE_NATIVE_REMOTE)
}

// ── Fixed failure details (CopyOutcome::Failed / immediate rejection) ───────

/// Both accepted tickets are still occupied, or the native worker is stuck.
pub const FAIL_BUSY: &str = "Copy busy: an earlier copy has not finished";
/// Over [`TERMINAL_COPY_MAX_BYTES`]; nothing was written.
pub const FAIL_TERMINAL_TOO_LARGE: &str =
    "Copy via terminal failed: too large (over 64 KiB); select less or use Copy on this host";
/// Over [`NATIVE_COPY_MAX_BYTES`]; nothing was written.
pub const FAIL_NATIVE_TOO_LARGE: &str = "Copy on this host failed: too large (over 1 MiB)";
/// The UI stopped waiting; the OS call may still finish.
pub const FAIL_NATIVE_TIMEOUT: &str =
    "Copy on this host failed: timed out after 3 s; the clipboard may or may not have changed";
/// The host clipboard could not be opened.
pub const FAIL_NATIVE_UNAVAILABLE: &str = "Copy on this host failed: clipboard unavailable";

/// `Copy on this host failed: <reason>`, for a definite native error.
pub fn native_failure(reason: &str) -> String {
    format!("Copy on this host failed: {reason}")
}

/// `Copy via terminal failed: <reason>`, for a terminal write error.
pub fn terminal_failure(reason: &str) -> String {
    format!("Copy via terminal failed: {reason}")
}

// ── One-row summaries (D10.4, all <= 38 display cells) ──────────────────────

pub const SUMMARY_PENDING: &str = "Copy pending; F6  x";
pub const SUMMARY_NO_SELECTION: &str = "No selection; F6 all; F1 help";
pub const SUMMARY_NATIVE_OK: &str = "Copied on this host; F6";
pub const SUMMARY_NATIVE_FAILED: &str = "Host failed; F6 retry; F1 help";
pub const SUMMARY_TERMINAL: &str = "Requested from terminal; F6";
pub const SUMMARY_TMUX: &str = "Requested; tmux needs on+OSC; F6 host";
pub const SUMMARY_STY: &str = "Requested; screen unsupported; F6 host";
pub const SUMMARY_STY_TMUX: &str = "Screen unsupported; tmux on+OSC; F6";
pub const SUMMARY_FALLBACK: &str = "Host unavailable; terminal asked; F6";
pub const SUMMARY_TERMINAL_FAILED: &str = "Terminal failed; F6 host; F1 help";
pub const SUMMARY_BUSY: &str = "Copy busy; F6 retry; F1 help";
pub const SUMMARY_TOO_LARGE: &str = "Too large; F6 select less; F1 help";
pub const SUMMARY_TIMEOUT: &str = "Host timed out; F6 retry; F1 help";
pub const SUMMARY_UNAVAILABLE: &str = "Copy unavailable; F6 other; F1 help";
pub const SUMMARY_FAILED: &str = "Copy failed; F6 retry; F1 help";

/// The fixed one-row summary for a completed or immediately rejected request.
///
/// Arbitrary backend text never reaches the footer: failures are classified
/// into fixed summaries, and the full safe detail stays available to F1 help.
pub fn footer_summary(feedback: &CopyFeedback) -> &'static str {
    match &feedback.outcome {
        CopyOutcome::NativeWritten => SUMMARY_NATIVE_OK,
        CopyOutcome::TerminalRequested => match feedback.guidance.as_deref() {
            Some(GUIDANCE_TMUX) => SUMMARY_TMUX,
            Some(GUIDANCE_STY) => SUMMARY_STY,
            Some(GUIDANCE_STY_TMUX) => SUMMARY_STY_TMUX,
            Some(GUIDANCE_AUTO_FALLBACK) => SUMMARY_FALLBACK,
            _ => SUMMARY_TERMINAL,
        },
        CopyOutcome::Failed(detail) => failure_summary(feedback.request.target, detail),
    }
}

fn failure_summary(target: CopyTarget, detail: &str) -> &'static str {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("busy") {
        SUMMARY_BUSY
    } else if lower.contains("too large") {
        SUMMARY_TOO_LARGE
    } else if lower.contains("timed out") {
        SUMMARY_TIMEOUT
    } else if lower.contains("unavailable")
        || lower.contains("not available")
        || lower.contains("unsupported")
    {
        SUMMARY_UNAVAILABLE
    } else if lower.contains("terminal") || target == CopyTarget::Terminal {
        SUMMARY_TERMINAL_FAILED
    } else if lower.contains("host") || target == CopyTarget::Native {
        SUMMARY_NATIVE_FAILED
    } else {
        SUMMARY_FAILED
    }
}

/// Plain-language name of a destination, for help text.
pub fn target_label(target: CopyTarget) -> &'static str {
    match target {
        CopyTarget::Auto => "Auto",
        CopyTarget::Terminal => "Copy via terminal",
        CopyTarget::Native => "Copy on this host",
    }
}

/// The complete, static route help shown by contextual F1 (D10.5). It never
/// relabels a past request: per-request guidance is shown separately.
pub const ROUTE_HELP: &[&str] = &[
    "Copy actions (F6 opens the rail; Left/Right or Tab/BackTab choose;",
    "Home = Copy all, End = Copy on this host; Enter runs once; Esc/F6/x close):",
    "  Copy all: the whole active text, routed Auto.",
    "  Copy selection: only the selected text, routed Auto; none = no request.",
    "  Copy via terminal: selection, else all, as an OSC 52 terminal request.",
    "  Copy on this host: selection, else all, to this host's clipboard only.",
    "Ctrl+Y copies all. In the viewer y copies the selection.",
    "Auto asks the terminal under SSH, tmux, screen or orch web, else this host.",
    "A terminal request is never confirmed delivery; terminals may ignore it.",
    "tmux: needs set-clipboard on (external ignores apps) and terminal OSC support.",
    "GNU screen: this unwrapped OSC path is unsupported; use Copy on this host.",
    "This host means the machine running orch, which may not be your client.",
    "Terminal requests are limited to 64 KiB; this host accepts up to 1 MiB.",
    "Click a finished result, or x on a pending one, to hide it; F1 still shows it.",
];
