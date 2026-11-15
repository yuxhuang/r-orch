//! One outside-workspace root for the judged suite and both hygiene checks.
use super::files;
use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) const SHIMS: &[&str] = &[
    "claude",
    "codex",
    "cursor",
    "cursor-agent",
    "agent",
    "opencode",
    "pi",
    "grok",
    "agy",
];
pub(super) const SESSION: &[&str] = &[
    "ORCH_AGENT_ID",
    "ORCH_AGENT_NAME",
    "ORCH_TASK_ID",
    "ORCH_SOCKET",
    "ORCH_PROJECT_ROOT",
    "ORCH_TUI_CLIENT",
    "ORCH_WEB_PASSWORD",
    "ORCH_DAEMON_DETACHED_CHILD",
    "ORCHESTRY_TASK_PROMPT",
    "ORCHESTRY_DEBUG",
    "ORCH_TUI_MOUSE",
    "ORCH_REMOTE_ENV_PASSTHROUGH",
];
pub(super) const SET: &[&str] = &[
    "HOME",
    "PATH",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "CARGO_TARGET_DIR",
    "XTASK_GATE_WITNESS_DIR",
];

// No TOML dependency is authorized in this slice. This is a fail-closed reader
// of env *names*, not a TOML parser: accept the repository's simple [env] table,
// refuse a new table/key spelling or multiline shape rather than miss a redirect.
fn config_keys(text: &str) -> Result<BTreeSet<String>> {
    let mut inside = false;
    let mut names = BTreeSet::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            ensure!(
                !line.starts_with("[env.") && !line.starts_with("[\"env\""),
                "unsupported cargo env table spelling"
            );
            inside = line == "[env]";
            continue;
        }
        if inside {
            let (key, _) = line
                .split_once('=')
                .context("unsupported multiline cargo env")?;
            let key = key.trim().trim_matches('"');
            ensure!(
                !key.is_empty()
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'),
                "unsupported cargo env key"
            );
            ensure!(names.insert(key.to_owned()), "duplicate cargo env key");
        }
    }
    Ok(names)
}
pub(super) fn preflight(
    repo: &Path,
    env: &BTreeMap<OsString, OsString>,
) -> Result<BTreeSet<String>> {
    let config = repo.join(".cargo/config.toml");
    let mut removed = if config.try_exists()? {
        config_keys(&files::text(&config, 64 * 1024)?)?
    } else {
        BTreeSet::new()
    };
    removed.extend(SESSION.iter().map(|s| s.to_string()));
    let mut refused = vec![];
    for name in env.keys() {
        let s = name.to_string_lossy();
        if s.starts_with("ORCH_") || s.starts_with("ORCHESTRY_") {
            if !removed.contains(s.as_ref()) {
                refused.push(s.into_owned());
            }
        } else if s.starts_with("XTASK_GATE_") {
            removed.insert(s.into_owned());
        }
    }
    ensure!(
        refused.is_empty(),
        "inherited variable: {}; unset the variable or run the opt-in test directly with cargo",
        refused.join(", ")
    );
    removed.extend(
        [
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "CODEX_HOME",
        ]
        .map(str::to_owned),
    );
    Ok(removed)
}

