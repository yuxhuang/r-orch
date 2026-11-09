//! `orch config`.
//!
//! Four things here are easy to get wrong:
//!
//! - **all three `--json` sites are compact**, with no indent, and
//!   `config get` on a missing key **drops the `value` key entirely** rather
//!   than emitting `null`;
//! - `config set` tries `JSON.parse` first and falls back to the raw string,
//!   so `5000` is a number and `nope` is a string;
//! - `config global set` with a bad value or an unknown key prints an error
//!   and still **exits 0**;
//! - `config global get` knows exactly two keys and answers `undefined` for
//!   anything else.
//!
//! The prompt mode adds one rule (design D1): a `set`, `patch` or `update`
//! that could change which agents render eco, or whether a custom template is
//! configured, is validated as a complete candidate **under the definition
//! lock** — the enum, and every agent that would resolve to eco against a
//! custom template — before anything is written. `defaults.agent.prompt_mode`
//! itself is written key-only, so unknown extension keys and unset defaults
//! stay exactly as they were.

use super::rpc::Rpc;
use orch_core::error::{Error, Result};
use orch_domain::{ACTIVITY_FILTER_PRESETS, TUI_PALETTE_NAMES, TuiPaletteName};
use orch_proto::ops::{Op, OpClass, Request, args};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::CliContext;
use crate::output::{Styles, print_error, print_success};

/// `{ key, value }`, where an absent value drops the key.
#[derive(Debug, Serialize)]
struct KeyValue<'a> {
    key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<&'a Value>,
}

/// Unlike `Option<Value>`'s default deserializer, retain a present JSON null.
#[derive(Serialize, Deserialize)]
struct ConfigValue {
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    value: Option<Value>,
}
fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

// Reuse the closed request DTO and wire operation; only the presentation
// result reader differs so an authored null does not become a missing key.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct ConfigRead(args::ConfigGetArgs);
impl Op for ConfigRead {
    const NAME: &'static str = <args::ConfigGetArgs as Op>::NAME;
    const CLASS: OpClass = <args::ConfigGetArgs as Op>::CLASS;
    type Output = ConfigValue;
    fn into_request(self) -> Request {
        self.0.into_request()
    }
}

pub(crate) async fn get_value(engine: &Rpc, key: &str) -> Result<Option<Value>> {
    Ok(engine
        .call::<ConfigRead>(serde_json::json!({"key":key}))
        .await?
        .value)
}

/// `JSON.stringify(value)` for the human line: a string keeps its quotes, and
/// an absent value prints `undefined`.
fn render_value(value: Option<&Value>) -> String {
    match value {
        Some(value) => serde_json::to_string(value).unwrap_or_else(|_| "undefined".to_owned()),
        None => "undefined".to_owned(),
    }
}

pub async fn get(engine: &Rpc, ctx: &CliContext, styles: &Styles, key: &str) -> Result<()> {
    let value = get_value(engine, key).await?;

    if let Some(format) = ctx.output {
        return format.print_compact(&KeyValue {
            key,
            value: value.as_ref(),
        });
    }
    println!("  {} = {}", styles.dim(key), render_value(value.as_ref()));
    Ok(())
}

pub async fn set(
    engine: &Rpc,
    _ctx: &CliContext,
    styles: &Styles,
    key: &str,
    raw: &str,
) -> Result<()> {
    let parsed = set_value(engine, key, raw).await?;
    // No JSON mode: the success line is all `config set` prints.
    print_success(
        styles,
        &format!(
            "{key} = {}",
            serde_json::to_string(&parsed).unwrap_or_default()
        ),
    );
    Ok(())
}

/// Parse and validate the value in the daemon, which owns the definition lock.
pub(crate) async fn set_value(engine: &Rpc, key: &str, raw: &str) -> Result<Value> {
    engine
        .call::<args::ConfigSetArgs>(serde_json::json!({"key":key,"value":raw}))
        .await?;
    Ok(serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned())))
}

/// Edit an authored snapshot; a concurrent edit is a conflict, never overwritten.
pub async fn edit(engine: &Rpc) -> Result<()> {
    let base = engine.request(args::ConfigDefinitionGetArgs {}).await?;
    let input = super::org_template::edit_document(&base.document).await?;
    super::org_template::document_request(engine, &input, |input| {
        args::ConfigDefinitionApplyArgs {
            definition: args::DefinitionInput {
                input,
                kind: args::MutationKind::Update,
                base_digest: Some(base.digest),
            },
        }
    })
    .await?;
    Ok(())
}

pub async fn mutate(
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    args: &crate::tree::DefinitionInput,
    kind: crate::document_input::MutationKind,
) -> Result<()> {
    let input = crate::document_input::read_document(&args.file, args.input_format).await?;
    let file = args.file.clone();
    let source = move |err| super::org_template::at_source(&file, err);
    let candidate = apply_definition(engine, &input, kind, &source).await?;
    if let Some(format) = ctx.output {
        format.print(&candidate)?;
    } else {
        print_success(styles, "Updated project configuration");
    }
    Ok(())
}

