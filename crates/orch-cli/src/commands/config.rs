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

use orch_core::error::Result;
use orch_domain::{ACTIVITY_FILTER_PRESETS, GlobalConfig, TUI_PALETTE_NAMES, TuiPaletteName};
use orch_engine::Engine;
use serde::Serialize;
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

/// `JSON.stringify(value)` for the human line: a string keeps its quotes, and
/// an absent value prints `undefined`.
fn render_value(value: Option<&Value>) -> String {
    match value {
        Some(value) => serde_json::to_string(value).unwrap_or_else(|_| "undefined".to_owned()),
        None => "undefined".to_owned(),
    }
}

pub async fn get(engine: &Engine, ctx: &CliContext, styles: &Styles, key: &str) -> Result<()> {
    let value = engine.config_store.get(key).await?;

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
    engine: &Engine,
    _ctx: &CliContext,
    styles: &Styles,
    key: &str,
    raw: &str,
) -> Result<()> {
    // `JSON.parse` first, the raw string as the fallback.
    let parsed: Value = serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()));
    engine.config_store.set(key, parsed.clone()).await?;
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

/// `config edit` spawns the editor on the real file and prints nothing.
pub async fn edit(engine: &Engine) -> Result<()> {
    crate::editor::edit_file(&engine.paths.config_path()).await
}

pub async fn mutate(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    args: &crate::tree::DefinitionInput,
    kind: crate::document_input::MutationKind,
) -> Result<()> {
    use crate::document_input::{MutationKind, apply_mutation};
    use orch_infra::storage::{FileConfigStore, fs_utils};
    let input = crate::document_input::read_document(&args.file, args.input_format).await?;
    let _lock = fs_utils::mutation_lock(&engine.paths.root().join(".definition.lock"))
        .await
        .map_err(|err| crate::document_input::at_source(&args.file, err))?;
    let store = FileConfigStore::new(engine.paths.clone());
    let current = store.read_raw().await?;
    let candidate = apply_mutation(&current, &input, kind)
        .map_err(|err| crate::document_input::at_source(&args.file, err))?;
    let complete = kind == MutationKind::Update;
    orch_infra::storage::config::validate_raw_config(&candidate, complete)
        .map_err(|err| crate::document_input::at_source(&args.file, err))?;
    if candidate != current {
        store
            .write_raw(&candidate, complete)
            .await
            .map_err(|err| crate::document_input::at_source(&args.file, err))?;
    }
    if let Some(format) = ctx.output {
        format.print(&candidate)?;
    } else {
        print_success(styles, "Updated project configuration");
    }
    Ok(())
}

/// The two keys `config global get`/`set` understand.
fn global_value(config: &GlobalConfig, key: &str) -> Option<Value> {
    match key {
        "activity_filter" => Some(Value::String(
            config.tui.activity_filter.as_str().to_owned(),
        )),
        "palette" => Some(Value::String(config.tui.palette.as_str().to_owned())),
        _ => None,
    }
}

pub async fn global_get(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    key: &str,
) -> Result<()> {
    let config = engine.global_config_store.read().await?;
    let value = global_value(&config, key);

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
    engine: &Engine,
    _ctx: &CliContext,
    styles: &Styles,
    key: &str,
    value: &str,
) -> Result<()> {
    let mut config = engine.global_config_store.read().await?;

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
            config.tui.activity_filter = value.to_owned();
            engine.global_config_store.write(&config).await?;
            print_success(styles, &format!("{key} = {value}"));
        }
        "palette" => {
            // Lowercased before validation, and the **lowercased** form is
            // what gets stored and echoed.
            let palette = value.to_lowercase();
            let Ok(parsed) = palette.parse::<TuiPaletteName>() else {
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
            config.tui.palette = parsed;
            engine.global_config_store.write(&config).await?;
            print_success(styles, &format!("{key} = {palette}"));
        }
        _ => print_error(styles, &format!("Unknown global config key: {key}"), None),
    }
    Ok(())
}

pub async fn global_show(engine: &Engine, ctx: &CliContext, styles: &Styles) -> Result<()> {
    let config = engine.global_config_store.read().await?;

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
    use orch_domain::default_global_config;
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
    fn global_get_knows_exactly_two_keys() {
        let config = default_global_config();
        assert_eq!(
            global_value(&config, "palette"),
            Some(json!(config.tui.palette.as_str()))
        );
        assert_eq!(
            global_value(&config, "activity_filter"),
            Some(json!(config.tui.activity_filter.as_str()))
        );
        for unknown in ["tui.palette", "notifications", "", "Palette"] {
            assert_eq!(global_value(&config, unknown), None, "{unknown}");
        }
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
