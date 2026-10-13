//! The per-CLI model-listing parsers, ported one regex at a time from
//! `src/infrastructure/models/model-discovery.ts:170-303` (PARITY.md §8.12,
//! `docs/parity/adapters.md` §7.5–7.6).

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::discovery::ModelOption;

static ANSI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").expect("static regex"));

/// `--model <model>  <text>` up to the next flag, `Commands:` or end of
/// output. The lookahead is why this one needs `fancy-regex`.
static CLAUDE_MODEL_FLAG: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"--model <model>\s+([\s\S]*?)(?=\n\s{2,}--|\nCommands:|$)")
        .expect("static regex")
});

static CLAUDE_ALIAS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:^|[\s,(])['"]([a-z0-9][a-z0-9._/-]*)['"]"#).expect("static regex")
});

static CURSOR_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\S+)\s+-\s+(.+)$").expect("static regex"));

static CURSOR_STATUS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+\(([^)]+)\)\s*$").expect("static regex"));

static PI_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^provider\s{2,}model\s{2,}").expect("static regex"));

static PI_COLUMNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s{2,}").expect("static regex"));

static GROK_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*([*-])\s+([^\s].*?)(?:\s+\(default\))?\s*$").expect("static regex")
});

static GROK_DEFAULT_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+\(default\)\s*$").expect("static regex"));

static LEADING_TILDES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^~+").expect("static regex"));

static GPT_LIKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(gpt|api|ai)$").expect("static regex"));

static ASCII_LETTERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z]+$").expect("static regex"));

pub fn strip_ansi(value: &str) -> String {
    ANSI.replace_all(value, "").into_owned()
}

/// First occurrence of each `value` wins.
pub fn dedupe_options(options: Vec<ModelOption>) -> Vec<ModelOption> {
    let mut seen = std::collections::HashSet::new();
    options
        .into_iter()
        .filter(|option| seen.insert(option.value.clone()))
        .collect()
}

/// Prepend the `Default` row, unless there are no options at all (in which
/// case the caller falls back) or one is already present.
pub fn with_default(options: Vec<ModelOption>, hint: &str) -> Vec<ModelOption> {
    if options.is_empty() {
        return options;
    }
    if options.iter().any(|option| option.value.is_empty()) {
        return options;
    }
    let mut result = Vec::with_capacity(options.len() + 1);
    result.push(ModelOption::new("", "Default", hint));
    result.extend(options);
    result
}

