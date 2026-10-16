//! Linux caps a single argv string at 128 KiB (`MAX_ARG_STRLEN`), so a claude
//! run whose system prompt or task prompt travelled in argv failed `exec` with
//! `E2BIG` ("Failed to spawn process: claude"). Both prompts now go through a
//! private file and stdin; this drives the real adapter against a real `claude`
//! stand-in with >200 KB of each.

#![cfg(unix)]

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use orch_core::adapter::{Adapter, AgentEventKind};
use orch_core::process::{ProcessManager, Signal, SpawnOptions, SpawnedChild};
use orch_infra::adapters::ClaudeAdapter;
use orch_infra::process::SystemProcessManager;
use support::{collect, fixed_clock, params, with_system_prompt};

/// Records what `claude` received, then answers with a `result` line.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
out="$1"
shift
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--system-prompt-file" ]; then
    file="$2"
  fi
  shift
done
printf '%s' "$file" > "$out/path"
stat -c %a "$file" > "$out/mode"
cp "$file" "$out/system"
cat > "$out/stdin"
printf '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}\n'
"#;

/// Runs the fake script in place of `claude`, passing it the capture dir as an
/// extra leading argument.
struct FakeClaude {
    inner: SystemProcessManager,
    script: PathBuf,
    capture: PathBuf,
}

#[async_trait]
impl ProcessManager for FakeClaude {
    fn is_alive(&self, pid: u32) -> bool {
        self.inner.is_alive(pid)
    }

    fn kill(&self, pid: u32, signal: Signal) {
        self.inner.kill(pid, signal);
    }

    async fn kill_with_grace(&self, pid: u32, grace: std::time::Duration) {
        self.inner.kill_with_grace(pid, grace).await;
    }

    fn spawn(
        &self,
        command: &str,
        args: &[String],
        opts: SpawnOptions,
    ) -> orch_core::Result<SpawnedChild> {
        assert_eq!(command, "claude");
        let mut full = vec![self.capture.to_string_lossy().into_owned()];
        full.extend_from_slice(args);
        let opts = SpawnOptions {
            cwd: std::env::temp_dir(),
            ..opts
        };
        self.inner
            .spawn(&self.script.to_string_lossy(), &full, opts)
    }
}

fn big(seed: &str) -> String {
    // Leading dashes, newlines and quotes: things argv handling used to care
    // about.
    let mut text = format!("--{seed}\n-x \"quoted\" 'single' $HOME `tick`\n");
    while text.len() < 220 * 1024 {
        text.push_str(&format!("{seed} line with some filler text, é ü 漢字\n"));
    }
    text
}

fn install(dir: &Path) -> Arc<FakeClaude> {
    let script = dir.join("claude");
    std::fs::write(&script, FAKE_CLAUDE).expect("write script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let capture = dir.join("capture");
    std::fs::create_dir(&capture).expect("capture dir");
    Arc::new(FakeClaude {
        inner: SystemProcessManager::new(),
        script,
        capture,
    })
}

#[tokio::test]
async fn oversized_prompts_spawn_and_arrive_intact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pm = install(dir.path());
    let system = big("system");
    let task = big("task");

    let adapter =
        ClaudeAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock());
    let handle = adapter
        .execute(with_system_prompt(params(&task, "/ws"), &system))
        .expect("an oversized prompt must not break the spawn");
    let collected = collect(handle).await;

    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Done]);
    let seen = |name: &str| std::fs::read(pm.capture.join(name)).expect(name);
    assert_eq!(seen("stdin"), task.as_bytes(), "task prompt over stdin");
    assert_eq!(seen("system"), system.as_bytes(), "system prompt file");
    assert_eq!(String::from_utf8(seen("mode")).expect("mode").trim(), "600");

    let path = PathBuf::from(String::from_utf8(seen("path")).expect("path"));
    assert!(
        !path.exists(),
        "system prompt file {} outlived the process",
        path.display()
    );
}
