//! D1-2 .. D1-5, D5-6, D5-8 — the prompt-mode and effort CLI surfaces, driven
//! through the real binary against disposable stores.
//!
//! Where a behaviour must stay what it was (full-replacement `update -f`,
//! ordinary `status --json` shape) the *baseline* `d8c2319` binary is the
//! oracle (`ORCH_T1_OLD_BIN`); those comparisons print `SKIPPED` without it.

mod token_eco_support;

use std::fs;

use serde_json::{Value, json};
use token_eco_support::{Sandbox, new_reader, old_reader};

const ECO_HINT: &str = "Keep progress and final reporting concise";

fn project_mode(sb: &Sandbox) -> String {
    let out = sb.run(
        &new_reader(),
        &["--json", "config", "get", "defaults.agent.prompt_mode"],
        None,
    );
    assert!(out.ok(), "{}", out.all());
    serde_json::from_str::<Value>(&out.stdout).unwrap()["value"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn config_bytes(sb: &Sandbox) -> Vec<u8> {
    fs::read(sb.root().join("config.yml")).unwrap()
}

fn agent_file(sb: &Sandbox, id: &str) -> String {
    fs::read_to_string(sb.agents_dir().join(format!("{id}.yml"))).unwrap()
}

fn dispatch(sb: &Sandbox, agent: &str, title: &str) -> String {
    let new = new_reader();
    let before = sb.recorded("codex").len();
    let task = sb.run(
        &new,
        &["task", "add", title, "--assignee", agent, "--quiet"],
        None,
    );
    assert!(task.ok(), "{}", task.all());
    let run = sb.run(&new, &["run", task.stdout.trim()], None);
    assert!(run.ok(), "dispatch failed: {}", run.all());
    sb.recorded_stdin("codex", before)
        .expect("a prompt was delivered")
}

// ── D1-5: surfaces ─────────────────────────────────────────────────────────

#[test]
fn an_omitted_project_mode_reads_as_normal_and_init_writes_it_explicitly() {
    let new = new_reader();
    let sb = Sandbox::new(&new);
    assert_eq!(project_mode(&sb), "normal");
    assert!(
        String::from_utf8(config_bytes(&sb))
            .unwrap()
            .contains("prompt_mode: normal"),
        "init must write the default"
    );
    // A pre-0.10 file without the key is read as normal, and reading never rewrites it.
    let legacy = String::from_utf8(config_bytes(&sb))
        .unwrap()
        .replace("    prompt_mode: normal\n", "");
    assert!(!legacy.contains("prompt_mode"));
    fs::write(sb.root().join("config.yml"), &legacy).unwrap();
    assert_eq!(project_mode(&sb), "normal");
    sb.run(&new, &["agent", "list"], None);
    assert_eq!(
        config_bytes(&sb),
        legacy.as_bytes(),
        "reads must not backfill"
    );
}

#[test]
fn init_with_a_mode_flag_stores_it_and_an_existing_project_keeps_its_mode() {
    let new = new_reader();
    for flag in ["--prompt-mode", "--mode"] {
        let sb = Sandbox::new(&new);
        let fresh = tempfile::tempdir().unwrap();
        let proj = fresh.path().canonicalize().unwrap().join("p");
        fs::create_dir(&proj).unwrap();
        let mut cmd = std::process::Command::new(&new.bin);
        cmd.current_dir(&proj)
            .env_clear()
            .env("HOME", sb.home.path())
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                sb.home.path().join("global.yml"),
            )
            .env("PATH", "/usr/bin:/bin")
            .args(["init", flag, "eco"]);
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{flag}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let written = fs::read_to_string(proj.join(".orchestry/config.yml")).unwrap();
        assert!(written.contains("prompt_mode: eco"), "{flag}: {written}");
        // Re-init is a no-op for an initialised project (existing behaviour).
        let again = std::process::Command::new(&new.bin)
            .current_dir(&proj)
            .env_clear()
            .env("HOME", sb.home.path())
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                sb.home.path().join("global.yml"),
            )
            .env("PATH", "/usr/bin:/bin")
            .args(["init", flag, "normal"])
            .output()
            .unwrap();
        let after = fs::read_to_string(proj.join(".orchestry/config.yml")).unwrap();
        assert_eq!(
            after,
            written,
            "re-init must not change the stored mode ({})",
            String::from_utf8_lossy(&again.stderr)
        );
    }
}

