//! `/msg` — the recipient grammar, its quoter, and recipient resolution
//! (design-v2 §3.2, §3.4).
//!
//! Pure: `&[Agent]` in, a decision out. It is the only place that knows the
//! quoting rules, and `quote_recipient ∘ parse_recipient` is an identity on
//! every legal agent name — which matters because autocomplete inserts the
//! quoted form and agent names may contain spaces, quotes, backslashes and
//! emoji (`agent_service.rs:82` trims and rejects empty; nothing else).
//!
//! Two rules carry the safety of the whole feature:
//!
//! 1. **A prefix never commits.** Resolution commits only on an id, an
//!    NFC-exact name, or a *unique* case-insensitive name. A prefix match at
//!    send time is a did-you-mean refusal ([`ResolveError::DidYouMean`]).
//!    Prefixes are for the suggestion panel, where the user sees the row
//!    before committing.
//! 2. **An unquoted exact match shadowed by a longer name is refused**
//!    ([`ResolveError::Shadowed`]). With agents `Code` and `Code Reviewer`,
//!    `/msg Code Reviewer please look` commits to `Code` on exact match and
//!    eats the body's first word — the one remaining path on which a body
//!    could reach an agent the user did not name.
//!
//! Not `shlex` (ARCHITECTURE §4.8 criterion (a)): we need one token plus the
//! **verbatim remainder of the line**, and `shlex::split` consumes the whole
//! input — destroying the body's own quoting and whitespace — while
//! `shlex::Shlex` exposes no byte offset to recover the remainder from. Its
//! POSIX `#`-comment rule would also silently drop a body beginning `#1 is
//! broken`, and an unterminated quote yields `None` with no position where we
//! need both a specific error and an in-progress partial for autocomplete.

use std::borrow::Cow;

use orch_domain::{Agent, AgentStatus};
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};

use crate::theme::Token;

/// The broadcast keyword: **exactly** this, unquoted. `ALL`, `All` and `@all`
/// fall through to name resolution and fail safely, because broadcast is the
/// one irreversible action here (design-v2 §3.4.4).
pub const BROADCAST_KEYWORD: &str = "all";

/// At most this many candidates are named in an ambiguity or did-you-mean
/// error; the rest become `… and N more`.
const MAX_LISTED: usize = 5;

/// The recipient token at the head of a `/msg` tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientToken {
    /// The decoded name: quotes removed, `\"` and `\\` resolved.
    pub text: String,
    /// Written in quotes. A quoted token never broadcasts and never triggers
    /// the shadow check — it is unambiguous by construction.
    pub quoted: bool,
    /// The token is **followed by something**: a closing quote, whitespace or
    /// a body. `false` while the token is still the last thing on the line,
    /// which is what keeps the recipient chip quiet until the user has
    /// finished typing a name (`/msg Alic` claims nothing about `Alice`).
    pub complete: bool,
    /// Whitespace followed the token, so the body has begun — even if it is
    /// empty. This is what closes the suggestion panel once the body starts.
    pub body_started: bool,
    /// The body: everything after the whitespace run that follows the token,
    /// verbatim.
    pub rest: String,
}

/// Why a `/msg` tail could not be tokenized. The message is the contract — the
/// TUI renders it as typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// `/msg` with nothing after it.
    Missing,
    /// `/msg "Code Rev hello`, `/msg 'don't hi`, `/msg "say \`
    UnclosedQuote,
    /// `/msg "Alice"hello`
    JunkAfterQuote,
    /// `/msg "" hi`
    EmptyRecipient,
}

impl ParseError {
    pub fn message(self) -> &'static str {
        match self {
            ParseError::Missing => "Usage: /msg <agent|all> <message>",
            ParseError::UnclosedQuote => {
                "Unclosed quote in recipient — write /msg \"Code Reviewer\" <message>"
            }
            ParseError::JunkAfterQuote => "Expected a space after the quoted name",
            ParseError::EmptyRecipient => "Recipient is empty",
        }
    }
}

/// Who the message is for, once resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    /// Every enabled agent. `enabled` is the fan-out the confirm step quotes.
    Broadcast { enabled: usize },
    Agent {
        id: String,
        name: String,
        disabled: bool,
    },
}

/// One named candidate in an error: `agt_a1b2 Alice`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub id: String,
    pub name: String,
}

/// Why a token could not be resolved to exactly one recipient. Every variant
/// renders as one or more feed lines; none of them ever falls back to the
/// current selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// Two agents match at the same commit stage.
    Ambiguous { text: String, found: Vec<Named> },
    /// Nothing committed, but exactly one name starts with the token.
    DidYouMean { text: String, name: String },
    /// Nothing committed and several names start with the token.
    SeveralStartWith { text: String, found: Vec<Named> },
    /// Nothing committed and nothing is close.
    NoMatch { text: String },
    /// An unquoted exact match whose name is a prefix of another agent's, with
    /// a body that continues that longer name.
    Shadowed {
        short: String,
        long: String,
        /// The body with the rest of the longer name still at its head.
        short_body: String,
        /// The body with that part removed — the longer name's reading.
        long_body: String,
    },
    /// A broadcast with nobody to send to, refused before the backend is
    /// called rather than reported after the fact.
    NoEnabledAgents,
}

