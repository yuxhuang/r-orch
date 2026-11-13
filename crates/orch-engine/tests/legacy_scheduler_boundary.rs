//! Pin only the mixed live/legacy boundaries. The compiler and doctests prove
//! exclusion of the private helpers and public scheduler surface respectively.

use std::path::{Path, PathBuf};

const RETRY: &str = r#"
        #[cfg(test)]
        if !self.is_daemon_mode() {
            self.process_retry_queue(state).await?;
        }
"#;
const SCHEDULE: &str = r#"
    pub(super) async fn schedule_dispatch_after_run(&self, task_id: &str) {
        #[cfg(test)]
        {
            let was_single = self.inner.single_task_runs.lock().await.remove(task_id);
            if !was_single {
                self.schedule_immediate_dispatch(0);
            }
        }
        #[cfg(not(test))]
        let _ = task_id;
    }
"#;
const SUCCESS: &str = r#"
    pub(super) async fn handle_run_success(
        &self,
        task_id: &str,
        run_id: &str,
        agent_id: &str,
        tokens: Option<TokenUsage>,
        result_text: Option<String>,
        files_changed: Vec<String>,
    ) -> Result<()> {
        #[cfg(test)]
        if !self.is_daemon_mode() {
            let mut slot = self.inner.state.lock().await;
            let Some(state) = slot.as_mut() else {
                return Ok(());
            };
            return self.handle_run_success_locked(
                state,
                task_id,
                run_id,
                agent_id,
                tokens,
                result_text,
                files_changed,
            )
            .await;
        }
        self.daemon_handle_success(
            task_id,
            run_id,
            agent_id,
            tokens,
            result_text,
            files_changed,
        )
        .await
    }
"#;
const MAILBOX: &str = r#"
                    #[cfg(test)]
                    (Some(messages), MailboxMode::Drain) => Ok(messages
                        .drain_mailbox(&agent.id, &task_id)
                        .await
                        .unwrap_or_default()),
"#;

fn compact(text: &str) -> String {
    text.split_whitespace().collect()
}

// Four fixed sites, not a Rust item parser. Missing/renamed anchors are errors;
// the exact function body prevents an extra unconditional scheduling call.
fn fixed_method<'a>(source: &'a str, signature: &str) -> Result<&'a str, String> {
    if source.matches(signature).count() != 1 {
        return Err(format!("missing or repeated boundary: {signature}"));
    }
    let start = source.find(signature).unwrap();
    let end = source[start..]
        .find("\n    }\n")
        .ok_or_else(|| format!("missing boundary end: {signature}"))?;
    Ok(&source[start..start + end + "\n    }".len()])
}

fn retry_boundary(source: &str) -> Result<(), String> {
    let reconcile = fixed_method(source, "pub(super) async fn reconcile(")?;
    if reconcile.matches("process_retry_queue").count() != 1
        || !compact(reconcile).contains(&compact(RETRY))
    {
        return Err("reconcile retry boundary must be test-only".into());
    }
    Ok(())
}

fn method_boundary(source: &str, name: &str, expected: &str) -> Result<(), String> {
    let signature = format!("pub(super) async fn {name}(");
    if compact(fixed_method(source, &signature)?) != compact(expected) {
        return Err(format!("{name} boundary changed"));
    }
    Ok(())
}

fn mailbox_boundary(source: &str) -> Result<(), String> {
    let source = compact(source);
    let arm = source
        .split_once("match(self.deps().message_service.as_ref(),mailbox){")
        .and_then(|(_, arms)| arms.split_once("(Some(messages),MailboxMode::Snapshot)"))
        .map(|(drain, _)| drain)
        .ok_or("missing mailbox preparation boundary")?;
    if arm != compact(MAILBOX) || !source.contains("enumMailboxMode{#[cfg(test)]Drain,Snapshot,}") {
        return Err("MailboxMode::Drain variant and arm must be test-only".into());
    }
    Ok(())
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn no_features(source: &str, manifest: &str) -> bool {
    !compact(source).contains("feature=") && !compact(manifest).contains("[features]")
}

fn unused_allow_count(source: &str) -> usize {
    let source = compact(source);
    ["allow", "expect"]
        .into_iter()
        .map(|kind| {
            source
                .split(&format!("{kind}("))
                .skip(1)
                .filter(|attribute| {
                    attribute.split(')').next().unwrap().split(',').any(|lint| {
                        lint == "dead_code" || lint.starts_with("unused") || lint == "warnings"
                    })
                })
                .count()
        })
        .sum()
}

#[test]
fn mixed_boundaries_exclude_legacy_scheduling() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/orchestrator");
    retry_boundary(&read(&src.join("tick.rs"))).unwrap();
    let collect = read(&src.join("collect.rs"));
    method_boundary(&collect, "schedule_dispatch_after_run", SCHEDULE).unwrap();
    method_boundary(&collect, "handle_run_success", SUCCESS).unwrap();
    mailbox_boundary(&read(&src.join("dispatch.rs"))).unwrap();
}