#[test]
fn agent_add_stores_only_an_explicit_override_never_a_copy_of_the_project_mode() {
    let new = new_reader();
    let sb = Sandbox::new(&new);
    sb.run(
        &new,
        &["config", "set", "defaults.agent.prompt_mode", "eco"],
        None,
    );
    let mut files = Vec::new();
    for (name, flag) in [
        ("Plain", None),
        ("Inherit", Some("inherit")),
        ("Normal", Some("normal")),
        ("Eco", Some("eco")),
    ] {
        let mut args = vec![
            "agent",
            "add",
            name,
            "--adapter",
            "codex",
            "--model",
            "gpt-6.1-sol",
            "--quiet",
        ];
        if let Some(f) = flag {
            args.extend(["--prompt-mode", f]);
        }
        let out = sb.run(&new, &args, None);
        assert!(out.ok(), "{}", out.all());
        files.push((name, agent_file(&sb, out.stdout.trim())));
    }
    for (name, file) in &files {
        match *name {
            "Plain" | "Inherit" => assert!(
                !file.contains("prompt_mode"),
                "{name} must store no override:\n{file}"
            ),
            "Normal" => assert!(file.contains("prompt_mode: normal"), "{file}"),
            _ => assert!(file.contains("prompt_mode: eco"), "{file}"),
        }
    }
}

#[test]
fn flag_only_update_is_a_locked_merge_patch_that_touches_only_the_named_fields() {
    let new = new_reader();
    let sb = Sandbox::new(&new);
    let id = sb
        .run(
            &new,
            &[
                "agent",
                "add",
                "Subject",
                "--adapter",
                "codex",
                "--model",
                "gpt-6.1-sol",
                "--role",
                "Keeps this role.",
                "--max-turns",
                "9",
                "--quiet",
            ],
            None,
        )
        .stdout
        .trim()
        .to_owned();
    let before: serde_yaml_ng::Value = serde_yaml_ng::from_str(&agent_file(&sb, &id)).unwrap();
    let out = sb.run(
        &new,
        &[
            "agent",
            "update",
            &id,
            "--prompt-mode",
            "eco",
            "--role-summary",
            "Short hint.",
            "--effort",
            "high",
        ],
        None,
    );
    assert!(out.ok(), "{}", out.all());
    let after: serde_yaml_ng::Value = serde_yaml_ng::from_str(&agent_file(&sb, &id)).unwrap();
    assert_eq!(after["config"]["prompt_mode"], "eco");
    assert_eq!(after["config"]["role_summary"], "Short hint.");
    assert_eq!(after["config"]["effort"], "high");
    for (path, was) in [("role", &before["role"]), ("name", &before["name"])] {
        assert_eq!(&after[path], was, "{path} must be untouched");
    }
    assert_eq!(after["config"]["max_turns"], before["config"]["max_turns"]);
    assert_eq!(after["config"]["model"], before["config"]["model"]);
    // `inherit` and `default` clear, atomically, in one write.
    let clear = sb.run(
        &new,
        &[
            "agent",
            "update",
            &id,
            "--prompt-mode",
            "inherit",
            "--effort",
            "default",
            "--role-summary",
            "Second.",
        ],
        None,
    );
    assert!(clear.ok(), "{}", clear.all());
    let cleared: serde_yaml_ng::Value = serde_yaml_ng::from_str(&agent_file(&sb, &id)).unwrap();
    assert!(cleared["config"]["prompt_mode"].is_null() && cleared["config"]["effort"].is_null());
    assert_eq!(cleared["config"]["role_summary"], "Second.");
}

#[test]
fn mixing_a_file_with_flags_fails_before_any_io_and_neither_form_fails_too() {
    let new = new_reader();
    let sb = Sandbox::new(&new);
    let id = sb
        .run(
            &new,
            &[
                "agent",
                "add",
                "Subject",
                "--adapter",
                "codex",
                "--model",
                "gpt-6.1-sol",
                "--quiet",
            ],
            None,
        )
        .stdout
        .trim()
        .to_owned();
    let before = sb.agent_bytes();
    let doc = r#"{"name":"Subject","adapter":"codex","config":{"model":"gpt-6.1-sol"}}"#.to_owned();
    for args in [
        vec!["agent", "update", &id, "-f", "-", "--prompt-mode", "eco"],
        vec!["agent", "update", &id, "-f", "-", "--effort", "high"],
        vec!["agent", "update", &id, "-f", "-", "--role-summary", "x"],
        vec![
            "agent",
            "update",
            &id,
            "--input-format",
            "json",
            "--effort",
            "high",
        ],
        vec!["agent", "update", &id],
    ] {
        let out = sb.run(&new, &args, Some(&doc));
        assert!(!out.ok(), "{args:?} must fail: {}", out.all());
        assert_eq!(sb.agent_bytes(), before, "{args:?} changed bytes");
    }
}