impl ResolveError {
    /// The feed lines, in order. Only [`ResolveError::Shadowed`] has more than
    /// one.
    pub fn lines(&self) -> Vec<String> {
        match self {
            ResolveError::Ambiguous { text, found } => vec![format!(
                "Ambiguous recipient \"{text}\" — {} agents match: {} (use the id)",
                found.len(),
                list(found)
            )],
            ResolveError::DidYouMean { text, name } => vec![format!(
                "No agent named \"{text}\" — did you mean \"{name}\"? \
                 Quote names with spaces: /msg \"{name}\" <message>"
            )],
            ResolveError::SeveralStartWith { text, found } => vec![format!(
                "No agent named \"{text}\" — {} agents start with it: {}",
                found.len(),
                list(found)
            )],
            ResolveError::NoMatch { text } => {
                vec![format!(
                    "No agent matches \"{text}\" — /agent list to see names"
                )]
            }
            ResolveError::Shadowed {
                short,
                long,
                short_body,
                long_body,
            } => vec![
                format!("\"{short}\" is an agent, and so is \"{long}\" — quote the one you mean:"),
                // Both lines are quoted even where the quoter would go bare:
                // the quotes are what the two readings differ by, so showing
                // `/msg Code Reviewer …` as the fix would show the input again.
                format!("  /msg {} {short_body}", force_quoted(short)),
                format!("  /msg {} {long_body}", force_quoted(long)),
            ],
            ResolveError::NoEnabledAgents => vec!["No enabled agents to message".to_owned()],
        }
    }
}

/// [`quote_recipient`], but never bare — for messages whose whole point is
/// where the quotes go.
fn force_quoted(name: &str) -> String {
    let quoted = quote_recipient(name);
    if quoted.starts_with('"') {
        return quoted;
    }
    format!("\"{quoted}\"")
}

fn list(found: &[Named]) -> String {
    let mut parts: Vec<String> = found
        .iter()
        .take(MAX_LISTED)
        .map(|c| format!("{} {}", c.id, c.name))
        .collect();
    if found.len() > MAX_LISTED {
        parts.push(format!("… and {} more", found.len() - MAX_LISTED));
    }
    parts.join(", ")
}

// ── Tokenizer ──────────────────────────────────────────────────────────────

/// Tokenize a `/msg` tail strictly: every malformed form is an error.
pub fn parse_recipient(tail: &str) -> Result<RecipientToken, ParseError> {
    let token = tokenize(tail).ok_or(ParseError::Missing)?;
    match token {
        Tokenized::Complete(token) => {
            if token.text.is_empty() {
                return Err(ParseError::EmptyRecipient);
            }
            Ok(token)
        }
        Tokenized::Unclosed(_) => Err(ParseError::UnclosedQuote),
        Tokenized::Junk => Err(ParseError::JunkAfterQuote),
    }
}

/// Tokenize for *suggestions*, where an unterminated token is a legal
/// in-progress state rather than an error: `/msg "Code R` reports `Code R`
/// with `complete: false`, and a pending `\` is dropped.
///
/// `None` only when there is nothing to work with at all.
pub fn parse_partial(tail: &str) -> Option<RecipientToken> {
    match tokenize(tail)? {
        Tokenized::Complete(token) | Tokenized::Unclosed(token) => Some(token),
        // `"Alice"x` — the token is complete and the junk is the user's
        // problem to fix; filtering on `Alice` is still the useful answer.
        Tokenized::Junk => None,
    }
}

enum Tokenized {
    Complete(RecipientToken),
    Unclosed(RecipientToken),
    Junk,
}

fn tokenize(tail: &str) -> Option<Tokenized> {
    let tail = tail.trim_start();
    let mut chars = tail.char_indices().peekable();
    let (quote, start) = match chars.peek().copied() {
        None => return None,
        Some((idx, ch @ ('"' | '\''))) => {
            chars.next();
            (Some(ch), idx + ch.len_utf8())
        }
        Some((idx, _)) => (None, idx),
    };

    let mut text = String::new();
    let mut end = None;
    match quote {
        // A bare token runs to the first Unicode whitespace. `\` is an
        // ordinary character here — bare tokens have no escapes.
        None => {
            for (idx, ch) in chars.by_ref() {
                if ch.is_whitespace() {
                    end = Some(idx);
                    break;
                }
                text.push(ch);
            }
        }
        // Double quotes take `\"` and `\\`. A `\` before anything else is a
        // literal backslash, so `"C:\path"` means what it looks like; the
        // quoter still emits the escaped form, which keeps the round trip
        // exact either way.
        Some('"') => {
            let mut escaped = false;
            for (idx, ch) in chars.by_ref() {
                if escaped {
                    if ch != '"' && ch != '\\' {
                        text.push('\\');
                    }
                    text.push(ch);
                    escaped = false;
                    continue;
                }
                match ch {
                    '\\' => escaped = true,
                    '"' => {
                        end = Some(idx + ch.len_utf8());
                        break;
                    }
                    _ => text.push(ch),
                }
            }
            // A trailing `\` leaves the token unterminated, which is why the
            // quoter quotes on *any* backslash: the question never arises for
            // text it produced.
            if escaped && end.is_none() {
                return Some(Tokenized::Unclosed(partial(text, true, "")));
            }
        }
        // Single quotes are literal throughout.
        Some(_) => {
            for (idx, ch) in chars.by_ref() {
                if ch == '\'' {
                    end = Some(idx + ch.len_utf8());
                    break;
                }
                text.push(ch);
            }
        }
    }

    let quoted = quote.is_some();
    let Some(end) = end else {
        // Ran out of input. For a bare token that is simply the end of the
        // line; for a quoted one it is unterminated.
        let token = partial(text, quoted, "");
        return Some(if quoted {
            Tokenized::Unclosed(token)
        } else {
            Tokenized::Complete(token)
        });
    };
    debug_assert!(end >= start);

    let after = &tail[end..];
    let body = after.trim_start();
    if quoted && !after.is_empty() && body.len() == after.len() {
        // A closing quote must be followed by whitespace or end-of-input.
        // `"Alice"hello` is junk the user can see and fix; but `'don't hi`
        // closed at an apostrophe that was meant to be part of the name, so
        // the useful answer there is that the token never terminated.
        return Some(if quote == Some('\'') {
            Tokenized::Unclosed(partial(text, true, ""))
        } else {
            Tokenized::Junk
        });
    }
    Some(Tokenized::Complete(RecipientToken {
        text,
        quoted,
        complete: true,
        body_started: !after.is_empty(),
        rest: body.to_owned(),
    }))
}

