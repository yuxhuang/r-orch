//! `formatAgentOutput` and friends — the defensive JSON-shape dispatcher that
//! turns one agent event's `data` / `error` payload into an activity-feed
//! summary plus a capped detail string (`src/tui/App.tsx`, docs/parity/tui.md
//! §7).
//!
//! Every legacy adapter shape is accepted on purpose: run histories on disk
//! predate adapter normalization, so a `.jsonl` replay still contains the flat
//! Codex items, the Codex `item.started` / `item.completed` provider wrapper
//! and the raw Claude stream envelopes handled below (docs/PARITY.md §12.6).
//!
//! TS `.slice(0, n)` and `.length` count UTF-16 code units; the helpers here
//! count `char`s. The two agree for everything except astral characters
//! (emoji, CJK extensions), where a cut lands one `char` later than in TS —
//! which can only shift where an already-truncated line ends, never which
//! branch is taken.

use orch_domain::extract_error_message;
use serde_json::Value;

/// Max length of a stored detail string (`MAX_DETAIL_LEN` in `App.tsx`).
///
/// The formatters append a bare `…` past the cap; `addMessage` appends
/// `…[truncated]` to the detail it stores (docs/parity/tui.md §7).
pub const MAX_DETAIL_LEN: usize = 2048;

/// A formatted agent payload. `summary: None` means the caller skips the entry
/// entirely (`reasoning` items and tool_use-only assistant messages).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formatted {
    pub summary: Option<String>,
    pub detail: String,
}

/// Semantic message type a summary classifies into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryKind {
    Lifecycle,
    Tool,
    Result,
    Info,
    Output,
}

/// Palette slot a classified summary renders in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryColor {
    Dim,
    Yellow,
    Silver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classification {
    pub kind: SummaryKind,
    pub color: SummaryColor,
}

/// `undefined` stands in for a missing JSON field; `Value::Null` is the only
/// value we can hand to helpers that take `&Value` by reference.
static NULL: Value = Value::Null;

/* ── Public API ───────────────────────────────────────── */

/// Extract a human-readable summary from an agent output payload, which may be
/// raw JSON from any adapter, a bracket lifecycle tag, or plain text.
pub fn format_agent_output(raw: &str) -> Formatted {
    // Bracket-tags like [init], [hook_started], [hook_response] pass through.
    let trimmed = js_trim(raw);
    if is_lifecycle_tag(trimmed) {
        return with_detail(trimmed, raw);
    }

    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return with_detail(extract_summary_from_truncated(raw), raw);
    };
    // `JSON.parse("null")` succeeds, but the dispatcher's first property read
    // then throws on `null`, so TS lands in the very same catch block as a
    // parse failure.
    if parsed.is_null() {
        return with_detail(extract_summary_from_truncated(raw), raw);
    }

    dispatch(&parsed, raw)
}

/// Unwrap nested provider envelopes while preserving the full payload for the
/// detail view.
pub fn format_agent_error(raw: &str) -> Formatted {
    let message = extract_error_message(&Value::String(raw.to_owned()));
    // `message || raw` — an empty unwrap falls back to the untouched payload.
    let source = if message.is_empty() { raw } else { &message };
    with_detail(first_line_trunc(source, 200), raw)
}

/// Classify an agent output summary into a semantic message type + color.
pub fn classify_agent_summary(summary: &str) -> Classification {
    let cls = |kind, color| Classification { kind, color };
    if is_lifecycle_tag(summary) {
        return cls(SummaryKind::Lifecycle, SummaryColor::Dim);
    }
    if summary.starts_with('\u{2699}') {
        return cls(SummaryKind::Tool, SummaryColor::Dim);
    }
    if summary.starts_with("$ ") {
        return cls(SummaryKind::Tool, SummaryColor::Dim);
    }
    if summary.starts_with('\u{2190}') {
        return cls(SummaryKind::Result, SummaryColor::Dim);
    }
    if summary.starts_with('\u{2713}') {
        return cls(SummaryKind::Lifecycle, SummaryColor::Dim);
    }
    if summary.starts_with('\u{26A0}') {
        return cls(SummaryKind::Info, SummaryColor::Yellow);
    }
    if summary.starts_with('\u{23F3}') {
        return cls(SummaryKind::Info, SummaryColor::Silver);
    }
    cls(SummaryKind::Output, SummaryColor::Silver)
}

/// Format a tool input into a concise hint (`Read` → file basename, `Bash` →
/// command snippet).
///
/// `_name` is accepted for signature parity with the TS helper, which also
/// ignores it: the hint is derived entirely from the input shape.
pub fn format_tool_input(_name: &str, input: &Value) -> String {
    // `!input || typeof input !== 'object'` — arrays are objects in JS and
    // fall through to the generic JSON branch below.
    if !matches!(input, Value::Object(_) | Value::Array(_)) {
        return String::new();
    }

    // File-based tools: show the last two path segments.
    if let Some(path) = truthy_str(input, "file_path") {
        let parts: Vec<&str> = path.split('/').collect();
        return parts[parts.len().saturating_sub(2)..].join("/");
    }
    if let Some(command) = truthy_str(input, "command") {
        return slice_chars(command, 60);
    }
    if let Some(pattern) = truthy_str(input, "pattern") {
        return format!("\"{}\"", slice_chars(pattern, 40));
    }
    if let Some(glob) = truthy_str(input, "glob") {
        return slice_chars(glob, 40);
    }

    // Generic: compact JSON. `serde_json` is built with `preserve_order`, so
    // key order matches `JSON.stringify` on the same parsed document.
    let json = input.to_string();
    if char_len(&json) > 80 {
        format!("{}...", slice_chars(&json, 77))
    } else {
        json
    }
}