/// Apply the document under the daemon's definition lock; validation and
/// publication run in one operation.
pub(crate) async fn apply_definition(
    engine: &Rpc,
    input: &Value,
    kind: crate::document_input::MutationKind,
    source: &(dyn Fn(Error) -> Error + Sync),
) -> Result<Value> {
    let result = super::org_template::document_request(engine, input, |input| {
        args::ConfigDefinitionApplyArgs {
            definition: args::DefinitionInput {
                input,
                kind: super::org_template::wire_kind(kind),
                base_digest: None,
            },
        }
    })
    .await
    .map_err(source)?;
    serde_json::from_str(&result.document).map_err(|error| Error::other(error.to_string()))
}

/// The project prompt-mode key.
pub const PROMPT_MODE_KEY: &str = "defaults.agent.prompt_mode";

pub async fn global_get(engine: &Rpc, ctx: &CliContext, styles: &Styles, key: &str) -> Result<()> {
    let value = engine
        .call::<args::ConfigGlobalGetArgs>(serde_json::json!({"key":key}))
        .await?
        .value;

    if let Some(format) = ctx.output {
        return format.print_compact(&KeyValue {
            key,
            value: value.as_ref(),
        });
    }
    println!("  {} = {}", styles.dim(key), render_value(value.as_ref()));
    Ok(())
}

/// An invalid value and an unknown key both print an error and **exit 0**.
pub async fn global_set(
    engine: &Rpc,
    _ctx: &CliContext,
    styles: &Styles,
    key: &str,
    value: &str,
) -> Result<()> {
    match key {
        "activity_filter" => {
            if !ACTIVITY_FILTER_PRESETS.contains(&value) {
                print_error(
                    styles,
                    &format!(
                        "Invalid value \"{value}\". Valid: {}",
                        ACTIVITY_FILTER_PRESETS.join(", ")
                    ),
                    None,
                );
                return Ok(());
            }
            engine
                .call::<args::ConfigGlobalSetArgs>(serde_json::json!({"key":key,"value":value}))
                .await?;
            print_success(styles, &format!("{key} = {value}"));
        }
        "palette" => {
            // Lowercased before validation, and the **lowercased** form is
            // what gets stored and echoed.
            let palette = value.to_lowercase();
            let Ok(_) = palette.parse::<TuiPaletteName>() else {
                print_error(
                    styles,
                    &format!(
                        "Invalid value \"{value}\". Valid: {}",
                        TUI_PALETTE_NAMES
                            .iter()
                            .map(|name| name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    None,
                );
                return Ok(());
            };
            engine
                .call::<args::ConfigGlobalSetArgs>(serde_json::json!({"key":key,"value":palette}))
                .await?;
            print_success(styles, &format!("{key} = {palette}"));
        }
        _ => print_error(styles, &format!("Unknown global config key: {key}"), None),
    }
    Ok(())
}

pub async fn global_show(engine: &Rpc, ctx: &CliContext, styles: &Styles) -> Result<()> {
    let config = engine.request(args::ConfigGlobalShowArgs {}).await?;

    if let Some(format) = ctx.output {
        return format.print_compact(&config);
    }
    // Raw, not JSON-quoted — unlike `global get`.
    println!(
        "  {} = {}",
        styles.dim("tui.activity_filter"),
        config.tui.activity_filter.as_str()
    );
    println!(
        "  {} = {}",
        styles.dim("tui.palette"),
        config.tui.palette.as_str()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_missing_value_drops_the_key_in_json_and_prints_undefined() {
        let missing = KeyValue {
            key: "nope.nope",
            value: None,
        };
        assert_eq!(
            serde_json::to_string(&missing).expect("serialize"),
            r#"{"key":"nope.nope"}"#
        );
        assert_eq!(render_value(None), "undefined");
    }

    #[test]
    fn a_present_value_keeps_its_json_form() {
        let value = json!(10_000);
        assert_eq!(
            serde_json::to_string(&KeyValue {
                key: "scheduling.poll_interval_ms",
                value: Some(&value),
            })
            .expect("serialize"),
            r#"{"key":"scheduling.poll_interval_ms","value":10000}"#
        );
        assert_eq!(render_value(Some(&value)), "10000");
        // A string value keeps its quotes, as `JSON.stringify` gives.
        assert_eq!(render_value(Some(&json!("amber"))), "\"amber\"");
    }

    #[test]
    fn set_parses_a_json_scalar_and_falls_back_to_the_raw_string() {
        for (raw, expected) in [
            ("5000", json!(5000)),
            ("true", json!(true)),
            ("null", json!(null)),
            ("\"quoted\"", json!("quoted")),
            ("[1,2]", json!([1, 2])),
            ("not json", json!("not json")),
            ("", json!("")),
        ] {
            let parsed: Value =
                serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()));
            assert_eq!(parsed, expected, "{raw:?}");
        }
    }
}