fn partial(text: String, quoted: bool, rest: &str) -> RecipientToken {
    RecipientToken {
        text,
        quoted,
        complete: false,
        body_started: false,
        rest: rest.to_owned(),
    }
}

/// The inverse of [`parse_recipient`]: the shortest form that parses back to
/// `name`.
///
/// Bare only when the name has no Unicode whitespace, no `"`, no `'`, no `\`,
/// does not start with `@` and is not the broadcast keyword; otherwise
/// double-quoted with `\` escaped before `"`. Quoting on *any* backslash is
/// what keeps a trailing one from leaving an unterminated token.
pub fn quote_recipient(name: &str) -> String {
    let bare = !name.is_empty()
        && !name.chars().any(char::is_whitespace)
        && !name.contains(['"', '\'', '\\'])
        && !name.starts_with('@')
        && name != BROADCAST_KEYWORD;
    if bare {
        return name.to_owned();
    }
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

// ── Resolution ─────────────────────────────────────────────────────────────

/// Resolve a token to exactly one recipient, or refuse.
///
/// `body` is only read by the shadow check (§3.4.3). There is **no** access to
/// the current selection, by construction: `/agent disable` substitutes the
/// selection for an unknown argument, and doing that here would deliver a body
/// to an agent the user never named.
pub fn resolve_recipient(
    agents: &[Agent],
    token: &RecipientToken,
    body: &str,
) -> Result<Recipient, ResolveError> {
    if !token.quoted && token.text == BROADCAST_KEYWORD {
        let enabled = enabled_count(agents);
        if enabled == 0 {
            return Err(ResolveError::NoEnabledAgents);
        }
        return Ok(Recipient::Broadcast { enabled });
    }

    // As typed first, so an agent literally named `@lead` wins over the
    // `@`-stripped `lead`. The `@` is a resolution fallback, not grammar.
    let fallback = token.text.strip_prefix('@').filter(|rest| !rest.is_empty());
    for text in [token.text.as_str()].into_iter().chain(fallback) {
        match commit(agents, text) {
            Some(Ok(index)) => {
                let agent = &agents[index];
                if !token.quoted {
                    check_shadow(agents, text, body, agent)?;
                }
                return Ok(Recipient::Agent {
                    id: agent.id.clone(),
                    name: agent.name.clone(),
                    disabled: agent.status == AgentStatus::Disabled,
                });
            }
            Some(Err(found)) => {
                return Err(ResolveError::Ambiguous {
                    text: token.text.clone(),
                    found,
                });
            }
            None => {}
        }
    }

    // Nothing committed: diagnose, never send.
    let mut starting: Vec<Named> = Vec::new();
    for agent in agents {
        if ci_strip_prefix(&nfc(&agent.name), &nfc(&token.text)).is_some() {
            starting.push(named(agent));
        }
    }
    Err(match starting.len() {
        0 => ResolveError::NoMatch {
            text: token.text.clone(),
        },
        1 => ResolveError::DidYouMean {
            text: token.text.clone(),
            name: starting.remove(0).name,
        },
        _ => ResolveError::SeveralStartWith {
            text: token.text.clone(),
            found: starting,
        },
    })
}

/// The first commit stage with any candidate decides: one is the recipient,
/// several are an ambiguity, and resolution never falls through to a later
/// stage. `None` means no stage matched anything.
fn commit(agents: &[Agent], text: &str) -> Option<Result<usize, Vec<Named>>> {
    let normalized = nfc(text);
    let stages: [&dyn Fn(&Agent) -> bool; 3] = [
        &|agent: &Agent| agent.id == text,
        &|agent: &Agent| nfc(&agent.name) == normalized,
        &|agent: &Agent| ci_eq(&nfc(&agent.name), &normalized),
    ];
    for matches in stages {
        let found: Vec<usize> = (0..agents.len())
            .filter(|index| matches(&agents[*index]))
            .collect();
        match found.len() {
            0 => {}
            1 => return Some(Ok(found[0])),
            _ => return Some(Err(found.iter().map(|i| named(&agents[*i])).collect())),
        }
    }
    None
}

/// §3.4.3: `Code` committed, but `Code Reviewer` exists and the body begins
/// with `Reviewer` — or with the start of it — so there are two readings:
/// refuse and show both.
fn check_shadow(
    agents: &[Agent],
    text: &str,
    body: &str,
    committed: &Agent,
) -> Result<(), ResolveError> {
    let normalized = nfc(text);
    let body_nfc = nfc(body);
    for agent in agents {
        if agent.id == committed.id {
            continue;
        }
        let name = nfc(&agent.name);
        // The longer name must continue past a whitespace boundary, so `Codex`
        // does not shadow `Code`.
        let Some(after_prefix) = ci_strip_prefix(&name, &normalized) else {
            continue;
        };
        let remainder = after_prefix.trim_start();
        if remainder.len() == after_prefix.len() || remainder.is_empty() {
            continue;
        }
        // …and the body must read as the rest of that name, whole or started.
        let Some(long_body) = continues_name(remainder, &body_nfc) else {
            continue;
        };
        return Err(ResolveError::Shadowed {
            short: committed.name.clone(),
            long: agent.name.clone(),
            short_body: body.to_owned(),
            long_body,
        });
    }
    Ok(())
}

/// The body with the longer name's remainder taken off its head, when the body
/// reads as that remainder — either all of it (`/msg Code Reviewer please`) or
/// a leading run of whole words of it (`/msg Code Rev hi`, where `Rev` cannot
/// be a body word the user meant for `Code`).
///
/// The second form closes the gap the first left: `Code` committed, the first
/// word of the body was eaten as part of a name it only *started*, and nothing
/// warned (gate code (a)).
fn continues_name(remainder: &str, body: &str) -> Option<String> {
    if let Some(rest) = ci_strip_prefix(body, remainder)
        && (rest.is_empty() || rest.starts_with(char::is_whitespace))
    {
        return Some(rest.trim_start().to_owned());
    }
    // Longest first would mean re-walking the body; prefixes only grow, so the
    // last candidate that fits is the longest one and the first that does not
    // ends the search.
    let mut longest = None;
    for end in word_ends(body) {
        match ci_strip_prefix(remainder, &body[..end]) {
            // A whole-name match is the branch above; here the name must still
            // have something left, or the user typed all of it.
            Some(rest) if !rest.is_empty() => longest = Some(body[end..].trim_start().to_owned()),
            _ => break,
        }
    }
    longest
}

/// The byte offsets at which a whole word of `text` ends, in order.
fn word_ends(text: &str) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut in_word = false;
    for (index, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if in_word {
                ends.push(index);
            }
            in_word = false;
        } else {
            in_word = true;
        }
    }
    if in_word {
        ends.push(text.len());
    }
    ends
}