/// Extract readable text from a Claude-API content array (or a bare string).
///
/// `None` means "nothing renderable was found" and makes the caller skip the
/// entry; an empty `Some("")` is falsy in TS and is treated the same way by
/// every caller here.
pub fn extract_text_from_content(content: &Value, max_len: usize) -> Option<String> {
    if let Value::String(s) = content {
        return Some(slice_chars(s, max_len));
    }
    let blocks = content.as_array()?;

    let mut parts: Vec<String> = Vec::new();
    let mut total_len: usize = 0;
    for block in blocks {
        if total_len >= max_len {
            break;
        }
        match str_field(block, "type") {
            Some("text") => {
                if let Some(text) = str_field(block, "text") {
                    // Only the first meaningful line — skips file dumps.
                    let first_line = text.split('\n').find(|l| has_non_space(l)).unwrap_or("");
                    parts.push(slice_chars(first_line, max_len.saturating_sub(total_len)));
                    // TS adds the *unsliced* length to the running total.
                    total_len += char_len(first_line);
                }
            }
            Some("tool_use") => {
                let name = name_or_tool(block);
                let hint = format_tool_input(&name, field(block, "input"));
                let s = format!("\u{2699} {name}({hint})");
                total_len += char_len(&s);
                parts.push(s);
            }
            Some("tool_result") => {
                // Tool result content is never inlined — just marked.
                parts.push("\u{2190} (result)".to_owned());
                // TS adds a hard-coded 10, not the marker's own length.
                total_len += 10;
            }
            Some("thinking") => {
                if let Some(thinking) = str_field(block, "thinking") {
                    let head = slice_chars(thinking, 60);
                    let line = head.split('\n').next().unwrap_or("");
                    total_len += char_len(line) + 3;
                    parts.push(format!("\u{1F4AD} {line}"));
                }
            }
            _ => {}
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// Short description of `tool_result` content (for user messages).
pub fn summarize_tool_result(content: &Value) -> String {
    if let Value::String(s) = content {
        let lines: Vec<&str> = s.split('\n').collect();
        let first_line = lines
            .iter()
            .copied()
            .find(|l| has_non_space(l))
            .unwrap_or("");
        if lines.len() > 3 {
            return format!("{}... ({} lines)", slice_chars(first_line, 80), lines.len());
        }
        return slice_chars(first_line, 120);
    }
    let Some(blocks) = content.as_array() else {
        return "(result)".to_owned();
    };

    let mut summaries: Vec<String> = Vec::new();
    for block in blocks {
        match str_field(block, "type") {
            Some("tool_result") => {
                // TS also slices `tool_use_id` into a `toolId` local it never
                // uses; omitted here.
                let inner = str_field(block, "content").unwrap_or("");
                let lines = inner.split('\n').count();
                if truthy(field(block, "is_error")) {
                    summaries.push(format!("\u{2715} error: {}", slice_chars(inner, 60)));
                } else if lines > 3 {
                    summaries.push(format!("\u{2713} {lines} lines"));
                } else {
                    summaries.push(format!("\u{2713} {}", slice_chars(inner, 80)));
                }
            }
            Some("text") => {
                if let Some(text) = str_field(block, "text") {
                    summaries.push(slice_chars(text.split('\n').next().unwrap_or(""), 80));
                }
            }
            _ => {}
        }
    }

    let joined = summaries.join(" ");
    if joined.is_empty() {
        "(result)".to_owned()
    } else {
        joined
    }
}

/// First non-blank line of `s`, sliced to `n` chars. Skips leading blank lines
/// (`"\nhi"` → `"hi"`).
pub fn first_line_trunc(s: &str, n: usize) -> String {
    // Fast path for single-line input — most agent events are one line.
    if !s.contains('\n') {
        return slice_chars(s, n);
    }
    let line = s.split('\n').find(|l| has_non_space(l)).unwrap_or(s);
    slice_chars(line, n)
}

/* ── The dispatcher ───────────────────────────────────── */

fn dispatch(parsed: &Value, raw: &str) -> Formatted {
    let ty = str_field(parsed, "type");
    let role = str_field(parsed, "role");
    let has_type = truthy(field(parsed, "type"));
    let has_role = truthy(field(parsed, "role"));

    // Older Codex runs persisted the provider wrapper. Re-enter with its item
    // so existing .jsonl history benefits from the current item formatting.
    //
    // The item is re-serialized rather than dispatched directly so the nested
    // call sees the same `raw` TS gives it (its lifecycle-tag check and its
    // `raw.slice(0, 150)` fallback both read it). Recursion is bounded by
    // serde_json's own 128-level nesting limit, which rejects anything deeper
    // before we get here.
    if matches!(ty, Some("item.started") | Some("item.completed")) && truthy(field(parsed, "item"))
    {
        let inner = serde_json::to_string(field(parsed, "item")).unwrap_or_default();
        return Formatted {
            summary: format_agent_output(&inner).summary,
            detail: cap_detail(raw),
        };
    }

    // ── Legacy flat Codex item shapes, persisted before adapter normalization.
    if ty == Some("agent_message")
        && let Some(text) = str_field(parsed, "text")
    {
        return with_detail(first_line_trunc(text, 200), raw);
    }
    if ty == Some("reasoning") {
        return Formatted {
            summary: None,
            detail: String::new(),
        };
    }
    if ty == Some("command_execution")
        && let Some(command) = str_field(parsed, "command")
    {
        let result = str_field(parsed, "aggregated_output")
            .or_else(|| str_field(parsed, "result"))
            .filter(|s| !s.is_empty());
        let tail = result.map_or_else(String::new, |r| {
            format!(" \u{2192} {}", first_line_trunc(r, 80))
        });
        return with_detail(format!("$ {}{tail}", slice_chars(command, 120)), raw);
    }
    if ty == Some("web_search") {
        let query = str_field(parsed, "query").or_else(|| {
            field(parsed, "action")
                .get("queries")
                .and_then(Value::as_array)
                .and_then(|queries| queries.first())
                .and_then(Value::as_str)
        });
        let query = query.map_or_else(String::new, |q| slice_chars(q, 80));
        return with_detail(format!("\u{2699} web_search({query})"), raw);
    }

    // ── Canonical AgentEvent data shapes — see adapters/interface.ts.
    if let Some(text) = str_field(parsed, "text")
        && !text.is_empty()
        && !has_type
        && !has_role
    {
        return with_detail(first_line_trunc(text, 200), raw);
    }
    if let Some(command) = str_field(parsed, "command")
        && !has_type
    {
        let tail = str_field(parsed, "result")
            .filter(|s| !s.is_empty())
            .map_or_else(String::new, |r| {
                format!(" \u{2192} {}", first_line_trunc(r, 80))
            });
        return with_detail(format!("$ {}{tail}", slice_chars(command, 120)), raw);
    }
    if let Some(paths) = field(parsed, "paths").as_array()
        && !paths.is_empty()
        && !has_type
    {
        // MSG_ICONS.file
        let joined = js_join(paths, ", ");
        return with_detail(format!("\u{270E} {}", slice_chars(&joined, 180)), raw);
    }
    if let Some(message) = str_field(parsed, "message")
        && !has_role
        && !truthy(field(parsed, "content"))
        && !truthy(field(parsed, "subtype"))
    {
        // MSG_ICONS.error
        return with_detail(format!("\u{2715} {}", first_line_trunc(message, 200)), raw);
    }
    if let Some(result) = str_field(parsed, "result")
        && !has_type
    {
        return with_detail(format!("\u{2713} {}", first_line_trunc(result, 200)), raw);
    }

    // ── Claude API message: {"type":"message","role":"assistant","content":[…]}
    if ty == Some("message") && role == Some("assistant") {
        return assistant_text(field(parsed, "content"), raw);
    }

    // ── Claude stream: {"type":"assistant","message":{"content":[…]}}
    if ty == Some("assistant") || role == Some("assistant") {
        return assistant_text(resolve_content(parsed), raw);
    }

    // ── User message (tool results flowing back)
    if ty == Some("user") || role == Some("user") {
        let summary = summarize_tool_result(resolve_content(parsed));
        return with_detail(format!("\u{2190} {}", slice_chars(&summary, 180)), raw);
    }

    // ── Tool use block (Claude: type === 'tool_use'; OpenCode: name + input)
    if ty == Some("tool_use") || (str_field(parsed, "name").is_some() && has_key(parsed, "input")) {
        let name = name_or_tool(parsed);
        let hint = format_tool_input(&name, field(parsed, "input"));
        return with_detail(format!("\u{2699} {name}({hint})"), raw);
    }

    // ── Tool result block
    if ty == Some("tool_result") {
        let summary = summarize_tool_result(field(parsed, "content"));
        return with_detail(format!("\u{2190} {}", slice_chars(&summary, 180)), raw);
    }

    // ── Result / done
    if ty == Some("result") {
        let text = str_field(parsed, "result").filter(|s| !s.is_empty());
        return with_detail(
            text.map_or_else(
                || "\u{2713} Agent finished".to_owned(),
                |t| format!("\u{2713} {}", slice_chars(t, 180)),
            ),
            raw,
        );
    }

    // ── Rate limit event
    if ty == Some("rate_limit_event") {
        let kind = nullish(field(parsed, "rate_limit_info").get("rateLimitType"))
            .map_or_else(|| "unknown".to_owned(), js_to_string);
        return with_detail(format!("\u{23F3} Rate limited ({kind})"), raw);
    }

    // ── System event with a subtype (task_progress, task_notification, …)
    if truthy(field(parsed, "subtype")) {
        // Nested message content, when the subtype carries one.
        if truthy(field(parsed, "message")) {
            let message = field(parsed, "message");
            let content = nullish(message.get("content")).unwrap_or(message);
            if let Some(text) = renderable(content) {
                return with_detail(slice_chars(&text, 200), raw);
            }
        }
        return with_detail(format!("[{}]", js_to_string(field(parsed, "subtype"))), raw);
    }

    // ── Generic: try the content field.
    if truthy(field(parsed, "content"))
        && let Some(text) = renderable(field(parsed, "content"))
    {
        return with_detail(slice_chars(&text, 200), raw);
    }

    // ── Fallback: show the type, or truncate the payload.
    if has_type {
        return with_detail(format!("[{}]", js_to_string(field(parsed, "type"))), raw);
    }
    with_detail(slice_chars(raw, 150), raw)
}

/// Assistant content → a summary, or `None` so the caller skips the entry
/// (tool_use-only or empty content).
fn assistant_text(content: &Value, raw: &str) -> Formatted {
    match renderable(content) {
        Some(text) => with_detail(slice_chars(&text, 200), raw),
        None => Formatted {
            summary: None,
            detail: String::new(),
        },
    }
}

/// `extractTextFromContent(content)` under TS truthiness: `""` is as good as
/// `null` at every call site.
fn renderable(content: &Value) -> Option<String> {
    extract_text_from_content(content, 200).filter(|t| !t.is_empty())
}

/// `parsed.message?.content ?? parsed.content`.
fn resolve_content(parsed: &Value) -> &Value {
    nullish(field(parsed, "message").get("content"))
        .or_else(|| nullish(parsed.get("content")))
        .unwrap_or(&NULL)
}

/* ── Regex fallback for truncated / unparseable payloads ── */

/// Extract a human-readable summary from truncated or unparseable JSON by
/// scanning for the fields the TS version matches with regexes.
fn extract_summary_from_truncated(raw: &str) -> String {
    if let Some(subtype) = match_plain_field(raw, "subtype") {
        return format!("[{subtype}]");
    }

    let ty = match_plain_field(raw, "type");
    let role = match_plain_field(raw, "role");
    if ty.is_none() && role.is_none() {
        return slice_chars(raw, 200);
    }

    // assistant / message — try to find text content.
    if ty == Some("assistant") || ty == Some("message") || role == Some("assistant") {
        if let Some(text) = match_escaped_field(raw, "text") {
            return slice_chars(&text, 200);
        }
        return "\u{1F4AC} (assistant)".to_owned();
    }

    if ty == Some("user") || ty == Some("tool_result") || role == Some("user") {
        return "\u{2190} (tool result)".to_owned();
    }

    if ty == Some("tool_use") {
        let name = match_plain_field(raw, "name").unwrap_or("tool");
        return format!("\u{2699} {name}()");
    }

    if ty == Some("result") {
        if let Some(result) = match_escaped_field(raw, "result") {
            return format!("\u{2713} {}", slice_chars(&result, 180));
        }
        return "\u{2713} Agent finished".to_owned();
    }

    if ty == Some("rate_limit_event") {
        return "\u{23F3} Rate limited".to_owned();
    }

    format!("[{}]", ty.or(role).unwrap_or_default())
}

/// `/"<key>"\s*:\s*"([^"]+)"/` — a quote-free, non-empty value.
fn match_plain_field<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    find_field_value(raw, key, |body| match body.find('"') {
        // `[^"]+` needs at least one character, and the closing quote must
        // exist — no shorter repetition can satisfy it either.
        Some(0) | None => None,
        Some(end) => Some(end),
    })
}

/// `/"<key>"\s*:\s*"((?:[^"\\]|\\.)*)"/`, then `JSON.parse('"<body>"')`.
///
/// `None` when the pattern does not match or the captured body is not a valid
/// JSON string — a truncated escape, which TS swallows the same way.
fn match_escaped_field(raw: &str, key: &str) -> Option<String> {
    let body = find_field_value(raw, key, scan_escaped_body)?;
    serde_json::from_str::<String>(&format!("\"{body}\"")).ok()
}

/// Offset of the closing quote of a JSON string body, or `None` when the
/// repetition cannot reach one.
fn scan_escaped_body(body: &str) -> Option<usize> {
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some(i),
            '\\' => match chars.next() {
                // `.` in `\\.` does not match a line terminator in JS, so the
                // repetition cannot consume this pair; and no shorter prefix
                // ends just before a quote, so the whole match fails.
                None | Some((_, '\n' | '\r' | '\u{2028}' | '\u{2029}')) => return None,
                Some(_) => {}
            },
            _ => {}
        }
    }
    None
}