#[test]
fn boundary_checks_reject_missing_misplaced_and_unconditional_gates() {
    let retry = format!("pub(super) async fn reconcile() {{\n{RETRY}\n    }}\n");
    retry_boundary(&retry).unwrap();
    assert!(retry_boundary(&retry.replace("#[cfg(test)]", "")).is_err());
    assert!(
        retry_boundary(&retry.replace(
            "#[cfg(test)]",
            "let unrelated = (); #[cfg(test)] let other = ();"
        ))
        .is_err()
    );
    assert!(
        retry_boundary(&retry.replace(
            "if !self.is_daemon_mode()",
            "self.process_retry_queue(state).await?; if !self.is_daemon_mode()"
        ))
        .is_err()
    );
    for (name, fixture) in [
        ("schedule_dispatch_after_run", SCHEDULE),
        ("handle_run_success", SUCCESS),
    ] {
        method_boundary(fixture, name, fixture).unwrap();
        assert!(method_boundary(&fixture.replace("#[cfg(test)]", ""), name, fixture).is_err());
        assert!(
            method_boundary(
                &fixture.replace("#[cfg(test)]", "#[cfg(test)] let unrelated = ();"),
                name,
                fixture
            )
            .is_err()
        );
        assert!(
            method_boundary(
                &fixture.replace(
                    "#[cfg(test)]",
                    "self.schedule_immediate_dispatch(0); #[cfg(test)]"
                ),
                name,
                fixture
            )
            .is_err()
        );
        assert!(method_boundary(&fixture.replace(name, "renamed"), name, fixture).is_err());
    }
    let mailbox = format!(
        "enum MailboxMode {{ #[cfg(test)] Drain, Snapshot, }} match (self.deps().message_service.as_ref(), mailbox) {{ {MAILBOX} (Some(messages), MailboxMode::Snapshot) => {{}} }}"
    );
    mailbox_boundary(&mailbox).unwrap();
    assert!(mailbox_boundary(&mailbox.replacen("#[cfg(test)]", "", 1)).is_err());
    assert!(
        mailbox_boundary(&mailbox.replace(MAILBOX, &MAILBOX.replace("#[cfg(test)]", ""))).is_err()
    );
    assert!(
        mailbox_boundary(&mailbox.replace(MAILBOX, &format!("unconditional_call(); {MAILBOX}")))
            .is_err()
    );
    assert!(mailbox_boundary(&mailbox.replace("#[cfg(test)]", "")).is_err());
    assert!(
        mailbox_boundary(&mailbox.replace("#[cfg(test)] Drain", "Drain, #[cfg(test)] Other"))
            .is_err()
    );
    assert!(
        mailbox_boundary(&mailbox.replace("MailboxMode::Drain", "MailboxMode::Renamed")).is_err()
    );
}

#[test]
fn normal_library_has_no_feature_escape_or_unused_allowance() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_path = root.join("Cargo.toml");
    let manifest = read(&manifest_path);
    assert!(
        no_features("", &manifest),
        "feature table in {}",
        manifest_path.display()
    );
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    assert!(files.len() > 50, "empty or incomplete source scan");
    for path in files {
        assert!(
            no_features(&read(&path), ""),
            "feature gate in {}",
            path.display()
        );
    }
    // Pin per-file counts, so an allowance cannot move to hide another item.
    for file in [
        "orchestrator.rs",
        "lib.rs",
        "orchestrator/dispatch.rs",
        "orchestrator/tick.rs",
        "orchestrator/collect.rs",
        "orchestrator/recovery.rs",
    ] {
        assert_eq!(
            unused_allow_count(&read(&root.join("src").join(file))),
            0,
            "{file}"
        );
    }
}

#[test]
fn policy_checks_have_positive_controls() {
    assert!(no_features("#[cfg(test)] fn legacy() {}", "[dependencies]"));
    assert!(!no_features(
        "#[cfg(feature = \"legacy\")] fn legacy() {}",
        ""
    ));
    assert!(!no_features("", "[features]\nlegacy = []"));
    assert_eq!(unused_allow_count("#[cfg(test)] fn legacy() {}"), 0);
    for kind in ["allow", "expect"] {
        assert_eq!(
            unused_allow_count(&format!("#[{kind}(clippy::all, reason = \"fixture\")]")),
            0
        );
        for lint in ["dead_code", "unused", "unused_imports", "warnings"] {
            let attribute = format!("{kind}( {lint}, reason = \"fixture\" )");
            assert_eq!(
                unused_allow_count(&format!("#[{attribute}] fn legacy() {{}}")),
                1
            );
            assert_eq!(
                unused_allow_count(&format!(
                    "#[cfg_attr(not(test), {attribute})] fn legacy() {{}}"
                )),
                1
            );
        }
    }
}