// ── Suggestions ────────────────────────────────────────────────────────────

/// One row the panel can offer after `/msg `.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Candidate {
    /// An index into the agent slice that was passed in.
    Agent(usize),
    /// The broadcast row, which always sorts **last** so `suggestion_index 0`
    /// is an agent row whenever any agent matches (design-v2 §3.5.3).
    Broadcast,
}

/// Candidates for a partial recipient, in panel order: the exact name, then
/// name prefixes, then id prefixes, then name substrings, then the broadcast
/// row.
///
/// `order` is the display order of `agents` — `AppState::sorted_agents`, so the
/// panel lists them the way the Agents tab does. Prefix matching is safe here
/// precisely because the user sees the row before committing; §3.4.1 is what
/// refuses to *commit* on one.
///
/// The **exact** group exists because accepting a row is a fill, and a fill
/// that changes the recipient is a wrong send: with `alice` and `Alice` in the
/// fleet, `/msg Alice` showed `→ Alice` and Tab rewrote it to `/msg alice `
/// (gate QA repro A). The name the user typed is the one Enter would resolve
/// (`commit` stage 2), so it is also the row Tab must fill.
pub fn suggest_recipients(agents: &[Agent], order: &[usize], partial: &str) -> Vec<Candidate> {
    let needle = nfc(partial);
    let mut out: Vec<Candidate> = Vec::new();
    let mut seen = vec![false; agents.len()];

    for by in [
        MatchBy::NameExact,
        MatchBy::NamePrefix,
        MatchBy::IdPrefix,
        MatchBy::NameSubstring,
    ] {
        for index in order.iter().copied().filter(|i| *i < agents.len()) {
            let agent = &agents[index];
            let hit = match by {
                MatchBy::NameExact => !needle.is_empty() && nfc(&agent.name) == needle,
                MatchBy::NamePrefix => ci_strip_prefix(&nfc(&agent.name), &needle).is_some(),
                MatchBy::IdPrefix => agent.id.starts_with(partial),
                MatchBy::NameSubstring => ci_contains(&nfc(&agent.name), &needle),
            };
            // A later group never lists an agent an earlier one already did.
            if hit && !seen[index] {
                seen[index] = true;
                out.push(Candidate::Agent(index));
            }
        }
    }

    // Shown when the partial is empty or could still become `all`, and never
    // above an agent row: broadcast is the one irreversible action here.
    if needle.is_empty() || ci_strip_prefix(BROADCAST_KEYWORD, &needle).is_some() {
        out.push(Candidate::Broadcast);
    }
    out
}

#[derive(Clone, Copy)]
enum MatchBy {
    NameExact,
    NamePrefix,
    IdPrefix,
    NameSubstring,
}

// ── The recipient chip ─────────────────────────────────────────────────────

/// What the command bar pins in front of the body while `/msg` is being typed:
/// the resolution the Enter key *would* make, before it is pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub text: String,
    pub tone: Token,
}

/// The chip for a token, or `None` when there is nothing to say yet.
///
/// Driven by the same resolution the send uses, so the chip and the refusal
/// can never disagree. A still-incomplete token resolves too — the user is
/// mid-name and a live `→ Code Reviewer` is the point.
pub fn chip(agents: &[Agent], token: &RecipientToken, body: &str) -> Option<Chip> {
    if token.text.is_empty() {
        return None;
    }
    let (text, tone) = match resolve_recipient(agents, token, body) {
        Ok(Recipient::Broadcast { enabled }) => (format!("→ all agents ({enabled})"), Token::Amber),
        Ok(Recipient::Agent {
            name,
            disabled: true,
            ..
        }) => (format!("→ {name} (disabled)"), Token::Amber),
        Ok(Recipient::Agent { name, .. }) => (format!("→ {name}"), Token::Green),
        Err(ResolveError::Ambiguous { found, .. }) => {
            (format!("⚠ ambiguous: {} agents", found.len()), Token::Red)
        }
        Err(ResolveError::DidYouMean { name, .. }) => {
            (format!("⚠ no match — did you mean \"{name}\"?"), Token::Red)
        }
        Err(ResolveError::SeveralStartWith { found, .. }) => (
            format!("⚠ no match — {} agents start with it", found.len()),
            Token::Red,
        ),
        Err(ResolveError::NoMatch { text }) => (format!("⚠ no match: \"{text}\""), Token::Red),
        // The shadow refusal commits to an agent; naming it is more use than
        // repeating the ambiguity the feed will spell out on Enter.
        Err(ResolveError::Shadowed { short, .. }) => {
            (format!("⚠ \"{short}\" or longer?"), Token::Red)
        }
        Err(ResolveError::NoEnabledAgents) => ("⚠ no enabled agents".to_owned(), Token::Red),
    };
    Some(Chip { text, tone })
}

pub fn enabled_count(agents: &[Agent]) -> usize {
    agents
        .iter()
        .filter(|a| a.status != AgentStatus::Disabled)
        .count()
}

fn named(agent: &Agent) -> Named {
    Named {
        id: agent.id.clone(),
        name: agent.name.clone(),
    }
}

// ── Unicode helpers ────────────────────────────────────────────────────────