/// Walk every `"<key>"` occurrence and return the first whose tail is
/// `\s*:\s*"` plus a body `scan` accepts — the backtracking regex engine's
/// behaviour of advancing the start position when the tail fails to match.
fn find_field_value<'a>(
    raw: &'a str,
    key: &str,
    scan: impl Fn(&str) -> Option<usize>,
) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let mut from = 0usize;
    while let Some(rel) = raw.get(from..)?.find(&needle) {
        let after = from + rel + needle.len();
        from = from + rel + 1;
        let rest = skip_js_space(&raw[after..]);
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        let Some(body) = skip_js_space(rest).strip_prefix('"') else {
            continue;
        };
        if let Some(end) = scan(body) {
            return Some(&body[..end]);
        }
    }
    None
}

/* ── Small helpers ────────────────────────────────────── */

/// Bracket-tags emitted by adapters: `[init]`, `[hook_started]`, … —
/// `/^\[[\w.:-]+\]$/`.
fn is_lifecycle_tag(s: &str) -> bool {
    let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) else {
        return false;
    };
    !inner.is_empty()
        && inner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

/// `raw.length > MAX_DETAIL_LEN ? raw.slice(0, MAX_DETAIL_LEN) + '…' : raw`.
fn cap_detail(raw: &str) -> String {
    if char_len(raw) > MAX_DETAIL_LEN {
        format!("{}…", slice_chars(raw, MAX_DETAIL_LEN))
    } else {
        raw.to_owned()
    }
}