#[test]
fn file_update_keeps_its_full_replacement_semantics_exactly_as_the_baseline() {
    let Some(old) = old_reader() else { return };
    let new = new_reader();
    let doc = r#"{"name":"Subject","adapter":"codex","role":"Replaced role.","config":{"model":"gpt-6.1-sol","max_turns":7}}"#;
    let normalised = |sb: &Sandbox, id: &str| agent_file(sb, id).replace(id, "<id>");
    let mut results = Vec::new();
    for reader in [&old, &new] {
        let sb = Sandbox::new(reader);
        let id = sb
            .run(
                reader,
                &[
                    "agent",
                    "add",
                    "Subject",
                    "--adapter",
                    "codex",
                    "--model",
                    "gpt-5",
                    "--role",
                    "Original.",
                    "--skills",
                    "review",
                    "--quiet",
                ],
                None,
            )
            .stdout
            .trim()
            .to_owned();
        let out = sb.run(
            reader,
            &["--json", "agent", "update", &id, "-f", "-"],
            Some(doc),
        );
        assert!(out.ok(), "{}: {}", reader.name, out.all());
        let shown: Value = serde_json::from_str(&out.stdout).unwrap();
        let keys: Vec<_> = shown.as_object().unwrap().keys().cloned().collect();
        results.push((normalised(&sb, &id), keys, shown["config"].clone()));
    }
    assert_eq!(
        results[0].0, results[1].0,
        "replaced definition differs between baseline and new binary"
    );
    assert_eq!(
        results[0].1, results[1].1,
        "update --json top-level shape changed"
    );
    assert_eq!(results[0].2, results[1].2, "update --json config changed");
}