/// NFC, skipping the walk when the text is already normalized — which every
/// ASCII name is, so the common case costs one scan and no allocation.
fn nfc(text: &str) -> Cow<'_, str> {
    if text.is_ascii() || is_nfc_quick(text.chars()) == IsNormalized::Yes {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.nfc().collect())
}

/// If `text` starts with `prefix` case-insensitively, the rest of `text`.
///
/// Built by lowercasing `text` one character at a time so the returned slice
/// is a real boundary in the original: `char::to_lowercase` can change the
/// character count (`İ` → `i̇`), so taking `prefix.len()` bytes would be wrong
/// and could panic.
fn ci_strip_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.is_ascii() && prefix.is_ascii() {
        let head = text.get(..prefix.len())?;
        return head
            .eq_ignore_ascii_case(prefix)
            .then(|| &text[prefix.len()..]);
    }
    let target: String = prefix.to_lowercase();
    if target.is_empty() {
        return Some(text);
    }
    let mut built = String::with_capacity(target.len());
    for (index, ch) in text.char_indices() {
        built.extend(ch.to_lowercase());
        if !target.starts_with(&built) {
            return None;
        }
        if built == target {
            return Some(&text[index + ch.len_utf8()..]);
        }
    }
    None
}

fn ci_eq(text: &str, other: &str) -> bool {
    if text.is_ascii() && other.is_ascii() {
        return text.eq_ignore_ascii_case(other);
    }
    text.to_lowercase() == other.to_lowercase()
}