fn with_detail(summary: impl Into<String>, raw: &str) -> Formatted {
    Formatted {
        summary: Some(summary.into()),
        detail: cap_detail(raw),
    }
}

/// `s.slice(0, n)`. See the module note on UTF-16 vs `char` counting.
fn slice_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `s.length`. See the module note on UTF-16 vs `char` counting.
fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// The JS `\s` class, which is also what `String.prototype.trim` strips. It
/// differs from `char::is_whitespace` at U+0085 (not JS whitespace) and
/// U+FEFF (not Unicode `White_Space`).
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{b}'
            | '\u{c}'
            | '\r'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    ) || ('\u{2000}'..='\u{200a}').contains(&c)
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

fn skip_js_space(s: &str) -> &str {
    s.trim_start_matches(is_js_space)
}

/// `/\S/.test(line)` — equivalently `line.trim().length > 0`.
fn has_non_space(s: &str) -> bool {
    s.chars().any(|c| !is_js_space(c))
}

/// A field of an object, or `Null` standing in for `undefined`. Property reads
/// on a non-object are `undefined` in JS too (`null` aside, which the caller
/// has already ruled out).
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&NULL)
}

/// `typeof value[key] === 'string' ? value[key] : undefined`.
fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

/// `value[key] && typeof value[key] === 'string'` — an empty string is falsy
/// and falls through.
fn truthy_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    str_field(value, key).filter(|s| !s.is_empty())
}

/// `key in value`.
fn has_key(value: &Value, key: &str) -> bool {
    value.as_object().is_some_and(|map| map.contains_key(key))
}

/// The left side of `a ?? b`: only `null` / `undefined` fall through.
fn nullish(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| !v.is_null())
}

