//! D2-1 — normal prompt byte identity against bytes frozen from `d8c2319`.
//!
//! The fixtures in `fixtures/token-eco/baseline-prompts/` were captured by
//! running this same file with `T1_CAPTURE_DIR` set inside a disposable
//! `git archive d8c2319` export. Here (no env var) every case is rendered by
//! the current tree and compared byte for byte with the frozen `(system,
//! user)` strings: no normalization, no hashes.

#[path = "token_eco_cases/mod.rs"]
mod cases;

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Frozen {
    name: String,
    /// The user half (or the whole legacy prompt).
    user: String,
    system: Option<String>,
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/token-eco/baseline-prompts")
}

#[tokio::test]
async fn capture_or_compare_normal_prompts_against_the_frozen_baseline() {
    let capture = std::env::var_os("T1_CAPTURE_DIR");
    let all = cases::all_cases();
    assert!(all.len() >= 25, "case list shrank to {}", all.len());
    let mut failures = Vec::new();
    for case in &all {
        let (user, system) = cases::render_like_baseline_dispatch(case).await;
        let got = Frozen {
            name: case.name.to_owned(),
            user,
            system,
        };
        if let Some(dir) = &capture {
            let dir = PathBuf::from(dir);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join(format!("{}.json", case.name)),
                serde_json::to_string_pretty(&got).unwrap() + "\n",
            )
            .unwrap();
            continue;
        }
        let path = fixture_dir().join(format!("{}.json", case.name));
        let frozen: Frozen = serde_json::from_str(
            &fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
        )
        .unwrap();
        if frozen.user != got.user {
            failures.push(format!("{}: user half differs", case.name));
        }
        if frozen.system != got.system {
            failures.push(format!("{}: system half differs", case.name));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_frozen_fixture_has_a_case_and_vice_versa() {
    if std::env::var_os("T1_CAPTURE_DIR").is_some() {
        return;
    }
    let mut on_disk: Vec<String> = fs::read_dir(fixture_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    let mut wanted: Vec<String> = cases::all_cases()
        .iter()
        .map(|c| c.name.to_owned())
        .collect();
    wanted.sort();
    assert_eq!(on_disk, wanted);
}