pub fn label_from_model_id(value: &str) -> String {
    if value.is_empty() {
        return "Default".to_owned();
    }
    // A label with whitespace is already human-readable; pass it through.
    if value.chars().any(char::is_whitespace) {
        return value.to_owned();
    }

    let last = value.rsplit('/').next().unwrap_or(value);
    LEADING_TILDES
        .replace(last, "")
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            if GPT_LIKE.is_match(part) {
                return part.to_uppercase();
            }
            if ASCII_LETTERS.is_match(part) {
                let mut chars = part.chars();
                match chars.next() {
                    // `charAt(0).toUpperCase() + slice(1)` — the tail keeps
                    // its original case.
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            } else {
                part.to_uppercase()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Claude has no model-listing command, so its aliases are scraped out of
/// `claude --help`.
pub fn parse_claude_model_aliases(output: &str) -> Vec<ModelOption> {
    let plain = strip_ansi(output);
    let model_flag = CLAUDE_MODEL_FLAG
        .captures(&plain)
        .ok()
        .flatten()
        .and_then(|caps| caps.get(1).map(|m| m.as_str().to_owned()))
        .unwrap_or_default();

    let options = CLAUDE_ALIAS
        .captures_iter(&model_flag)
        .filter_map(|caps| caps.get(1))
        .map(|alias| alias.as_str().trim().to_owned())
        .filter(|alias| !alias.is_empty())
        .map(|value| {
            let hint = if value.contains('-') {
                "installed CLI example"
            } else {
                "latest alias"
            };
            ModelOption::new(&value, label_from_model_id(&value), hint)
        })
        .collect();
    dedupe_options(options)
}

/// Codex has no listing command either; this reads the CLI's own on-disk
/// `models_cache.json` and never writes it.
pub fn parse_codex_models_cache(input: &str) -> Vec<ModelOption> {
    let Ok(parsed) = serde_json::from_str::<Value>(input) else {
        return Vec::new();
    };
    let Some(models) = parsed.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut options = Vec::new();
    for item in models {
        if !item.is_object() && !item.is_array() {
            continue;
        }
        let Some(slug) = item.get("slug").and_then(Value::as_str) else {
            continue;
        };
        // `visibility` absent means visible; anything other than "list" hides
        // the model.
        match item.get("visibility") {
            Some(Value::Null) | None => {}
            Some(visibility) if visibility.as_str() == Some("list") => {}
            Some(_) => continue,
        }
        let label = item
            .get("display_name")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| label_from_model_id(slug));
        let hint = item
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("Codex cache");
        options.push(ModelOption::new(slug, label, hint));
    }
    dedupe_options(options)
}

/// `<id> - <label> (<status>)`, where the parenthesised status is optional.
pub fn parse_cursor_models(output: &str) -> Vec<ModelOption> {
    let plain = strip_ansi(output);
    let mut options = Vec::new();
    for raw_line in plain.split('\n') {
        let line = raw_line.trim();
        let Some(caps) = CURSOR_LINE.captures(line) else {
            continue;
        };
        let value = caps.get(1).expect("group 1").as_str();
        let label_and_hint = caps.get(2).expect("group 2").as_str();

        let status = CURSOR_STATUS.captures(label_and_hint);
        let (label, hint) = match &status {
            Some(caps) => {
                let whole = caps.get(0).expect("group 0");
                (
                    label_and_hint[..whole.start()].trim(),
                    caps.get(1).expect("group 1").as_str(),
                )
            }
            None => (label_and_hint.trim(), "runtime"),
        };
        options.push(ModelOption::new(value, label, hint));
    }
    dedupe_options(options)
}

/// A whitespace-aligned `provider  model  …` table; everything after the
/// second column becomes the hint.
pub fn parse_pi_models(output: &str) -> Vec<ModelOption> {
    let plain = strip_ansi(output);
    let lines: Vec<&str> = plain
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();

    let Some(header) = lines.iter().position(|line| PI_HEADER.is_match(line)) else {
        return Vec::new();
    };

    let mut options = Vec::new();
    for line in &lines[header + 1..] {
        let columns: Vec<&str> = PI_COLUMNS.split(line).collect();
        let (Some(provider), Some(model)) = (columns.first(), columns.get(1)) else {
            continue;
        };
        if provider.is_empty() || model.is_empty() {
            continue;
        }
        let rest: Vec<&str> = columns
            .iter()
            .skip(2)
            .copied()
            .filter(|column| !column.is_empty())
            .collect();
        let hint = if rest.is_empty() {
            "runtime".to_owned()
        } else {
            rest.join(" · ")
        };
        options.push(ModelOption::new(
            format!("{provider}/{model}"),
            format!("{model} ({provider})"),
            hint,
        ));
    }
    dedupe_options(options)
}

/// `  * grok-4.5 (default)` / `  - grok-build`.
pub fn parse_grok_models(output: &str) -> Vec<ModelOption> {
    let plain = strip_ansi(output);
    let mut options = Vec::new();
    for line in plain.split('\n') {
        let Some(caps) = GROK_LINE.captures(line) else {
            continue;
        };
        let marker = caps.get(1).expect("group 1").as_str();
        let is_default = line.contains("(default)") || marker == "*";
        let value = GROK_DEFAULT_SUFFIX
            .replace(caps.get(2).expect("group 2").as_str(), "")
            .trim()
            .to_owned();
        let hint = if is_default {
            "current default"
        } else {
            "runtime"
        };
        options.push(ModelOption::new(&value, label_from_model_id(&value), hint));
    }
    dedupe_options(options)
}

/// One model per line, with the known helper lines filtered out. Used by
/// opencode and antigravity.
pub fn parse_line_models(output: &str, hint: &str) -> Vec<ModelOption> {
    let plain = strip_ansi(output);
    let options = plain
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("No models available"))
        .filter(|line| !line.starts_with("Use ") && !line.starts_with('/'))
        .map(|value| ModelOption::new(value, label_from_model_id(value), hint))
        .collect();
    dedupe_options(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(options: &[ModelOption]) -> Vec<&str> {
        options.iter().map(|option| option.value.as_str()).collect()
    }

    #[test]
    fn parses_aliases_advertised_by_the_installed_claude_cli() {
        let output = "
  --model <model>  Model for the current session. Provide an alias for the latest
                   model (e.g. 'fable', 'opus', or 'sonnet') or a model's full
                   name (e.g. 'claude-fable-5').
  --name <name>    Session name
";
        let options = parse_claude_model_aliases(output);
        assert_eq!(
            values(&options),
            ["fable", "opus", "sonnet", "claude-fable-5"]
        );
        assert_eq!(options[0].hint.as_deref(), Some("latest alias"));
        assert_eq!(
            options[3].hint.as_deref(),
            Some("installed CLI example"),
            "a hyphenated alias is an example, not a floating alias"
        );
    }

    #[test]
    fn claude_aliases_are_empty_without_the_model_flag() {
        assert!(parse_claude_model_aliases("--name <name>  Session name").is_empty());
    }

    #[test]
    fn parses_visible_models_from_the_codex_cache() {
        let cache = serde_json::json!({
            "models": [
                { "slug": "gpt-current", "display_name": "GPT Current", "description": "Latest", "visibility": "list" },
                { "slug": "hidden", "display_name": "Hidden", "visibility": "hide" },
                { "slug": "gpt-compatible" },
                { "display_name": "Missing slug" },
            ]
        })
        .to_string();

        assert_eq!(
            parse_codex_models_cache(&cache),
            vec![
                ModelOption::new("gpt-current", "GPT Current", "Latest"),
                ModelOption::new("gpt-compatible", "GPT Compatible", "Codex cache"),
            ]
        );
        assert!(parse_codex_models_cache("{broken").is_empty());
        assert!(parse_codex_models_cache("{}").is_empty());
    }

    #[test]
    fn parses_cursor_model_ids_labels_and_account_status() {
        let output = "
Available models

auto - Auto (current, default)
gpt-5.6-sol-high - GPT-5.6 Sol 1M High

Tip: use --model <id>
";
        assert_eq!(
            parse_cursor_models(output),
            vec![
                ModelOption::new("auto", "Auto", "current, default"),
                ModelOption::new("gpt-5.6-sol-high", "GPT-5.6 Sol 1M High", "runtime"),
            ]
        );
    }

    #[test]
    fn parses_the_pi_provider_table_into_canonical_ids() {
        let output = "
provider   model              context  max-out  thinking  images
anthropic  claude-sonnet-4-6  200K     64K      yes       yes
openai     gpt-5.4            1M       128K     yes       yes
";
        assert_eq!(
            parse_pi_models(output),
            vec![
                ModelOption::new(
                    "anthropic/claude-sonnet-4-6",
                    "claude-sonnet-4-6 (anthropic)",
                    "200K · 64K · yes · yes",
                ),
                ModelOption::new("openai/gpt-5.4", "gpt-5.4 (openai)", "1M · 128K · yes · yes"),
            ]
        );
    }

    #[test]
    fn pi_models_need_the_table_header() {
        assert!(parse_pi_models("anthropic  claude-sonnet-4-6").is_empty());
    }

    #[test]
    fn parses_grok_models_and_marks_the_current_default() {
        let output = "
Default model: grok-4.5

Available models:
  * grok-4.5 (default)
  - grok-build
";
        assert_eq!(
            parse_grok_models(output),
            vec![
                ModelOption::new("grok-4.5", "Grok 4.5", "current default"),
                ModelOption::new("grok-build", "Grok Build", "runtime"),
            ]
        );
    }

    #[test]
    fn parses_one_model_per_line_and_ignores_helper_text() {
        let output = "
Gemini 3.6 Flash (Medium)
No models available for a disabled provider
Use /help for usage
/tmp/cache/file
Claude Sonnet 5 (Thinking)
";
        assert_eq!(
            parse_line_models(output, "runtime"),
            vec![
                ModelOption::new(
                    "Gemini 3.6 Flash (Medium)",
                    "Gemini 3.6 Flash (Medium)",
                    "runtime",
                ),
                ModelOption::new(
                    "Claude Sonnet 5 (Thinking)",
                    "Claude Sonnet 5 (Thinking)",
                    "runtime",
                ),
            ]
        );
    }

    #[test]
    fn ansi_escapes_are_stripped_before_parsing() {
        let output = "\u{1b}[32mauto\u{1b}[0m - Auto";
        assert_eq!(values(&parse_cursor_models(output)), ["auto"]);
    }

    #[test]
    fn labels_are_derived_from_the_model_id() {
        assert_eq!(label_from_model_id(""), "Default");
        assert_eq!(label_from_model_id("gpt-compatible"), "GPT Compatible");
        assert_eq!(label_from_model_id("grok-4.5"), "Grok 4.5");
        assert_eq!(label_from_model_id("grok-build"), "Grok Build");
        assert_eq!(label_from_model_id("Already Spaced"), "Already Spaced");
        assert_eq!(
            label_from_model_id("openrouter/anthropic/claude-opus"),
            "Claude Opus"
        );
        assert_eq!(label_from_model_id("~~api_key"), "API Key");
        assert_eq!(label_from_model_id("claudeX"), "ClaudeX");
    }

    #[test]
    fn duplicate_values_keep_their_first_occurrence() {
        let options = vec![
            ModelOption::new("a", "First", "one"),
            ModelOption::new("a", "Second", "two"),
            ModelOption::new("b", "Other", "three"),
        ];
        assert_eq!(
            dedupe_options(options),
            vec![
                ModelOption::new("a", "First", "one"),
                ModelOption::new("b", "Other", "three"),
            ]
        );
    }

    #[test]
    fn the_default_row_is_prepended_only_when_there_is_something_to_default_from() {
        assert!(with_default(Vec::new(), "hint").is_empty());

        let existing = vec![ModelOption::new("", "Default", "existing")];
        assert_eq!(with_default(existing.clone(), "hint"), existing);

        assert_eq!(
            with_default(vec![ModelOption::new("a", "A", "x")], "hint"),
            vec![
                ModelOption::new("", "Default", "hint"),
                ModelOption::new("a", "A", "x"),
            ]
        );
    }
}