/// JS truthiness of a JSON value (`undefined` modelled as `Null`).
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `${value}` in a template literal. Objects stringify to `[object Object]`
/// exactly as in JS; number formatting can differ for extreme magnitudes
/// (`1e21`), which no adapter emits in these fields.
fn js_to_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(items) => js_join(items, ","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

/// `Array.prototype.join` — `null` / `undefined` elements become empty strings.
fn js_join(items: &[Value], sep: &str) -> String {
    items
        .iter()
        .map(|v| match v {
            Value::Null => String::new(),
            other => js_to_string(other),
        })
        .collect::<Vec<_>>()
        .join(sep)
}

/// `block.name ?? 'tool'`.
fn name_or_tool(block: &Value) -> String {
    nullish(block.get("name")).map_or_else(|| "tool".to_owned(), js_to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn summary(raw: &str) -> Option<String> {
        format_agent_output(raw).summary
    }

    /* ── Lifecycle tags ──────────────────────────────── */

    #[test]
    fn bracket_lifecycle_tag_passes_through() {
        let out = format_agent_output("[init]");
        assert_eq!(out.summary.as_deref(), Some("[init]"));
        assert_eq!(out.detail, "[init]");
    }

    #[test]
    fn bracket_lifecycle_tag_summary_is_trimmed_but_detail_is_not() {
        let out = format_agent_output("  [hook_started]\n");
        assert_eq!(out.summary.as_deref(), Some("[hook_started]"));
        assert_eq!(out.detail, "  [hook_started]\n");
    }

    #[test]
    fn empty_brackets_are_not_a_lifecycle_tag() {
        // `[\w.:-]+` needs at least one character, so this is plain text.
        assert_eq!(summary("[]").as_deref(), Some("[]"));
    }

    #[test]
    fn dotted_codex_lifecycle_events_render_as_their_type() {
        // app.test.tsx: "excludes dotted Codex lifecycle events".
        let raw = r#"{"type":"thread.started","thread_id":"thread_1"}"#;
        assert_eq!(summary(raw).as_deref(), Some("[thread.started]"));
    }

    /* ── Codex provider wrapper ──────────────────────── */

    #[test]
    fn codex_item_completed_reenters_with_its_item() {
        let raw = r#"{"type":"item.completed","item":{"type":"agent_message","text":"Readable Codex response"}}"#;
        let out = format_agent_output(raw);
        assert_eq!(out.summary.as_deref(), Some("Readable Codex response"));
        // The detail keeps the outer envelope.
        assert_eq!(out.detail, raw);
    }

    #[test]
    fn codex_item_started_reenters_with_its_item() {
        let raw = r#"{"type":"item.started","item":{"type":"command_execution","command":"ls"}}"#;
        assert_eq!(summary(raw).as_deref(), Some("$ ls"));
    }

    #[test]
    fn codex_wrapper_without_an_item_falls_back_to_its_type() {
        assert_eq!(
            summary(r#"{"type":"item.completed"}"#).as_deref(),
            Some("[item.completed]")
        );
    }

    #[test]
    fn codex_wrapper_item_skipped_by_the_inner_shape_stays_skipped() {
        let raw = r#"{"type":"item.completed","item":{"type":"reasoning","text":"thinking"}}"#;
        let out = format_agent_output(raw);
        assert_eq!(out.summary, None);
        // The outer detail is still populated — only the summary recurses.
        assert_eq!(out.detail, raw);
    }

    /* ── Legacy flat Codex items ─────────────────────── */

    #[test]
    fn legacy_agent_message_renders_its_text() {
        // app.test.tsx: "renders legacy Codex agent_message text".
        let raw = r#"{"id":"item_0","type":"agent_message","text":"Readable Codex response"}"#;
        assert_eq!(summary(raw).as_deref(), Some("Readable Codex response"));
    }

    #[test]
    fn legacy_agent_message_takes_the_first_non_blank_line() {
        let raw = r#"{"type":"agent_message","text":"\n\n  first\nsecond"}"#;
        assert_eq!(summary(raw).as_deref(), Some("  first"));
    }

    #[test]
    fn agent_message_without_string_text_falls_back_to_its_type() {
        assert_eq!(
            summary(r#"{"type":"agent_message","text":42}"#).as_deref(),
            Some("[agent_message]")
        );
    }

    #[test]
    fn reasoning_items_are_skipped_with_an_empty_detail() {
        let out = format_agent_output(r#"{"type":"reasoning","text":"pondering"}"#);
        assert_eq!(out.summary, None);
        assert_eq!(out.detail, "");
    }

    #[test]
    fn command_execution_appends_its_aggregated_output() {
        let raw = r#"{"type":"command_execution","command":"ls -la","aggregated_output":"total 8\nmore"}"#;
        assert_eq!(summary(raw).as_deref(), Some("$ ls -la \u{2192} total 8"));
    }

    #[test]
    fn command_execution_falls_back_to_result_when_aggregated_output_is_absent() {
        let raw = r#"{"type":"command_execution","command":"pwd","result":"/tmp"}"#;
        assert_eq!(summary(raw).as_deref(), Some("$ pwd \u{2192} /tmp"));
    }

    #[test]
    fn command_execution_with_an_empty_aggregated_output_has_no_tail() {
        // An empty string still wins the ternary, so `result` is never read.
        let raw = r#"{"type":"command_execution","command":"pwd","aggregated_output":"","result":"/tmp"}"#;
        assert_eq!(summary(raw).as_deref(), Some("$ pwd"));
    }

    #[test]
    fn command_execution_caps_the_command_at_120_chars() {
        let raw = format!(
            r#"{{"type":"command_execution","command":"{}"}}"#,
            "c".repeat(200)
        );
        assert_eq!(
            summary(&raw).as_deref(),
            Some(format!("$ {}", "c".repeat(120)).as_str())
        );
    }

    #[test]
    fn web_search_renders_its_query() {
        let raw = r#"{"type":"web_search","query":"ratatui tables"}"#;
        assert_eq!(
            summary(raw).as_deref(),
            Some("\u{2699} web_search(ratatui tables)")
        );
    }

    #[test]
    fn web_search_falls_back_to_the_first_action_query() {
        let raw = r#"{"type":"web_search","action":{"queries":["rust tui","ignored"]}}"#;
        assert_eq!(
            summary(raw).as_deref(),
            Some("\u{2699} web_search(rust tui)")
        );
    }

    #[test]
    fn web_search_without_a_query_renders_empty_parens() {
        let raw = r#"{"type":"web_search","action":{"queries":[]}}"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2699} web_search()"));
    }

    /* ── Canonical AgentEvent shapes ─────────────────── */

    #[test]
    fn canonical_text_event_renders_its_first_line() {
        assert_eq!(
            summary(r#"{"text":"Hello there\nand more"}"#).as_deref(),
            Some("Hello there")
        );
    }

    #[test]
    fn canonical_text_is_capped_at_200_chars() {
        let raw = format!(r#"{{"text":"{}"}}"#, "t".repeat(300));
        assert_eq!(summary(&raw).as_deref(), Some("t".repeat(200).as_str()));
    }

    #[test]
    fn an_empty_text_field_falls_through_to_the_raw_fallback() {
        let raw = r#"{"text":""}"#;
        assert_eq!(summary(raw).as_deref(), Some(raw));
    }

    #[test]
    fn canonical_command_event_renders_a_shell_prefix() {
        let raw = r#"{"command":"cargo test","result":"ok\n"}"#;
        assert_eq!(summary(raw).as_deref(), Some("$ cargo test \u{2192} ok"));
    }

    #[test]
    fn canonical_paths_event_renders_the_file_icon() {
        let raw = r#"{"paths":["src/a.rs","src/b.rs"]}"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{270E} src/a.rs, src/b.rs"));
    }

    #[test]
    fn canonical_message_event_renders_the_error_icon() {
        assert_eq!(
            summary(r#"{"type":"error","message":"boom\ndetails"}"#).as_deref(),
            Some("\u{2715} boom")
        );
    }

    #[test]
    fn a_message_field_alongside_a_role_is_not_treated_as_an_error() {
        // `!parsed.role` guards the error branch, so this reaches the user
        // one — where `message?.content ?? content` resolves to nothing.
        let raw = r#"{"role":"user","message":"ignored"}"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2190} (result)"));
    }

    #[test]
    fn canonical_result_event_renders_a_check() {
        assert_eq!(
            summary(r#"{"result":"all green"}"#).as_deref(),
            Some("\u{2713} all green")
        );
    }

    /* ── Claude envelopes ────────────────────────────── */

    #[test]
    fn claude_assistant_message_renders_its_text_blocks() {
        let raw = r#"{"type":"message","role":"assistant","content":[{"type":"text","text":"Working on it"}]}"#;
        assert_eq!(summary(raw).as_deref(), Some("Working on it"));
    }

    #[test]
    fn claude_assistant_message_renders_tool_use_blocks_as_a_hint() {
        let raw = r#"{"type":"message","role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/repo/src/app.rs"}}]}"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2699} Read(src/app.rs)"));
    }

    #[test]
    fn an_empty_assistant_message_is_skipped() {
        let out = format_agent_output(r#"{"type":"message","role":"assistant","content":[]}"#);
        assert_eq!(out.summary, None);
        assert_eq!(out.detail, "");
    }

    #[test]
    fn claude_assistant_stream_reads_the_nested_message_content() {
        let raw =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Streamed"}]}}"#;
        assert_eq!(summary(raw).as_deref(), Some("Streamed"));
    }

    #[test]
    fn an_assistant_role_falls_back_to_the_top_level_content() {
        let raw = r#"{"role":"assistant","content":[{"type":"text","text":"Flat content"}]}"#;
        assert_eq!(summary(raw).as_deref(), Some("Flat content"));
    }

    #[test]
    fn an_assistant_stream_with_unrenderable_content_is_skipped() {
        let out = format_agent_output(r#"{"type":"assistant","message":{"content":"   "}}"#);
        // A string content is sliced, not scanned for blocks — whitespace is
        // truthy, so it renders rather than being skipped.
        assert_eq!(out.summary.as_deref(), Some("   "));
        let out = format_agent_output(r#"{"type":"assistant","message":{"content":[]}}"#);
        assert_eq!(out.summary, None);
    }

    #[test]
    fn a_user_message_summarizes_its_tool_results() {
        let raw = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_0123456789","content":"ok"}]}}"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2190} \u{2713} ok"));
    }

    #[test]
    fn a_user_message_marks_tool_result_errors() {
        let raw = r#"{"type":"user","content":[{"type":"tool_result","is_error":true,"content":"no such file"}]}"#;
        assert_eq!(
            summary(raw).as_deref(),
            Some("\u{2190} \u{2715} error: no such file")
        );
    }

    #[test]
    fn a_user_message_counts_lines_for_long_tool_results() {
        let raw = r#"{"type":"user","content":[{"type":"tool_result","content":"a\nb\nc\nd\ne"}]}"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2190} \u{2713} 5 lines"));
    }

    #[test]
    fn a_user_message_with_string_content_counts_its_lines() {
        let raw = r#"{"type":"user","content":"first\n\nthird\nfourth\nfifth"}"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2190} first... (5 lines)"));
    }

    #[test]
    fn a_user_message_without_recognizable_content_says_result() {
        assert_eq!(
            summary(r#"{"type":"user","content":42}"#).as_deref(),
            Some("\u{2190} (result)")
        );
    }

    #[test]
    fn a_tool_use_block_renders_name_and_hint() {
        let raw = r#"{"type":"tool_use","name":"Bash","input":{"command":"cargo fmt"}}"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2699} Bash(cargo fmt)"));
    }

    #[test]
    fn a_tool_use_block_without_a_name_says_tool() {
        let raw = r#"{"type":"tool_use","input":{"pattern":"TODO"}}"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2699} tool(\"TODO\")"));
    }

    #[test]
    fn an_opencode_name_plus_input_shape_is_a_tool_use() {
        let raw = r#"{"name":"Read","input":{"file_path":"a/b/c.ts"}}"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2699} Read(b/c.ts)"));
    }

    #[test]
    fn a_tool_result_block_summarizes_its_content() {
        let raw = r#"{"type":"tool_result","content":[{"type":"tool_result","content":"done"}]}"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2190} \u{2713} done"));
    }

    #[test]
    fn a_result_event_renders_its_result_string() {
        assert_eq!(
            summary(r#"{"type":"result","result":"Task complete"}"#).as_deref(),
            Some("\u{2713} Task complete")
        );
    }

    #[test]
    fn a_result_event_without_a_result_says_agent_finished() {
        assert_eq!(
            summary(r#"{"type":"result","subtype":"success"}"#).as_deref(),
            Some("\u{2713} Agent finished")
        );
        assert_eq!(
            summary(r#"{"type":"result","result":""}"#).as_deref(),
            Some("\u{2713} Agent finished")
        );
    }

    #[test]
    fn a_rate_limit_event_names_its_limit_type() {
        let raw = r#"{"type":"rate_limit_event","rate_limit_info":{"rateLimitType":"primary"}}"#;
        assert_eq!(
            summary(raw).as_deref(),
            Some("\u{23F3} Rate limited (primary)")
        );
    }

    #[test]
    fn a_rate_limit_event_without_info_says_unknown() {
        assert_eq!(
            summary(r#"{"type":"rate_limit_event"}"#).as_deref(),
            Some("\u{23F3} Rate limited (unknown)")
        );
    }

    /* ── Generic shapes and fallbacks ────────────────── */

    #[test]
    fn a_subtype_event_renders_its_nested_message_text() {
        let raw = r#"{"subtype":"task_progress","message":{"content":[{"type":"text","text":"Step 1 done"}]}}"#;
        assert_eq!(summary(raw).as_deref(), Some("Step 1 done"));
    }

    #[test]
    fn a_subtype_event_accepts_a_bare_string_message() {
        let raw = r#"{"subtype":"task_notification","message":"Heads up"}"#;
        assert_eq!(summary(raw).as_deref(), Some("Heads up"));
    }

    #[test]
    fn a_subtype_event_without_a_message_renders_its_subtype() {
        let raw = r#"{"subtype":"task_progress","session_id":"s1"}"#;
        assert_eq!(summary(raw).as_deref(), Some("[task_progress]"));
    }

    #[test]
    fn a_generic_content_field_is_rendered() {
        let raw = r#"{"content":[{"type":"text","text":"Just content"}]}"#;
        assert_eq!(summary(raw).as_deref(), Some("Just content"));
    }

    #[test]
    fn an_unknown_type_falls_back_to_bracketed_type() {
        assert_eq!(
            summary(r#"{"type":"stream_event","foo":1}"#).as_deref(),
            Some("[stream_event]")
        );
    }

    #[test]
    fn an_unknown_shape_falls_back_to_150_raw_chars() {
        let raw = format!(r#"{{"unknown":"{}"}}"#, "z".repeat(400));
        assert_eq!(
            summary(&raw).as_deref(),
            Some(raw[..150].to_owned().as_str())
        );
    }

    #[test]
    fn a_json_null_payload_lands_in_the_regex_fallback() {
        // `parsed.type` throws on `null` in TS, so this takes the catch path.
        assert_eq!(summary("null").as_deref(), Some("null"));
    }

    #[test]
    fn a_json_scalar_payload_falls_back_to_the_raw_string() {
        assert_eq!(summary("42").as_deref(), Some("42"));
        assert_eq!(summary(r#""hello""#).as_deref(), Some(r#""hello""#));
    }

    /* ── Truncated / unparseable payloads ────────────── */

    #[test]
    fn a_truncated_subtype_wins_over_the_type() {
        let raw = r#"{"type":"system","subtype":"init","tools":["Read","Writ"#;
        assert_eq!(summary(raw).as_deref(), Some("[init]"));
    }

    #[test]
    fn a_truncated_assistant_message_recovers_its_text() {
        // The first `"text"` occurrence is the *value* of `"type"`, which is
        // not followed by a colon — the scan has to advance past it.
        let raw = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hi there"}"#;
        assert_eq!(summary(raw).as_deref(), Some("Hi there"));
    }

    #[test]
    fn a_truncated_assistant_text_is_json_unescaped() {
        let raw = r#"{"type":"message","content":[{"type":"text","text":"a \"quoted\" word\nnext"},{"type":"too"#;
        assert_eq!(summary(raw).as_deref(), Some("a \"quoted\" word\nnext"));
    }

    #[test]
    fn a_truncated_text_without_a_closing_quote_does_not_match() {
        // The regex needs the closing quote; no shorter repetition can reach
        // one, so the whole pattern fails.
        let raw = r#"{"type":"message","content":[{"type":"text","text":"half a lin"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{1F4AC} (assistant)"));
    }

    #[test]
    fn a_truncated_assistant_without_text_says_assistant() {
        let raw = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","nam"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{1F4AC} (assistant)"));
    }

    #[test]
    fn a_truncated_assistant_with_a_broken_escape_says_assistant() {
        // `JSON.parse('"...\\u00"')` throws; TS swallows it and falls through.
        let raw = r#"{"type":"assistant","text":"bad \u00"}"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{1F4AC} (assistant)"));
    }

    #[test]
    fn a_truncated_user_message_says_tool_result() {
        let raw = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2190} (tool result)"));
    }

    #[test]
    fn a_truncated_tool_use_recovers_its_name() {
        let raw = r#"{"type":"tool_use","name":"Grep","input":{"pattern":"fn ma"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2699} Grep()"));
    }

    #[test]
    fn a_truncated_tool_use_without_a_name_says_tool() {
        let raw = r#"{"type":"tool_use","id":"toolu_1","inpu"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2699} tool()"));
    }

    #[test]
    fn a_truncated_result_recovers_its_result_string() {
        let raw = r#"{"type":"result","result":"Finished the task","usage":{"inp"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2713} Finished the task"));
    }

    #[test]
    fn a_truncated_result_cut_mid_escape_says_agent_finished() {
        let raw = r#"{"type":"result","result":"oops\"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{2713} Agent finished"));
    }

    #[test]
    fn a_truncated_rate_limit_event_says_rate_limited() {
        let raw = r#"{"type":"rate_limit_event","rate_limit_info":{"rateLimit"#;
        assert_eq!(summary(raw).as_deref(), Some("\u{23F3} Rate limited"));
    }

    #[test]
    fn a_truncated_unknown_type_is_bracketed() {
        let raw = r#"{"type":"stream_event","event":{"type"#;
        assert_eq!(summary(raw).as_deref(), Some("[stream_event]"));
    }

    #[test]
    fn a_truncated_payload_with_only_a_role_is_bracketed() {
        let raw = r#"{"role":"system","content":[{"#;
        assert_eq!(summary(raw).as_deref(), Some("[system]"));
    }

    #[test]
    fn plain_text_output_is_capped_at_200_chars() {
        let raw = "x".repeat(3000);
        let out = format_agent_output(&raw);
        assert_eq!(out.summary.as_deref(), Some("x".repeat(200).as_str()));
        assert_eq!(out.detail, format!("{}…", "x".repeat(MAX_DETAIL_LEN)));
    }

    #[test]
    fn details_under_the_cap_are_untouched() {
        let raw = "y".repeat(MAX_DETAIL_LEN);
        assert_eq!(format_agent_output(&raw).detail, raw);
    }

    /* ── format_agent_error ──────────────────────────── */

    #[test]
    fn agent_error_unwraps_nested_provider_envelopes() {
        // app.test.tsx: "unwraps nested provider JSON errors".
        let provider = serde_json::to_string(&json!({
            "type": "error",
            "status": 400,
            "error": {
                "type": "invalid_request_error",
                "message": "The 'gpt-5.6' model is not supported for this account.",
            },
        }))
        .expect("serializable");
        let raw = serde_json::to_string(&json!({ "type": "error", "message": provider }))
            .expect("serializable");

        let out = format_agent_error(&raw);
        assert_eq!(
            out.summary.as_deref(),
            Some("The 'gpt-5.6' model is not supported for this account.")
        );
        assert!(
            !out.summary
                .unwrap_or_default()
                .contains("invalid_request_error")
        );
        assert_eq!(out.detail, raw);
    }

    #[test]
    fn agent_error_unwraps_a_turn_failed_envelope_to_the_same_message() {
        let provider = serde_json::to_string(&json!({
            "type": "error",
            "status": 400,
            "error": {
                "type": "invalid_request_error",
                "message": "The 'gpt-5.6' model is not supported for this account.",
            },
        }))
        .expect("serializable");
        let raw = serde_json::to_string(
            &json!({ "type": "turn.failed", "error": { "message": provider } }),
        )
        .expect("serializable");

        assert_eq!(
            format_agent_error(&raw).summary.as_deref(),
            Some("The 'gpt-5.6' model is not supported for this account.")
        );
    }

    #[test]
    fn agent_error_keeps_plain_text_and_caps_the_detail() {
        let out = format_agent_error("ConnectionRefused: localhost:1234");
        assert_eq!(
            out.summary.as_deref(),
            Some("ConnectionRefused: localhost:1234")
        );

        let huge = "X".repeat(10 * 1024);
        let out = format_agent_error(&huge);
        // tui-endurance.test.tsx pins the 2 KB detail cap.
        assert_eq!(out.summary.as_deref(), Some("X".repeat(200).as_str()));
        assert_eq!(out.detail.chars().count(), MAX_DETAIL_LEN + 1);
        assert!(out.detail.ends_with('…'));
    }

    #[test]
    fn agent_error_falls_back_to_the_raw_payload_when_nothing_unwraps() {
        // `extractErrorMessage('   ')` is empty, so `message || raw` wins.
        assert_eq!(format_agent_error("   ").summary.as_deref(), Some("   "));
    }

    #[test]
    fn agent_error_takes_the_first_non_blank_line() {
        assert_eq!(
            format_agent_error("\n\nfirst line\nsecond")
                .summary
                .as_deref(),
            Some("first line")
        );
    }

    /* ── classify_agent_summary ──────────────────────── */

    #[test]
    fn classification_covers_every_prefix() {
        let cases: [(&str, SummaryKind, SummaryColor); 8] = [
            ("[init]", SummaryKind::Lifecycle, SummaryColor::Dim),
            ("\u{2699} Read(a.rs)", SummaryKind::Tool, SummaryColor::Dim),
            ("$ ls -la", SummaryKind::Tool, SummaryColor::Dim),
            ("\u{2190} (result)", SummaryKind::Result, SummaryColor::Dim),
            (
                "\u{2713} Agent finished",
                SummaryKind::Lifecycle,
                SummaryColor::Dim,
            ),
            (
                "\u{26A0} Model metadata",
                SummaryKind::Info,
                SummaryColor::Yellow,
            ),
            (
                "\u{23F3} Rate limited",
                SummaryKind::Info,
                SummaryColor::Silver,
            ),
            (
                "Plain agent text",
                SummaryKind::Output,
                SummaryColor::Silver,
            ),
        ];
        for (summary, kind, color) in cases {
            assert_eq!(
                classify_agent_summary(summary),
                Classification { kind, color },
                "classifying {summary:?}"
            );
        }
    }

    #[test]
    fn classification_does_not_treat_a_bracketed_sentence_as_lifecycle() {
        let cls = classify_agent_summary("[not a tag] and more");
        assert_eq!(cls.kind, SummaryKind::Output);
    }

    /* ── Helpers ─────────────────────────────────────── */

    #[test]
    fn tool_input_hints_cover_every_branch() {
        assert_eq!(format_tool_input("Read", &json!(null)), "");
        assert_eq!(format_tool_input("Read", &json!("string")), "");
        assert_eq!(
            format_tool_input("Read", &json!({"file_path": "/repo/src/app.rs"})),
            "src/app.rs"
        );
        assert_eq!(
            format_tool_input("Read", &json!({"file_path": "app.rs"})),
            "app.rs"
        );
        assert_eq!(
            format_tool_input("Bash", &json!({"command": "c".repeat(100)})),
            "c".repeat(60)
        );
        assert_eq!(
            format_tool_input("Grep", &json!({"pattern": "fn main"})),
            "\"fn main\""
        );
        assert_eq!(
            format_tool_input("Glob", &json!({"glob": "**/*.rs"})),
            "**/*.rs"
        );
        assert_eq!(format_tool_input("Other", &json!({"a": 1})), r#"{"a":1}"#);
        assert_eq!(format_tool_input("Other", &json!([1, 2])), "[1,2]");
    }

    #[test]
    fn a_generic_tool_input_over_80_chars_is_elided() {
        let input = json!({ "foo": "x".repeat(75) });
        // `{"foo":"` is 8 chars, so 69 payload chars survive the 77-char cut.
        let expected = format!("{{\"foo\":\"{}...", "x".repeat(69));
        let hint = format_tool_input("Other", &input);
        assert_eq!(hint, expected);
        assert_eq!(hint.chars().count(), 80);
    }

    #[test]
    fn extract_text_from_content_handles_each_block_type() {
        assert_eq!(
            extract_text_from_content(&json!("plain string"), 200).as_deref(),
            Some("plain string")
        );
        assert_eq!(extract_text_from_content(&json!(null), 200), None);
        assert_eq!(extract_text_from_content(&json!(7), 200), None);
        assert_eq!(extract_text_from_content(&json!([]), 200), None);
        assert_eq!(
            extract_text_from_content(
                &json!([{"type": "text", "text": "\n  \nreal line\nnext"}]),
                200
            )
            .as_deref(),
            Some("real line")
        );
        assert_eq!(
            extract_text_from_content(&json!([{"type": "tool_result", "content": "ignored"}]), 200)
                .as_deref(),
            Some("\u{2190} (result)")
        );
        assert_eq!(
            extract_text_from_content(
                &json!([{"type": "thinking", "thinking": "Let me check\nmore"}]),
                200
            )
            .as_deref(),
            Some("\u{1F4AD} Let me check")
        );
        assert_eq!(
            extract_text_from_content(&json!([{"type": "unknown_block"}]), 200),
            None
        );
    }

    #[test]
    fn extract_text_from_content_joins_blocks_with_a_space() {
        let content = json!([
            {"type": "text", "text": "Reading the file"},
            {"type": "tool_use", "name": "Read", "input": {"file_path": "src/app.rs"}},
        ]);
        assert_eq!(
            extract_text_from_content(&content, 200).as_deref(),
            Some("Reading the file \u{2699} Read(src/app.rs)")
        );
    }

    #[test]
    fn extract_text_from_content_stops_once_the_budget_is_spent() {
        let content = json!([
            {"type": "text", "text": "a".repeat(250)},
            {"type": "text", "text": "never reached"},
        ]);
        assert_eq!(
            extract_text_from_content(&content, 200).as_deref(),
            Some("a".repeat(200).as_str())
        );
    }

    #[test]
    fn summarize_tool_result_covers_strings_arrays_and_junk() {
        assert_eq!(summarize_tool_result(&json!("  \nhello")), "hello");
        assert_eq!(
            summarize_tool_result(&json!("one\ntwo\nthree\nfour")),
            "one... (4 lines)"
        );
        assert_eq!(summarize_tool_result(&json!(null)), "(result)");
        assert_eq!(summarize_tool_result(&json!({})), "(result)");
        assert_eq!(
            summarize_tool_result(&json!([{"type": "other"}])),
            "(result)"
        );
        assert_eq!(
            summarize_tool_result(&json!([{"type": "text", "text": "line\nmore"}])),
            "line"
        );
        assert_eq!(
            summarize_tool_result(&json!([
                {"type": "tool_result", "content": "fine"},
                {"type": "tool_result", "is_error": true, "content": "bad"},
            ])),
            "\u{2713} fine \u{2715} error: bad"
        );
        assert_eq!(
            summarize_tool_result(&json!([{"type": "tool_result", "content": 42}])),
            "\u{2713} "
        );
    }

    #[test]
    fn first_line_trunc_skips_blank_lines_and_slices() {
        assert_eq!(first_line_trunc("hello", 10), "hello");
        assert_eq!(first_line_trunc("hello", 2), "he");
        assert_eq!(first_line_trunc("\nhi", 10), "hi");
        assert_eq!(first_line_trunc("   \n\tfoo\nbar", 10), "\tfoo");
        // No newline at all — the whitespace-only fast path just slices.
        assert_eq!(first_line_trunc("   ", 10), "   ");
        // Multi-line but entirely blank — TS falls back to the whole string.
        assert_eq!(first_line_trunc("\n   ", 10), "\n   ");
    }
}