#[test]
fn ordinary_status_and_list_json_keep_the_baseline_shape() {
    let Some(old) = old_reader() else { return };
    let new = new_reader();
    let mut shapes = Vec::new();
    for reader in [&old, &new] {
        let sb = Sandbox::new(reader);
        let id = sb
            .run(
                reader,
                &[
                    "agent",
                    "add",
                    "Subject",
                    "--adapter",
                    "codex",
                    "--model",
                    "gpt-6.1-sol",
                    "--quiet",
                ],
                None,
            )
            .stdout
            .trim()
            .to_owned();
        let status: Value = serde_json::from_str(
            &sb.run(reader, &["--json", "agent", "status", &id], None)
                .stdout,
        )
        .unwrap();
        let list: Value =
            serde_json::from_str(&sb.run(reader, &["--json", "agent", "list"], None).stdout)
                .unwrap();
        let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        let cfg = |v: &Value| {
            v["config"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        let listed = list
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id.as_str())
            .unwrap()
            .clone();
        shapes.push((keys(&status), cfg(&status), keys(&listed), cfg(&listed)));
    }
    assert_eq!(
        shapes[0], shapes[1],
        "ordinary agent JSON must not gain fields"
    );
}

#[test]
fn effective_json_reports_the_resolved_mode_and_its_source_without_touching_the_stored_agent() {
    let new = new_reader();
    let sb = Sandbox::new(&new);
    sb.run(
        &new,
        &["config", "set", "defaults.agent.prompt_mode", "eco"],
        None,
    );
    let inherited = sb
        .run(
            &new,
            &[
                "agent",
                "add",
                "I",
                "--adapter",
                "codex",
                "--model",
                "gpt-6.1-sol",
                "--quiet",
            ],
            None,
        )
        .stdout
        .trim()
        .to_owned();
    let overridden = sb
        .run(
            &new,
            &[
                "agent",
                "add",
                "O",
                "--adapter",
                "codex",
                "--model",
                "gpt-6.1-sol",
                "--prompt-mode",
                "normal",
                "--quiet",
            ],
            None,
        )
        .stdout
        .trim()
        .to_owned();
    for (id, mode, source) in [
        (&inherited, "eco", "project"),
        (&overridden, "normal", "agent"),
    ] {
        let out = sb.run(&new, &["--json", "agent", "show", id, "--effective"], None);
        assert!(out.ok(), "{}", out.all());
        let v: Value = serde_json::from_str(&out.stdout).expect("stdout is pure JSON");
        assert_eq!(
            v["effective_config"],
            json!({"prompt_mode": mode, "prompt_mode_source": source})
        );
        let mut top: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        top.sort();
        assert_eq!(top, ["agent", "effective_config"]);
        assert_eq!(v["agent"]["id"], id.as_str());
        let stored = &v["agent"]["config"]["prompt_mode"];
        assert_eq!(
            stored.is_null(),
            id == &inherited,
            "agent JSON must show the stored override only"
        );
    }
}

// ── D1-4 / D1-2 (project config) ───────────────────────────────────────────

fn with_custom_template(sb: &Sandbox, field: &str) {
    let path = sb.root().join("config.yml");
    let mut config = fs::read_to_string(&path).unwrap();
    config.push_str(&format!(
        "prompt:\n  {field}: custom {{{{ task.title }}}}\n"
    ));
    fs::write(&path, config).unwrap();
}

#[test]
fn switching_the_project_to_eco_with_custom_templates_lists_every_offender_and_writes_nothing() {
    let new = new_reader();
    for field in ["template", "system_template", "user_template"] {
        let sb = Sandbox::new(&new);
        let a = sb.seed_agent(&new, "Alpha", "codex", Some("gpt-6.1-sol"), None);
        let b = sb.seed_agent(&new, "Beta", "claude", Some("claude-sonnet-5-5"), None);
        let c = sb.seed_agent(&new, "Gamma", "codex", Some("gpt-6.1-sol"), None);
        sb.run(
            &new,
            &["agent", "update", &c, "--prompt-mode", "normal"],
            None,
        );
        with_custom_template(&sb, field);
        let (cfg, agents) = (config_bytes(&sb), sb.agent_bytes());
        for args in [vec!["config", "set", "defaults.agent.prompt_mode", "eco"]] {
            let out = sb.run(&new, &args, None);
            assert!(!out.ok(), "{field}: must refuse");
            let text = out.all();
            // The bootstrap agent inherits the project mode too, so it is an offender.
            for needle in [
                a.as_str(),
                b.as_str(),
                "agt_creator",
                &format!("prompt.{field}"),
                "--prompt-mode normal",
            ] {
                assert!(
                    text.contains(needle),
                    "{field}: message lacks `{needle}`: {text}"
                );
            }
            assert!(
                !text.contains(&c),
                "{field}: the normal-override agent is not an offender: {text}"
            );
        }
        assert_eq!(config_bytes(&sb), cfg, "{field}: config.yml changed");
        assert_eq!(sb.agent_bytes(), agents, "{field}: agent files changed");
        // Remedy: per-agent normal overrides make the switch legal.
        for id in [a.as_str(), b.as_str(), "agt_creator"] {
            assert!(
                sb.run(
                    &new,
                    &["agent", "update", id, "--prompt-mode", "normal"],
                    None
                )
                .ok(),
                "remedy for {id}"
            );
        }
        let ok = sb.run(
            &new,
            &["config", "set", "defaults.agent.prompt_mode", "eco"],
            None,
        );
        assert!(ok.ok(), "{field}: remedy should work: {}", ok.all());
    }
}

#[test]
fn an_agent_edit_that_would_make_a_custom_template_agent_eco_is_refused_with_bytes_preserved() {
    let new = new_reader();
    let sb = Sandbox::new(&new);
    let a = sb.seed_agent(&new, "Alpha", "codex", Some("gpt-6.1-sol"), None);
    with_custom_template(&sb, "template");
    let before = sb.agent_bytes();
    let out = sb.run(&new, &["agent", "update", &a, "--prompt-mode", "eco"], None);
    assert!(!out.ok());
    assert!(out.all().contains("custom templates"), "{}", out.all());
    assert_eq!(sb.agent_bytes(), before);
    let patch = sb.run(
        &new,
        &["agent", "patch", &a, "-f", "-"],
        Some(r#"{"config":{"prompt_mode":"eco"}}"#),
    );
    assert!(
        !patch.ok(),
        "patch must apply the same final-candidate check"
    );
    assert_eq!(sb.agent_bytes(), before);
}

#[test]
fn a_hand_edited_eco_plus_custom_template_project_fails_dispatch_before_any_model_child() {
    let new = new_reader();
    let sb = Sandbox::new(&new);
    let a = sb.seed_agent(&new, "Alpha", "codex", Some("gpt-6.1-sol"), None);
    with_custom_template(&sb, "template");
    let edited = String::from_utf8(config_bytes(&sb))
        .unwrap()
        .replace("prompt_mode: normal", "prompt_mode: eco");
    fs::write(sb.root().join("config.yml"), edited).unwrap();
    let task = sb
        .run(
            &new,
            &["task", "add", "t", "--assignee", &a, "--quiet"],
            None,
        )
        .stdout
        .trim()
        .to_owned();
    let run = sb.run(&new, &["run", &task], None);
    assert!(
        sb.recorded("codex").is_empty(),
        "no model child may be spawned"
    );
    let text = run.all();
    assert!(
        text.contains("custom templates") || text.contains("incompatible"),
        "diagnostic must be visible: {text}"
    );
    let agent = sb.run(&new, &["--json", "agent", "status", &a], None);
    assert_eq!(
        serde_json::from_str::<Value>(&agent.stdout).unwrap()["status"],
        "idle",
        "agent must be released"
    );
}

#[test]
fn config_set_and_patch_preserve_unknown_extension_keys_and_round_trip_through_get() {
    let new = new_reader();
    let sb = Sandbox::new(&new);
    let path = sb.root().join("config.yml");
    let mut config = fs::read_to_string(&path).unwrap();
    config.push_str("x_extension:\n  keep: me\n");
    fs::write(&path, &config).unwrap();
    assert!(
        sb.run(
            &new,
            &["config", "set", "defaults.agent.prompt_mode", "eco"],
            None
        )
        .ok()
    );
    assert_eq!(project_mode(&sb), "eco");
    let patch = sb.run(
        &new,
        &["config", "patch", "-f", "-"],
        Some(r#"{"defaults":{"agent":{"prompt_mode":"normal"}}}"#),
    );
    assert!(patch.ok(), "{}", patch.all());
    assert_eq!(project_mode(&sb), "normal");
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("x_extension") && text.contains("keep: me"),
        "extension key lost:\n{text}"
    );
    for bad in ["turbo", "ECO", ""] {
        let before = config_bytes(&sb);
        let out = sb.run(
            &new,
            &["config", "set", "defaults.agent.prompt_mode", bad],
            None,
        );
        assert!(!out.ok(), "`{bad}` must be rejected");
        assert_eq!(config_bytes(&sb), before, "`{bad}` changed config.yml");
    }
}

#[test]
fn concurrent_mode_toggles_and_unrelated_patches_never_corrupt_or_lose_either_write() {
    let new = new_reader();
    let sb = Sandbox::new(&new);
    let path = sb.root().join("config.yml");
    let mut config = fs::read_to_string(&path).unwrap();
    config.push_str("x_extension:\n  keep: me\n");
    fs::write(&path, config).unwrap();
    std::thread::scope(|scope| {
        for i in 0..4 {
            let sb = &sb;
            let new = &new;
            scope.spawn(move || {
                for round in 0..3 {
                    let mode = if (i + round) % 2 == 0 {
                        "eco"
                    } else {
                        "normal"
                    };
                    let out = sb.run(
                        new,
                        &["config", "set", "defaults.agent.prompt_mode", mode],
                        None,
                    );
                    assert!(out.ok(), "toggle {i}.{round}: {}", out.all());
                    let patch =
                        format!(r#"{{"scheduling":{{"max_concurrent_agents":{}}}}}"#, 3 + i);
                    let out = sb.run(new, &["config", "patch", "-f", "-"], Some(&patch));
                    assert!(out.ok(), "patch {i}.{round}: {}", out.all());
                }
            });
        }
    });
    let text = fs::read_to_string(&path).unwrap();
    serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&text).expect("config.yml is still valid YAML");
    assert!(text.contains("keep: me"), "extension key lost:\n{text}");
    assert!(["eco", "normal"].contains(&project_mode(&sb).as_str()));
    let cap = text
        .lines()
        .find(|l| l.trim().starts_with("max_concurrent_agents"))
        .unwrap()
        .to_owned();
    assert!((3..=6).any(|n| cap.ends_with(&format!(": {n}"))), "{cap}");
}

// ── D1-3: dispatch honours override / inheritance next dispatch ────────────

#[test]
fn an_agent_override_wins_over_the_project_mode_and_an_inheriting_agent_follows_the_next_switch() {
    let new = new_reader();
    let sb = Sandbox::new(&new);
    let fixed = sb.seed_agent(&new, "Fixed", "codex", Some("gpt-6.1-sol"), None);
    let follower = sb.seed_agent(&new, "Follower", "codex", Some("gpt-6.1-sol"), None);
    assert!(
        sb.run(
            &new,
            &["agent", "update", &fixed, "--prompt-mode", "normal"],
            None
        )
        .ok()
    );
    assert!(
        sb.run(
            &new,
            &["config", "set", "defaults.agent.prompt_mode", "eco"],
            None
        )
        .ok()
    );

    let fixed_prompt = dispatch(&sb, &fixed, "t1");
    let follower_prompt = dispatch(&sb, &follower, "t2");
    assert!(
        !fixed_prompt.contains(ECO_HINT),
        "override normal must render normal under project eco"
    );
    assert!(
        follower_prompt.contains(ECO_HINT),
        "inheriting agent must render eco"
    );

    // The switch lands on the *next* dispatch of the same agent, no restart.
    assert!(
        sb.run(
            &new,
            &["config", "set", "defaults.agent.prompt_mode", "normal"],
            None
        )
        .ok()
    );
    assert!(
        !dispatch(&sb, &follower, "t3").contains(ECO_HINT),
        "next dispatch must follow the new project mode"
    );
    // Overrides are retained across the switch.
    assert!(agent_file(&sb, &fixed).contains("prompt_mode: normal"));
}

// ── D5-6 / D5-8 ────────────────────────────────────────────────────────────

#[test]
fn clearing_the_generic_effort_keeps_a_native_model_suffix_and_never_resurrects_an_override() {
    let new = new_reader();
    let sb = Sandbox::new(&new);
    let model = "openrouter/anthropic/claude-opus-5.5#high";
    let id = sb
        .run(
            &new,
            &[
                "agent",
                "add",
                "Suffixed",
                "--adapter",
                "opencode",
                "--model",
                model,
                "--quiet",
            ],
            None,
        )
        .stdout
        .trim()
        .to_owned();
    assert!(
        sb.run(
            &new,
            &[
                "agent",
                "update",
                &id,
                "--prompt-mode",
                "eco",
                "--effort",
                "high"
            ],
            None
        )
        .ok()
    );
    let out = sb.run(&new, &["agent", "update", &id, "--effort", "default"], None);
    assert!(out.ok(), "{}", out.all());
    let file = agent_file(&sb, &id);
    assert!(
        file.contains(&format!("model: {model}")),
        "native suffix must survive:\n{file}"
    );
    assert!(
        !file.contains("effort:"),
        "generic effort must be gone:\n{file}"
    );
    assert!(
        file.contains("prompt_mode: eco"),
        "an unrelated override must not be resurrected or lost:\n{file}"
    );
    // A conflicting suffix + generic effort is refused at authoring.
    let conflict = sb.run(&new, &["agent", "update", &id, "--effort", "low"], None);
    assert!(
        !conflict.ok(),
        "suffix #high with generic low must conflict: {}",
        conflict.all()
    );
}

#[test]
fn changing_adapter_while_keeping_an_incompatible_effort_is_refused_and_clearing_it_in_the_same_edit_succeeds()
 {
    let new = new_reader();
    let sb = Sandbox::new(&new);
    let id = sb
        .run(
            &new,
            &[
                "agent",
                "add",
                "Mover",
                "--adapter",
                "claude",
                "--model",
                "claude-sonnet-5-5",
                "--effort",
                "xhigh",
                "--quiet",
            ],
            None,
        )
        .stdout
        .trim()
        .to_owned();
    let before = sb.agent_bytes();
    let refused = sb.run(
        &new,
        &["agent", "patch", &id, "-f", "-"],
        Some(r#"{"adapter":"antigravity","config":{"model":"gemini-4-argon"}}"#),
    );
    assert!(!refused.ok());
    let text = refused.all();
    for needle in [
        "xhigh",
        "antigravity",
        "low, medium, high",
        "--effort default",
    ] {
        assert!(text.contains(needle), "message lacks `{needle}`: {text}");
    }
    assert_eq!(sb.agent_bytes(), before);
    let ok = sb.run(
        &new,
        &["agent", "patch", &id, "-f", "-"],
        Some(r#"{"adapter":"antigravity","config":{"model":"gemini-4-argon","effort":null}}"#),
    );
    assert!(ok.ok(), "{}", ok.all());
}