fn ci_contains(text: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if text.is_ascii() && needle.is_ascii() {
        return text
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase());
    }
    text.to_lowercase().contains(&needle.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::make_agent;

    fn agent(id: &str, name: &str) -> Agent {
        make_agent(id, name, AgentStatus::Idle)
    }

    fn disabled(id: &str, name: &str) -> Agent {
        make_agent(id, name, AgentStatus::Disabled)
    }

    fn token(tail: &str) -> RecipientToken {
        parse_recipient(tail).expect("parses")
    }

    /// Every name the quoter has to survive. The list is the test: each entry
    /// is a shape of awkwardness agent names actually allow.
    const AWKWARD: [&str; 35] = [
        "plain",
        "two words",
        "  inner   runs  ",
        "say \"hi\"",
        "it's",
        "'tis",
        "C:\\path",
        "C:\\",
        "back\\\\slash",
        "tab\there",
        "nb\u{00A0}space",
        "all",
        "ALL",
        "All",
        "team",
        "lead",
        "@lead",
        "@",
        "@all",
        "-x",
        "--flag",
        "🚀 deploy",
        "名前 です",
        "é",         // NFC
        "e\u{0301}", // NFD
        "foo [bar]",
        "\"",
        "'",
        "\"\"",
        "''",
        "a\"b'c\\d",
        "x",
        "😀",
        "msg",
        "/msg",
    ];

    #[test]
    fn quoting_round_trips_every_awkward_name() {
        for name in AWKWARD {
            let line = format!("{} x", quote_recipient(name));
            let parsed = parse_recipient(&line)
                .unwrap_or_else(|err| panic!("{name:?} → {line:?}: {}", err.message()));
            assert_eq!(parsed.text, name, "round trip of {name:?} via {line:?}");
            assert_eq!(parsed.rest, "x", "body after {line:?}");
        }
        // A 200-character name and one with a trailing backslash, separately
        // because they are about length and the escape, not the character set.
        for name in ["n".repeat(200), "ends\\".to_owned()] {
            let parsed = parse_recipient(&format!("{} body", quote_recipient(&name))).expect("ok");
            assert_eq!(parsed.text, name);
            assert_eq!(parsed.rest, "body");
        }
    }

    #[test]
    fn the_quoter_is_bare_only_when_nothing_forces_quotes() {
        assert_eq!(quote_recipient("reviewer"), "reviewer");
        assert_eq!(quote_recipient("名前"), "名前");
        assert_eq!(quote_recipient("reviewer-1"), "reviewer-1");
        assert_eq!(quote_recipient("Code Reviewer"), "\"Code Reviewer\"");
        assert_eq!(quote_recipient("🚀 deploy"), "\"🚀 deploy\"");
        assert_eq!(quote_recipient("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quote_recipient("C:\\path"), "\"C:\\\\path\"");
        assert_eq!(quote_recipient("C:\\"), "\"C:\\\\\"");
        assert_eq!(quote_recipient("don't"), "\"don't\"");
        assert_eq!(quote_recipient("'tis"), "\"'tis\"");
        assert_eq!(quote_recipient("@lead"), "\"@lead\"");
        assert_eq!(quote_recipient("foo [bar]"), "\"foo [bar]\"");
        // Nothing is reserved after `/msg` except the broadcast keyword.
        assert_eq!(quote_recipient("team"), "team");
        assert_eq!(quote_recipient("lead"), "lead");
        assert_eq!(quote_recipient("all"), "\"all\"");
        // Any Unicode whitespace, not just U+0020.
        assert_eq!(quote_recipient("tab\there"), "\"tab\there\"");
        assert_eq!(quote_recipient("nb\u{00A0}space"), "\"nb\u{00A0}space\"");
    }

    #[test]
    fn a_body_is_verbatim_after_one_whitespace_run() {
        let parsed = token("alice  please   look  at  #12");
        assert_eq!(parsed.text, "alice");
        assert_eq!(parsed.rest, "please   look  at  #12");
        assert!(parsed.body_started);
        // Its own quotes, hashes and slashes are untouched.
        assert_eq!(token("alice say \"hi\" & /quit").rest, "say \"hi\" & /quit");
        assert_eq!(token("\"Code Reviewer\" #1 is broken").rest, "#1 is broken");
    }

    #[test]
    fn malformed_tails_are_refused_with_their_own_message() {
        for (tail, expected) in [
            ("", ParseError::Missing),
            ("   ", ParseError::Missing),
            ("\"Code Rev hello", ParseError::UnclosedQuote),
            ("'don't hi", ParseError::UnclosedQuote),
            ("\"say \\", ParseError::UnclosedQuote),
            ("\"Alice\"hello", ParseError::JunkAfterQuote),
            ("\"\" hi", ParseError::EmptyRecipient),
        ] {
            assert_eq!(parse_recipient(tail), Err(expected), "tail {tail:?}");
        }
        assert_eq!(
            ParseError::UnclosedQuote.message(),
            "Unclosed quote in recipient — write /msg \"Code Reviewer\" <message>"
        );
    }

    #[test]
    fn a_recipient_with_no_body_parses_but_reports_no_body() {
        let parsed = token("Alice");
        assert_eq!(parsed.text, "Alice");
        assert!(!parsed.body_started);
        assert_eq!(parsed.rest, "");
        // Still the last thing on the line, so nothing follows it yet — which
        // is what keeps the chip quiet mid-name.
        assert!(!parsed.complete);
        assert!(token("Alice ").complete);
        // `/msg "Alice"` — its closing quote is the terminator, so complete,
        // quoted, and still no body.
        let parsed = token("\"Alice\"");
        assert_eq!(parsed.text, "Alice");
        assert!(parsed.quoted);
        assert!(parsed.complete);
        assert!(!parsed.body_started);
    }

    #[test]
    fn incomplete_mode_keeps_an_in_progress_token() {
        let partial = parse_partial("\"Code R").expect("partial");
        assert_eq!(partial.text, "Code R");
        assert!(!partial.complete);
        assert!(!partial.body_started);
        // A pending escape is dropped rather than reported.
        assert_eq!(parse_partial("\"say \\").expect("partial").text, "say ");
        // A bare token's backslash is a literal.
        assert_eq!(parse_partial("C:\\pa").expect("partial").text, "C:\\pa");
        assert_eq!(parse_partial(""), None);
        // `/msg Alice ` has begun its body, which is what closes the panel.
        assert!(parse_partial("Alice ").expect("partial").body_started);
        assert!(!parse_partial("Alice").expect("partial").body_started);
    }

    // ── Resolution ─────────────────────────────────────────────────────────

    fn fleet() -> Vec<Agent> {
        vec![
            agent("agt_rev", "Code Reviewer"),
            agent("agt_code", "Code"),
            agent("agt_a1", "Alice"),
            agent("agt_a2", "alice"),
            disabled("agt_off", "say \"hi\""),
        ]
    }

    fn resolve(agents: &[Agent], tail: &str) -> Result<Recipient, ResolveError> {
        let parsed = token(tail);
        let body = parsed.rest.clone();
        resolve_recipient(agents, &parsed, &body)
    }

    #[test]
    fn an_id_an_exact_name_and_a_unique_case_insensitive_name_commit() {
        let fleet = fleet();
        assert_eq!(
            resolve(&fleet, "agt_rev hi"),
            Ok(Recipient::Agent {
                id: "agt_rev".into(),
                name: "Code Reviewer".into(),
                disabled: false,
            })
        );
        assert_eq!(
            resolve(&fleet, "\"Code Reviewer\" hi"),
            Ok(Recipient::Agent {
                id: "agt_rev".into(),
                name: "Code Reviewer".into(),
                disabled: false,
            })
        );
        // Unique case-insensitively: only `Code Reviewer` folds to this.
        assert_eq!(
            resolve(&fleet, "\"code reviewer\" hi"),
            Ok(Recipient::Agent {
                id: "agt_rev".into(),
                name: "Code Reviewer".into(),
                disabled: false,
            })
        );
        // A disabled agent resolves — `orch msg send` accepts one too.
        assert_eq!(
            resolve(&fleet, "\"say \\\"hi\\\"\" hi"),
            Ok(Recipient::Agent {
                id: "agt_off".into(),
                name: "say \"hi\"".into(),
                disabled: true,
            })
        );
    }

    #[test]
    fn nfd_input_matches_an_nfc_stored_name_and_vice_versa() {
        let fleet = vec![agent("agt_e", "José")]; // NFC
        assert!(matches!(
            resolve(&fleet, "Jose\u{0301} hi"), // NFD
            Ok(Recipient::Agent { .. })
        ));
        let fleet = vec![agent("agt_e", "Jose\u{0301}")];
        assert!(matches!(
            resolve(&fleet, "José hi"),
            Ok(Recipient::Agent { .. })
        ));
    }

    #[test]
    fn ambiguity_lists_the_ids_and_stops_rather_than_falling_through() {
        let fleet = fleet();
        // `Alice` is exact for one and case-insensitive for two; stage 2 wins
        // with a single candidate, so this is *not* ambiguous.
        assert_eq!(
            resolve(&fleet, "Alice hi"),
            Ok(Recipient::Agent {
                id: "agt_a1".into(),
                name: "Alice".into(),
                disabled: false,
            })
        );
        // `ALICE` matches neither exactly, so stage 3 sees both.
        let err = resolve(&fleet, "ALICE hi").expect_err("ambiguous");
        assert_eq!(
            err.lines(),
            vec![
                "Ambiguous recipient \"ALICE\" — 2 agents match: agt_a1 Alice, agt_a2 alice \
                 (use the id)"
            ]
        );
    }

    #[test]
    fn a_prefix_never_commits() {
        // The case that made v1 dangerous: `Code Reviewer` is the only match
        // for `Rev`, and it still refuses.
        let fleet = vec![agent("agt_rev", "Code Reviewer")];
        let err = resolve(&fleet, "Code please look at #12").expect_err("refused");
        assert_eq!(
            err.lines(),
            vec![
                "No agent named \"Code\" — did you mean \"Code Reviewer\"? \
                 Quote names with spaces: /msg \"Code Reviewer\" <message>"
            ]
        );

        let fleet = vec![agent("agt_1", "reviewer-1"), agent("agt_2", "reviewer-2")];
        let err = resolve(&fleet, "rev hi").expect_err("refused");
        assert_eq!(
            err.lines(),
            vec![
                "No agent named \"rev\" — 2 agents start with it: agt_1 reviewer-1, \
                 agt_2 reviewer-2"
            ]
        );

        let err = resolve(&fleet, "zzz hi").expect_err("refused");
        assert_eq!(
            err.lines(),
            vec!["No agent matches \"zzz\" — /agent list to see names"]
        );
    }

    #[test]
    fn an_unquoted_exact_match_shadowed_by_a_longer_name_is_refused() {
        let fleet = fleet();
        let err = resolve(&fleet, "Code Reviewer please look at #12").expect_err("refused");
        assert_eq!(
            err.lines(),
            vec![
                "\"Code\" is an agent, and so is \"Code Reviewer\" — quote the one you mean:",
                "  /msg \"Code\" Reviewer please look at #12",
                "  /msg \"Code Reviewer\" please look at #12",
            ]
        );

        // A body that does not continue the longer name commits to `Code`.
        assert_eq!(
            resolve(&fleet, "Code hello"),
            Ok(Recipient::Agent {
                id: "agt_code".into(),
                name: "Code".into(),
                disabled: false,
            })
        );
        // Quoted is unambiguous by construction, so no shadow check runs.
        assert_eq!(
            resolve(&fleet, "\"Code\" Reviewer please"),
            Ok(Recipient::Agent {
                id: "agt_code".into(),
                name: "Code".into(),
                disabled: false,
            })
        );
        // `Codex` does not shadow `Code`: the longer name must continue past a
        // whitespace boundary.
        let fleet = vec![agent("agt_c", "Code"), agent("agt_x", "Codex")];
        assert!(matches!(
            resolve(&fleet, "Code x hello"),
            Ok(Recipient::Agent { .. })
        ));
    }

    #[test]
    fn a_body_that_only_starts_the_longer_name_is_refused_as_well() {
        // `/msg Code Rev hi` committed to `Code` and sent "Rev hi": the body's
        // first word was eaten as part of a name it only started.
        let fleet = fleet();
        let err = resolve(&fleet, "Code Rev hi").expect_err("refused");
        assert_eq!(
            err.lines(),
            vec![
                "\"Code\" is an agent, and so is \"Code Reviewer\" — quote the one you mean:",
                "  /msg \"Code\" Rev hi",
                "  /msg \"Code Reviewer\" hi",
            ]
        );
        // The whole first word has to fit the name, and only whole words
        // count: `Review` does, `Reviewing` and `Rev,` do not.
        assert!(resolve(&fleet, "Code Review it").is_err());
        assert!(resolve(&fleet, "Code R").is_err());
        for body in ["Reviewing this", "Rev, please", "revert that", "hello"] {
            assert!(
                matches!(
                    resolve(&fleet, &format!("Code {body}")),
                    Ok(Recipient::Agent { .. })
                ),
                "body {body:?} must reach Code"
            );
        }
        // Quoted is still unambiguous by construction.
        assert!(matches!(
            resolve(&fleet, "\"Code\" Rev hi"),
            Ok(Recipient::Agent { .. })
        ));
    }

    #[test]
    fn only_the_exact_lowercase_bare_all_broadcasts() {
        let fleet = fleet();
        assert_eq!(
            resolve(&fleet, "all hi"),
            Ok(Recipient::Broadcast { enabled: 4 })
        );
        for tail in ["ALL hi", "All hi", "@all hi", "\"all\" hi"] {
            assert!(
                !matches!(resolve(&fleet, tail), Ok(Recipient::Broadcast { .. })),
                "{tail:?} must not broadcast"
            );
        }

        // An agent named `all` is shadowed by the keyword but reachable.
        let fleet = vec![agent("agt_all", "all")];
        assert_eq!(
            resolve(&fleet, "all hi"),
            Ok(Recipient::Broadcast { enabled: 1 })
        );
        for tail in ["\"all\" hi", "agt_all hi"] {
            assert_eq!(
                resolve(&fleet, tail),
                Ok(Recipient::Agent {
                    id: "agt_all".into(),
                    name: "all".into(),
                    disabled: false,
                }),
                "{tail:?}"
            );
        }
    }

    #[test]
    fn a_broadcast_with_no_enabled_agents_is_refused_before_sending() {
        let fleet = vec![disabled("agt_off", "off")];
        assert_eq!(
            resolve(&fleet, "all hi"),
            Err(ResolveError::NoEnabledAgents)
        );
        assert_eq!(
            ResolveError::NoEnabledAgents.lines(),
            vec!["No enabled agents to message"]
        );
        assert_eq!(resolve(&[], "all hi"), Err(ResolveError::NoEnabledAgents));
    }

    #[test]
    fn an_at_prefix_is_a_fallback_that_never_beats_a_literal_name() {
        let fleet = vec![agent("agt_l", "lead"), agent("agt_at", "@lead")];
        // As typed wins.
        assert_eq!(
            resolve(&fleet, "\"@lead\" hi"),
            Ok(Recipient::Agent {
                id: "agt_at".into(),
                name: "@lead".into(),
                disabled: false,
            })
        );
        // With no literal `@lead`, the `@` is stripped and retried once.
        let fleet = vec![agent("agt_l", "lead")];
        assert_eq!(
            resolve(&fleet, "@lead hi"),
            Ok(Recipient::Agent {
                id: "agt_l".into(),
                name: "lead".into(),
                disabled: false,
            })
        );
        // A bare `@` has nothing to fall back to.
        assert!(resolve(&fleet, "@ hi").is_err());
    }

    #[test]
    fn resolution_has_no_access_to_a_selection_and_an_empty_fleet_refuses() {
        // The S1 regression, by construction: the only inputs are the agent
        // list, the token and the body.
        assert_eq!(
            resolve(&[], "anyone hi"),
            Err(ResolveError::NoMatch {
                text: "anyone".into()
            })
        );
    }

    // ── Suggestions ────────────────────────────────────────────────────────

    fn indices(agents: &[Agent]) -> Vec<usize> {
        (0..agents.len()).collect()
    }

    #[test]
    fn suggestions_group_by_prefix_then_id_then_substring_with_broadcast_last() {
        let fleet = vec![
            agent("agt_zz", "zebra"),
            agent("agt_rev", "reviewer-1"),
            agent("agt_sub", "code-reviewer"),
        ];
        let order = indices(&fleet);

        // Empty partial: everything in order, then the broadcast row.
        assert_eq!(
            suggest_recipients(&fleet, &order, ""),
            vec![
                Candidate::Agent(0),
                Candidate::Agent(1),
                Candidate::Agent(2),
                Candidate::Broadcast
            ]
        );
        // `rev`: the name prefix first, then the substring match. No
        // broadcast row — `all` does not start with `rev`.
        assert_eq!(
            suggest_recipients(&fleet, &order, "rev"),
            vec![Candidate::Agent(1), Candidate::Agent(2)]
        );
        // An id prefix matches, and matches are never listed twice.
        assert_eq!(
            suggest_recipients(&fleet, &order, "agt_z"),
            vec![Candidate::Agent(0)]
        );
        // `a` is a prefix of every *id* (`agt_…`), so the id group lists the
        // whole fleet — the same set the empty partial shows, and still with
        // an agent at index 0 so Tab never fills `all` while an agent matches.
        assert_eq!(
            suggest_recipients(&fleet, &order, "a"),
            vec![
                Candidate::Agent(0),
                Candidate::Agent(1),
                Candidate::Agent(2),
                Candidate::Broadcast
            ]
        );
        // Only the broadcast row when nothing else matches.
        assert_eq!(
            suggest_recipients(&fleet, &order, "al"),
            vec![Candidate::Broadcast]
        );
        assert!(suggest_recipients(&fleet, &order, "zzz").is_empty());
    }

    #[test]
    fn an_exactly_typed_name_sorts_above_its_case_insensitive_twins() {
        // Accepting a row is a *fill*, so a row that sorts above the name the
        // user typed rewrites the recipient: with `alice` and `Alice`,
        // `/msg Alice` + Tab filled `/msg alice `.
        let fleet = vec![agent("agt_lo", "alice"), agent("agt_up", "Alice")];
        for order in [vec![0, 1], vec![1, 0]] {
            assert_eq!(
                suggest_recipients(&fleet, &order, "Alice"),
                vec![Candidate::Agent(1), Candidate::Agent(0)],
                "order {order:?}"
            );
            assert_eq!(
                suggest_recipients(&fleet, &order, "alice"),
                vec![Candidate::Agent(0), Candidate::Agent(1)],
                "order {order:?}"
            );
            // Neither is exact, so the given order decides, as before.
            assert_eq!(
                suggest_recipients(&fleet, &order, "ALICE")
                    .iter()
                    .map(|c| match c {
                        Candidate::Agent(index) => *index,
                        Candidate::Broadcast => usize::MAX,
                    })
                    .collect::<Vec<_>>(),
                order
            );
        }
        // An NFD-typed name is exact against its NFC twin.
        let fleet = vec![agent("agt_a", "josé"), agent("agt_b", "José")];
        assert_eq!(
            suggest_recipients(&fleet, &[0, 1], "Jose\u{0301}"),
            vec![Candidate::Agent(1), Candidate::Agent(0)]
        );
    }

    #[test]
    fn suggestions_follow_the_order_they_are_given() {
        let fleet = vec![agent("agt_1", "one"), agent("agt_2", "two")];
        assert_eq!(
            suggest_recipients(&fleet, &[1, 0], ""),
            vec![
                Candidate::Agent(1),
                Candidate::Agent(0),
                Candidate::Broadcast
            ]
        );
    }

    #[test]
    fn the_chip_shows_what_enter_would_do() {
        let fleet = fleet();
        let chip_for = |tail: &str| {
            let parsed = parse_partial(tail).expect("partial");
            let body = parsed.rest.clone();
            chip(&fleet, &parsed, &body).expect("chip")
        };

        assert_eq!(chip_for("\"Code Reviewer\" x").text, "→ Code Reviewer");
        assert_eq!(chip_for("\"Code Reviewer\" x").tone, Token::Green);
        assert_eq!(
            chip_for("\"say \\\"hi\\\"\" x").text,
            "→ say \"hi\" (disabled)"
        );
        assert_eq!(chip_for("\"say \\\"hi\\\"\" x").tone, Token::Amber);
        assert_eq!(chip_for("all x").text, "→ all agents (4)");
        assert_eq!(chip_for("ALICE x").text, "⚠ ambiguous: 2 agents");
        assert_eq!(chip_for("ALICE x").tone, Token::Red);
        assert_eq!(
            chip_for("\"Code Rev\" x").text,
            "⚠ no match — did you mean \"Code Reviewer\"?"
        );
        assert_eq!(chip_for("zzz x").text, "⚠ no match: \"zzz\"");
        // Nothing typed yet is nothing to say.
        assert_eq!(
            chip(&fleet, &parse_partial("\"").expect("partial"), ""),
            None
        );
    }

    #[test]
    fn case_insensitive_prefix_stripping_respects_character_boundaries() {
        assert_eq!(ci_strip_prefix("Code Reviewer", "code"), Some(" Reviewer"));
        assert_eq!(ci_strip_prefix("Code", "code"), Some(""));
        assert_eq!(ci_strip_prefix("Code", "codex"), None);
        assert_eq!(ci_strip_prefix("Code", ""), Some("Code"));
        // Non-ASCII, where a byte-length slice of the lowercase form would be
        // wrong: `İ` lowercases to two characters.
        assert_eq!(ci_strip_prefix("İstanbul", "i\u{0307}st"), Some("anbul"));
        assert_eq!(ci_strip_prefix("ÉCOLE libre", "école"), Some(" libre"));
        assert_eq!(ci_strip_prefix("名前 です", "名前"), Some(" です"));
    }
}
