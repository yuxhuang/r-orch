//! `orch context`.
//!
//! The shared key/value store agents exchange data through. Three quirks:
//!
//! - `context get` on a missing key prints an error but **exits 0**, and in
//!   JSON mode prints the bare literal `null`;
//! - `context delete --json` prints **nothing at all**;
//! - the TTL column is the duration since `expires_at`, which is unclamped,
//!   so a live TTL shows as a **negative** duration.

use orch_core::error::Result;
use orch_core::storage::ContextEntry;
use orch_engine::Engine;

use crate::context::CliContext;
use crate::output::{Styles, format_duration_since, print_error, print_success, print_table};

pub async fn set(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    key: &str,
    value: &str,
    ttl_ms: Option<u64>,
) -> Result<()> {
    engine.context.set(key, value, ttl_ms).await?;

    if let Some(format) = ctx.output {
        // Re-read, so the printed entry carries the stored timestamps.
        let entry = engine.context.get(key).await?;
        return format.print(&entry);
    }
    if ctx.quiet {
        println!("{key}");
        return Ok(());
    }
    print_success(styles, &format!("Set context \"{key}\""));
    Ok(())
}

pub async fn get(engine: &Engine, ctx: &CliContext, styles: &Styles, key: &str) -> Result<()> {
    let Some(entry) = engine.context.get(key).await? else {
        if let Some(format) = ctx.output {
            format.print(&serde_json::Value::Null)?;
        } else {
            // Exit 0: a missing key is not an error condition here.
            print_error(styles, &format!("Context key \"{key}\" not found"), None);
        }
        return Ok(());
    };

    if let Some(format) = ctx.output {
        return format.print(&entry);
    }
    if ctx.quiet {
        println!("{}", entry.value);
        return Ok(());
    }

    println!();
    println!("  {} = {}", entry.key, entry.value);
    if let Some(expires_at) = &entry.expires_at {
        println!("  {}", styles.dim(&format!("expires: {expires_at}")));
    }
    println!();
    Ok(())
}

pub async fn list(engine: &Engine, ctx: &CliContext, styles: &Styles) -> Result<()> {
    let entries = engine.context.list().await?;

    if let Some(format) = ctx.output {
        return format.print(&entries);
    }
    if ctx.quiet {
        for entry in &entries {
            println!("{}={}", entry.key, entry.value);
        }
        return Ok(());
    }
    if entries.is_empty() {
        println!(
            "\n  No shared context entries. Set one: {}\n",
            styles.dim("orch context set key value")
        );
        return Ok(());
    }

    println!();
    let rows: Vec<Vec<String>> = entries.iter().map(|entry| row(styles, entry)).collect();
    print_table(styles, &["KEY", "VALUE", "UPDATED", "TTL"], &rows, 2);
    // `entries` even for one — it is not pluralized.
    println!("\n  {} entries\n", entries.len());
    Ok(())
}

pub async fn delete(engine: &Engine, ctx: &CliContext, styles: &Styles, key: &str) -> Result<()> {
    engine.context.delete(key).await?;
    if !ctx.quiet && ctx.output.is_none() {
        print_success(styles, &format!("Deleted context \"{key}\""));
    }
    Ok(())
}

fn row(styles: &Styles, entry: &ContextEntry) -> Vec<String> {
    vec![
        entry.key.clone(),
        truncate(&entry.value),
        format_duration_since(&entry.updated_at),
        entry
            .expires_at
            .as_deref()
            .map(format_duration_since)
            .unwrap_or_else(|| styles.dim("—")),
    ]
}

/// `value.length > 50 ? value.slice(0, 47) + '...' : value`, in UTF-16 code
/// units.
fn truncate(value: &str) -> String {
    if value.encode_utf16().count() <= 50 {
        return value.to_owned();
    }
    let mut units = 0;
    let mut out = String::new();
    for ch in value.chars() {
        let width = ch.len_utf16();
        if units + width > 47 {
            break;
        }
        units += width;
        out.push(ch);
    }
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, value: &str, expires_at: Option<&str>) -> ContextEntry {
        ContextEntry {
            key: key.to_owned(),
            value: value.to_owned(),
            created_at: "2026-10-13T16:03:00.479Z".to_owned(),
            updated_at: jiff::Timestamp::now().to_string(),
            ttl_ms: expires_at.map(|_| 60_000),
            expires_at: expires_at.map(str::to_owned),
        }
    }

    #[test]
    fn a_long_value_is_truncated_to_fifty_with_an_ellipsis() {
        assert_eq!(truncate("short"), "short");
        let exactly_fifty = "x".repeat(50);
        assert_eq!(truncate(&exactly_fifty), exactly_fifty);

        let long = "x".repeat(51);
        let truncated = truncate(&long);
        assert_eq!(truncated.len(), 50);
        assert!(truncated.ends_with("..."));
        assert_eq!(&truncated[..47], &"x".repeat(47));
    }

    #[test]
    fn truncation_counts_utf16_units_and_never_splits_a_character() {
        // 26 astral characters are 52 UTF-16 units, so this truncates.
        let astral = "\u{1f600}".repeat(26);
        let truncated = truncate(&astral);
        assert!(truncated.ends_with("..."));
        // 47 units is 23 whole characters plus a half, so the half is dropped.
        assert_eq!(truncated.chars().count(), 23 + 3);
    }

    #[test]
    fn the_ttl_column_is_an_em_dash_without_an_expiry_and_negative_with_one() {
        let styles = Styles::plain();

        let without = row(&styles, &entry("k1", "v1", None));
        assert_eq!(without[3], "—");

        // A live TTL is in the future, and `formatDurationSince` is
        // unclamped — the `-60s`.
        let future = (jiff::Timestamp::now() + std::time::Duration::from_secs(60)).to_string();
        let with = row(&styles, &entry("k2", "v2", Some(&future)));
        assert!(with[3].starts_with('-'), "{}", with[3]);
    }

    #[test]
    fn a_row_carries_the_key_value_and_updated_age() {
        let row = row(&Styles::plain(), &entry("k1", "v1", None));
        assert_eq!(row[0], "k1");
        assert_eq!(row[1], "v1");
        assert_eq!(row[2], "0s");
    }
}