pub(super) struct Hermetic {
    pub root: PathBuf,
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub shims: PathBuf,
    pub witness: PathBuf,
    pub removed: BTreeSet<String>,
    pub pin: String,
    env: BTreeMap<OsString, OsString>,
}
impl Hermetic {
    pub(super) fn new(
        repo: &Path,
        target: &Path,
        parent: &Path,
        inherited: BTreeMap<OsString, OsString>,
    ) -> Result<Self> {
        let removed = preflight(repo, &inherited)?;
        let parent = parent.canonicalize()?;
        files::real_directories(&parent)?;
        ensure!(
            !parent.starts_with(repo)
                && !parent
                    .ancestors()
                    .any(|p| p.file_name().is_some_and(|s| s == ".orchestry")),
            "hermetic root must be outside workspace and .orchestry"
        );
        let root = files::private_dir(&parent, "r-orch-folded")?;
        let home = root.join("home");
        let cwd = root.join("cwd");
        let shims = root.join("shims");
        let witness = root.join("witness");
        for path in [&home, &cwd, &shims, &witness] {
            std::fs::create_dir(path)?;
        }
        let real_home = inherited
            .get(std::ffi::OsStr::new("HOME"))
            .context("HOME required before sentinel setup")?;
        let mut env = inherited.clone();
        for name in &removed {
            env.remove(std::ffi::OsStr::new(name));
        }
        let get = |key: &str, fallback: &str| {
            inherited
                .get(std::ffi::OsStr::new(key))
                .cloned()
                .unwrap_or_else(|| Path::new(real_home).join(fallback).into_os_string())
        };
        let pin = files::text(&repo.join("rust-toolchain.toml"), 16 * 1024)?;
        let channel: Vec<_> = pin
            .lines()
            .filter_map(|s| {
                s.trim()
                    .strip_prefix("channel")
                    .and_then(|s| s.trim().strip_prefix('='))
                    .map(str::trim)
            })
            .collect();
        ensure!(channel.len() == 1, "toolchain channel missing or ambiguous");
        let channel = channel[0].trim_matches('"');
        ensure!(
            channel.split('.').count() == 3
                && channel.bytes().all(|b| b.is_ascii_digit() || b == b'.'),
            "toolchain pin must be numeric"
        );
        let cargo_home = get("CARGO_HOME", ".cargo");
        // rustup prepends CARGO_HOME/bin when absent anywhere in PATH. Include
        // it after shims so the proxy cannot move a real CLI ahead of the shims.
        let mut paths = vec![shims.clone(), Path::new(&cargo_home).join("bin")];
        paths.extend(std::env::split_paths(
            inherited
                .get(std::ffi::OsStr::new("PATH"))
                .context("PATH required")?,
        ));
        for (key, value) in [
            ("HOME", home.clone().into_os_string()),
            ("PATH", std::env::join_paths(paths)?),
            ("CARGO_HOME", cargo_home),
            ("RUSTUP_HOME", get("RUSTUP_HOME", ".rustup")),
            ("RUSTUP_TOOLCHAIN", channel.into()),
            ("CARGO_TARGET_DIR", target.into()),
            ("XTASK_GATE_WITNESS_DIR", witness.clone().into_os_string()),
        ] {
            env.insert(key.into(), value);
        }
        env.insert("CARGO_TERM_COLOR".into(), "never".into());
        // A missing explicitly selected tool home must fail, not provision a
        // different environment while the gate is trying to judge it (TV6).
        // This fixed tooling control is not an exported credential/env value.
        env.insert("RUSTUP_AUTO_INSTALL".into(), "0".into());
        files::write(&root.join("calls.log"), b"")?;
        // Fixed script; arguments are length limited, passed through printf as
        // data and never evaluated. One append per invocation. Total evidence
        // reads are bounded; an oversized call log fails rather than passes.
        let script = b"#!/bin/sh\nname=${0##*/}\nargs=$(printf '%s' \"$*\" | /usr/bin/head -c 512)\nprintf '%s %s\\n' \"$name\" \"$args\" >> \"${0%/*}/../calls.log\"\nexit 127\n";
        use std::os::unix::fs::PermissionsExt;
        for name in SHIMS {
            let path = shims.join(name);
            files::write(&path, script)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            root,
            home,
            cwd,
            shims,
            witness,
            removed,
            pin: channel.to_owned(),
            env,
        })
    }
    pub(super) fn command(&self, repo: &Path) -> Command {
        let mut command = self.program("cargo");
        command.arg("--color").arg("never");
        let _ = repo;
        command
    }

    pub(super) fn program(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.env_clear().envs(&self.env).current_dir(&self.cwd);
        command
    }

    pub(super) fn available(&self, program: &str) -> bool {
        use std::os::unix::fs::PermissionsExt;
        self.env
            .get(std::ffi::OsStr::new("PATH"))
            .is_some_and(|path| {
                std::env::split_paths(path).any(|p| {
                    p.join(program)
                        .metadata()
                        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                })
            })
    }
    pub(super) fn witness(&self) -> Result<()> {
        let path = self.witness.join("witness.txt");
        if !path.try_exists()? {
            bail!("witness missing");
        }
        let text = files::text(&path, 16 * 1024)?;
        let mut fields = BTreeMap::new();
        for line in text.lines() {
            let (key, value) = line.split_once('=').context("witness mismatch: field")?;
            ensure!(
                key != "orch_var",
                "witness mismatch: inherited orch variable"
            );
            ensure!(
                fields.insert(key, value).is_none(),
                "witness mismatch: duplicate field"
            );
        }
        ensure!(
            fields.len() == 4
                && fields.get("schema") == Some(&"1")
                && fields.get("global_config_redirect") == Some(&"unset")
                && fields.get("home") == self.home.to_str().as_ref()
                && fields.get("path_first") == self.shims.to_str().as_ref(),
            "witness mismatch"
        );
        Ok(())
    }
    pub(super) fn hygiene(&self) -> Result<String> {
        self.witness().context("not evaluated: sentinel witness")?;
        let mut found = vec![];
        for entry in std::fs::read_dir(&self.home)?.take(128) {
            let entry = entry?;
            if entry.file_name() != ".cargo" && entry.file_name() != ".rustup" {
                found.push(super::report::diagnostic(
                    entry.file_name().as_encoded_bytes(),
                ));
            }
        }
        ensure!(
            found.is_empty(),
            "the suite wrote into a sentinel HOME: {}",
            found
                .iter()
                .map(|p| format!("<HOME>/{p}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        Ok("a sentinel HOME is still empty after the judged suite (except .cargo/.rustup)".into())
    }
    pub(super) fn cli(&self) -> Result<String> {
        self.witness().context("not evaluated: shim witness")?;
        let bytes = files::read(&self.root.join("calls.log"), 64 * 1024)?;
        ensure!(
            bytes.is_empty(),
            "a test exec'd a real agent CLI (recording shim): {}",
            super::report::diagnostic(&bytes)
        );
        Ok("no test exec'd an agent CLI through the suite PATH; cleared/custom child PATH is outside coverage".into())
    }
    pub(super) fn environment(&self) -> String {
        SET.iter()
            .map(|s| format!("set {s}\n"))
            .chain(self.removed.iter().map(|s| format!("removed {s}\n")))
            .collect()
    }
    // No Drop deletion: the caller may have an unresolved directly started child.
    // A successful caller removes only after Runner::finished and evidence copy.
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suite_environment_witness() {
        let Some(directory) = std::env::var_os("XTASK_GATE_WITNESS_DIR") else {
            return;
        };
        let home = std::env::var("HOME").unwrap();
        let path = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .next()
            .unwrap();
        let redirect = if std::env::var_os("ORCHESTRY_GLOBAL_CONFIG_PATH").is_some() {
            "set"
        } else {
            "unset"
        };
        let mut text = format!(
            "schema=1\nhome={home}\nglobal_config_redirect={redirect}\npath_first={}\n",
            path.display()
        );
        let mut names: Vec<_> = std::env::vars_os()
            .map(|(k, _)| k)
            .filter(|k| {
                let s = k.to_string_lossy();
                s.starts_with("ORCH_") || s.starts_with("ORCHESTRY_")
            })
            .collect();
        names.sort();
        for name in names {
            text.push_str(&format!("orch_var={}\n", name.to_string_lossy()));
        }
        std::fs::write(std::path::Path::new(&directory).join("witness.txt"), text).unwrap();
    }
    #[test]
    fn unknown_opt_in_refuses_names_without_values() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let env = [("ORCH_NEW_KNOB".into(), "SECRET_VALUE".into())].into();
        let e = preflight(root, &env).unwrap_err().to_string();
        assert!(e.contains("ORCH_NEW_KNOB") && !e.contains("SECRET_VALUE"));
        assert!(config_keys("[env.WHATEVER]\nvalue=1").is_err());
    }
}
