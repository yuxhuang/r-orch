// Included inside worker::tests so the env tests share the hermetic fixture.
//
// Security review F2: work submitted by a remote peer must not inherit the
// worker's credentials. The secrets have to be in the *orch process's own*
// environment to prove anything, and this crate forbids the `unsafe`
// `set_var`, so the parent re-executes this test binary with canaries set and
// the child runs one mirror and one local task through the real shell adapter.

const ENV_CHILD_OUT: &str = "ORCH_F2_CHILD_OUT";
const CANARY_KEY: &str = "f2-canary-api-key-value";
const CANARY_TOKEN: &str = "f2-canary-token-value";

#[cfg(unix)]
#[tokio::test]
async fn remote_mirror_runs_without_worker_secrets_but_local_runs_keep_them() {
    let out = tempfile::tempdir().unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "worker::tests::env_redaction_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ENV_CHILD_OUT, out.path())
        .env("HOME", out.path())
        .env("ORCH_F2_CANARY_API_KEY", CANARY_KEY)
        .env("ORCH_F2_CANARY_TOKEN", CANARY_TOKEN)
        .env("ORCH_F2_PASSTHROUGH_TOKEN", "f2-passthrough-value")
        .env(
            orch_core::process::REMOTE_ENV_PASSTHROUGH,
            "ORCH_F2_PASSTHROUGH_TOKEN",
        )
        .env("ORCH_F2_PLAIN", "f2-plain-value")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(60), child.output())
        .await
        .expect("env redaction child deadline")
        .unwrap();
    assert!(
        output.status.success(),
        "env redaction child failed: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Guards against the filter matching no test: the child really ran.
    assert!(out.path().join("done").exists(), "child did not run");
}

/// Names (never values) of secret-looking variables in an `env` dump.
fn secret_names(dump: &str) -> Vec<String> {
    dump.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, _)| name)
        .filter(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .filter(|name| orch_core::process::is_secret_env_name(name))
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn env_redaction_child() {
    let Some(out) = std::env::var_os(ENV_CHILD_OUT).map(std::path::PathBuf::from) else {
        return; // only meaningful when re-executed by the parent test
    };
    let (temp, worker, agent_id) = fixture().await;
    let mut agent = worker
        .ctx
        .engine
        .agents
        .get(&agent_id)
        .await
        .unwrap()
        .unwrap();
    agent.config.command = Some(format!("env > \"${ENV_CHILD_OUT}/$ORCH_TASK_ID.env\""));
    agent.config.approval_policy = Some(orch_domain::ApprovalPolicy::Auto);
    agent.config.workspace_mode = Some(orch_domain::WorkspaceMode::Shared);
    agent.config.timeout_ms = Some(5_000);
    agent.config.env = Some(
        [
            ("HOME".to_owned(), temp.path().to_str().unwrap().to_owned()),
            // The owner's explicit per-agent choice survives the filter.
            (
                "ORCH_F2_EXPLICIT_API_KEY".to_owned(),
                "f2-explicit-value".to_owned(),
            ),
        ]
        .into_iter()
        .collect(),
    );
    worker.ctx.engine.agents.save(&agent).await.unwrap();

    worker
        .handle("controller", "C", submit("env-key", &agent_id))
        .await
        .unwrap();
    let remote = mirror(&worker, "env-key").await;
    // Sequential: a single-task run treats every other dispatchable task as
    // claimed, so the local control is only created once the mirror is done.
    let execution = timeout(Duration::from_secs(20), async {
        worker.ctx.engine.orchestrator.run_task(&remote.id).await?;
        worker.ctx.engine.orchestrator.wait_for_collectors().await;
        let local = worker
            .ctx
            .engine
            .task_service
            .create(CreateTaskInput {
                title: "local control".into(),
                assignee: Some(agent_id.clone()),
                ..Default::default()
            })
            .await?;
        worker.ctx.engine.orchestrator.run_task(&local.id).await?;
        worker.ctx.engine.orchestrator.wait_for_collectors().await;
        Result::<Task>::Ok(local)
    })
    .await;
    let stopped = worker.ctx.engine.orchestrator.stop().await;
    let local = execution.unwrap().unwrap();
    stopped.unwrap();

    let read = |id: &str| {
        let path = out.join(format!("{id}.env"));
        for _ in 0..200 {
            if let Ok(dump) = std::fs::read_to_string(&path)
                && !dump.is_empty()
            {
                return dump;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let listing: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        panic!("no env dump at {} (have {listing:?})", path.display());
    };
    let remote_env = read(&remote.id);
    let local_env = read(&local.id);

    // The control proves the canaries were inheritable at all.
    assert!(local_env.contains(CANARY_KEY));
    assert!(local_env.contains(CANARY_TOKEN));

    assert!(!remote_env.contains(CANARY_KEY), "API key canary leaked");
    assert!(!remote_env.contains(CANARY_TOKEN), "token canary leaked");
    let mut leaked = secret_names(&remote_env);
    leaked.retain(|name| name != "ORCH_F2_PASSTHROUGH_TOKEN" && name != "ORCH_F2_EXPLICIT_API_KEY");
    assert!(leaked.is_empty(), "mirror inherited secret-named vars: {leaked:?}");

    assert!(remote_env.contains("ORCH_F2_PLAIN=f2-plain-value"));
    assert!(remote_env.contains("ORCH_F2_PASSTHROUGH_TOKEN=f2-passthrough-value"));
    assert!(remote_env.contains("ORCH_F2_EXPLICIT_API_KEY=f2-explicit-value"));
    assert!(remote_env.contains(&format!("ORCH_TASK_ID={}", remote.id)));

    std::fs::write(out.join("done"), "").unwrap();
}
