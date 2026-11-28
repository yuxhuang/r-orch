//! Real task-store writers in separate OS processes. Removing index writer
//! serialization must make the parent observe durable YAML missing from the index.
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use orch_core::storage::TaskStore;
use orch_core::test_support::make_task;
use orch_infra::storage::paths::Paths;
use orch_infra::storage::task::FileTaskStore;

const WRITERS: usize = 4;
const TASKS: usize = 32;

struct Writer(Child);
impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// Re-enter this executable, never a real CLI. Each round still releases four
// writers together. Only the documented acquisition timeout is retryable: the
// same task ID is re-saved, never a new ID, and the raw index oracle never retries.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
enum Progress {
    Ready,
    Saved { round: usize },
    Retry { round: usize, error: String },
    Failed { round: usize, error: String },
}

fn report(progress: Progress) {
    println!("INDEX_PROBE {}", serde_json::to_string(&progress).unwrap());
    std::io::stdout().flush().unwrap();
}

#[test]
#[ignore = "child process entry point"]
fn task_writer_process() {
    let root = std::env::var_os("ORCH_INDEX_PROBE_ROOT").unwrap();
    let writer = std::env::var("ORCH_INDEX_PROBE_WRITER").unwrap();
    let fault = std::env::var("ORCH_INDEX_PROBE_FAULT").unwrap_or_default();
    let attempts: usize = std::env::var("ORCH_INDEX_PROBE_ATTEMPTS")
        .unwrap()
        .parse()
        .unwrap();
    let store = FileTaskStore::new(Paths::new(root));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut input = std::io::stdin().lock();
    report(Progress::Ready);
    for round in 0..TASKS {
        let mut line = String::new();
        assert_ne!(input.read_line(&mut line).unwrap(), 0);
        if writer == "0" && round == 0 {
            match fault.as_str() {
                "exit" => std::process::exit(23),
                "stall" => {
                    input.read_line(&mut line).unwrap();
                }
                _ => {}
            }
        }
        let mut task = make_task();
        task.id = if fault == "error" && writer == "0" {
            "invalid/id".into()
        } else {
            format!("tsk_probe_{writer}_{round}")
        };
        for attempt in 1..=attempts {
            match runtime.block_on(store.save(&task)) {
                Ok(()) => {
                    report(Progress::Saved { round });
                    break;
                }
                Err(error) => {
                    let retryable = matches!(&error, orch_core::error::Error::Io { context, source, .. }
                        if source.kind() == std::io::ErrorKind::TimedOut
                            && context.starts_with("Timed out after 5000 ms waiting for index writer lock"));
                    let error = format!("attempt={attempt}/{attempts}: {error:?}");
                    if retryable && attempt < attempts {
                        report(Progress::Retry { round, error });
                    } else {
                        report(Progress::Failed { round, error });
                        panic!("writer={writer} round={round} save failed");
                    }
                }
            }
        }
    }
}

struct ProbeWriters {
    children: Vec<Writer>,
    readers: Vec<std::thread::JoinHandle<()>>,
}
impl Drop for ProbeWriters {
    fn drop(&mut self) {
        // Stop and reap before joining pipe readers, including every error path.
        self.children.clear();
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
    }
}

type Event = (usize, Result<Progress, String>);

fn collect_round(
    rx: &std::sync::mpsc::Receiver<Event>,
    writers: &mut ProbeWriters,
    round: Option<usize>,
    budget: Duration,
    retries: &mut usize,
) -> Result<(), String> {
    let deadline = Instant::now() + budget;
    let mut completed = BTreeSet::new();
    while completed.len() < WRITERS {
        let (writer, event) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| {
                let pending: Vec<_> = writers.children.iter_mut().enumerate()
                    .filter(|(id, _)| !completed.contains(id))
                    .map(|(id, child)| (id, child.0.try_wait().unwrap()))
                    .collect();
                format!("round={round:?} unresponsive writers (None means still alive)={pending:?}: {error}")
            })?;
        match event {
            Ok(Progress::Ready) if round.is_none() => {
                completed.insert(writer);
            }
            Ok(Progress::Saved { round: saved }) if round == Some(saved) => {
                if !completed.insert(writer) {
                    return Err(format!(
                        "writer={writer} round={saved} duplicate acknowledgement"
                    ));
                }
            }
            Ok(Progress::Retry {
                round: retry,
                error,
            }) if round == Some(retry) => {
                *retries += 1;
                let diagnostic = format!(
                    "writer={writer} round={retry} retries={retries} retryable save error: {error}"
                );
                eprintln!("{diagnostic}");
                // One isolated acquisition timeout is tolerable; a shared counter
                // across all writers and rounds keeps repeated lock stalls red.
                if *retries > 1 {
                    return Err(format!(
                        "{diagnostic}; retry ceiling exceeded (maximum 1 per iteration)"
                    ));
                }
            }
            Ok(Progress::Failed { round, error }) => {
                return Err(format!("writer={writer} round={round} save error: {error}"));
            }
            // A child's stdout closes just after its final acknowledgement.
            Err(_) if round == Some(TASKS - 1) && completed.contains(&writer) => {}
            other => {
                let status = writers.children[writer].0.try_wait().unwrap();
                return Err(format!(
                    "writer={writer} round={round:?} status={status:?}: {other:?}"
                ));
            }
        }
    }
    Ok(())
}

fn scripted_round(
    retries: &mut usize,
    round: usize,
    retry_writers: &[usize],
) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    for &writer in retry_writers {
        tx.send((
            writer,
            Ok(Progress::Retry {
                round,
                error: "injected timeout".into(),
            }),
        ))
        .unwrap();
    }
    for writer in 0..WRITERS {
        tx.send((writer, Ok(Progress::Saved { round }))).unwrap();
    }
    let mut writers = ProbeWriters {
        children: Vec::new(),
        readers: Vec::new(),
    };
    collect_round(
        &rx,
        &mut writers,
        Some(round),
        Duration::from_secs(1),
        retries,
    )
}

#[test]
fn one_isolated_retry_is_tolerated_across_later_rounds() {
    let mut retries = 0;
    scripted_round(&mut retries, 0, &[]).unwrap();
    scripted_round(&mut retries, 1, &[2]).unwrap();
    scripted_round(&mut retries, 2, &[]).unwrap();
    assert_eq!(retries, 1);
}

#[test]
fn second_retry_in_one_round_fails_for_same_or_different_writers() {
    for retry_writers in [[1, 1], [1, 3]] {
        let mut retries = 0;
        let error = scripted_round(&mut retries, 3, &retry_writers).unwrap_err();
        assert_eq!(
            error,
            format!(
                "writer={} round=3 retries=2 retryable save error: injected timeout; retry ceiling exceeded (maximum 1 per iteration)",
                retry_writers[1]
            )
        );
    }
}

#[test]
fn second_retry_in_a_later_round_exceeds_the_iteration_ceiling() {
    let mut retries = 0;
    scripted_round(&mut retries, 0, &[1]).unwrap();
    let error = scripted_round(&mut retries, 5, &[2]).unwrap_err();
    assert_eq!(
        error,
        "writer=2 round=5 retries=2 retryable save error: injected timeout; retry ceiling exceeded (maximum 1 per iteration)"
    );
}

fn run_writers(
    root: &std::path::Path,
    fault: &str,
    budget: Duration,
    attempts: usize,
) -> Result<usize, String> {
    let mut retries = 0;
    let (tx, rx) = std::sync::mpsc::channel();
    let mut writers = ProbeWriters {
        children: Vec::new(),
        readers: Vec::new(),
    };
    for writer in 0..WRITERS {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "task_writer_process", "--ignored", "--nocapture"])
            .env_clear()
            .env("HOME", root)
            .env("TMPDIR", root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("ORCHESTRY_GLOBAL_CONFIG_PATH", root.join("global.yml"))
            .env("ORCH_INDEX_PROBE_ROOT", root)
            .env("ORCH_INDEX_PROBE_WRITER", writer.to_string())
            .env("ORCH_INDEX_PROBE_FAULT", fault)
            .env("ORCH_INDEX_PROBE_ATTEMPTS", attempts.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let tx = tx.clone();
        writers.readers.push(std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if let Some(json) = line.strip_prefix("INDEX_PROBE ") {
                            let event = serde_json::from_str(json)
                                .map_err(|e| format!("invalid progress: {e}"));
                            if tx.send((writer, event)).is_err() {
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        let _ = tx.send((writer, Err(format!("stdout read: {error}"))));
                        return;
                    }
                }
            }
            let _ = tx.send((
                writer,
                Err("child stdout closed before acknowledgement".into()),
            ));
        }));
        writers.children.push(Writer(child));
    }
    drop(tx);
    collect_round(
        &rx,
        &mut writers,
        None,
        Duration::from_secs(30),
        &mut retries,
    )?;
    for round in 0..TASKS {
        for (writer, child) in writers.children.iter_mut().enumerate() {
            writeln!(child.0.stdin.as_mut().unwrap(), "save")
                .map_err(|error| format!("writer={writer} round={round} send: {error}"))?;
        }
        collect_round(&rx, &mut writers, Some(round), budget, &mut retries)?;
    }
    for (id, writer) in writers.children.iter_mut().enumerate() {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = writer.0.try_wait().unwrap() {
                if !status.success() {
                    return Err(format!("writer={id} exit: {status}"));
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!("writer={id} did not exit"));
            }
            std::thread::yield_now();
        }
    }
    Ok(retries)
}

fn snapshot(paths: &Paths) -> (BTreeSet<String>, BTreeSet<String>) {
    let items: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(paths.tasks_dir().join("_index.json")).unwrap())
            .unwrap();
    let indexed = items
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_owned())
        .collect();
    let durable = std::fs::read_dir(paths.tasks_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "yml"))
        .map(|p| p.file_stem().unwrap().to_str().unwrap().to_owned())
        .collect();
    (durable, indexed)
}

fn prepare(root: &std::path::Path) -> Paths {
    let paths = Paths::new(root);
    std::fs::create_dir_all(paths.tasks_dir()).unwrap();
    std::fs::write(paths.tasks_dir().join("_index.json"), "[]\n").unwrap();
    paths
}

#[test]
fn simultaneous_task_saves_keep_every_durable_task_in_the_index() {
    let iterations = std::env::var("ORCH_INDEX_PROBE_ITERATIONS")
        .map(|v| v.parse::<usize>().unwrap())
        .unwrap_or(20);
    assert!((1..=20).contains(&iterations));
    for iteration in 0..iterations {
        let temp = tempfile::tempdir().unwrap();
        let paths = prepare(temp.path());
        let result = run_writers(temp.path(), "", Duration::from_secs(40), 3);
        // run_writers has reaped every child, so even a failed round has a stable
        // raw snapshot. Never call list/rebuild: that would heal a missing entry.
        eprintln!("PROBE iteration={iteration} writer result={result:?}");
        let (durable, indexed) = snapshot(&paths);
        let missing: Vec<_> = durable.difference(&indexed).collect();
        println!(
            "PROBE iteration={iteration} writers={WRITERS} tasks_per_writer={TASKS} yaml={} indexed={} lost={} missing={missing:?} result={result:?}",
            durable.len(),
            indexed.len(),
            missing.len()
        );
        let retries = result.unwrap();
        println!("PROBE iteration={iteration} retries={retries}");
        assert_eq!(
            durable.len(),
            WRITERS * TASKS,
            "all acknowledged saves must have YAML"
        );
        assert_eq!(
            durable, indexed,
            "durable task YAML lost from the published index"
        );
    }
}

#[test]
fn child_save_error_preserves_writer_round_and_original_error() {
    let temp = tempfile::tempdir().unwrap();
    prepare(temp.path());
    let error = run_writers(temp.path(), "error", Duration::from_secs(40), 3).unwrap_err();
    assert!(
        error.contains("writer=0 round=0 save error: attempt=1/3:") && error.contains("invalid/id"),
        "{error}"
    );
}

#[test]
fn child_exit_is_reported_without_an_acknowledgement_timeout() {
    let temp = tempfile::tempdir().unwrap();
    prepare(temp.path());
    let error = run_writers(temp.path(), "exit", Duration::from_secs(40), 3).unwrap_err();
    assert!(
        error.contains("writer=0 round=Some(0)") && error.contains("stdout closed"),
        "{error}"
    );
}

#[test]
fn live_child_without_progress_has_a_bounded_round_deadline() {
    let temp = tempfile::tempdir().unwrap();
    prepare(temp.path());
    let error = run_writers(temp.path(), "stall", Duration::from_millis(200), 3).unwrap_err();
    assert!(
        error.contains("round=Some(0) unresponsive writers") && error.contains("(0, None)"),
        "{error}"
    );
}

fn hold_index_lock(dir: &std::path::Path) -> std::fs::File {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".index.lock"))
        .unwrap();
    file.try_lock().unwrap();
    file
}

#[tokio::test]
async fn every_index_writer_times_out_under_external_contention_without_publishing() {
    use futures::FutureExt;
    use std::panic::AssertUnwindSafe;

    use orch_core::error::Error;
    use orch_infra::storage::index::{IndexManager, ItemFormat};
    async fn case(operation: &str) {
        let temp = tempfile::tempdir().unwrap();
        let index = IndexManager::<serde_json::Value>::new(temp.path().into(), ItemFormat::Json);
        std::fs::write(index.index_path(), "[]\n").unwrap();
        let lock = hold_index_lock(temp.path());
        let started = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(7), async {
            match operation {
                "update" => {
                    index
                        .update_index(|_| vec![serde_json::json!({"id": "new"})])
                        .await
                }
                "write" => index.write_index(&[serde_json::json!({"id": "new"})]).await,
                _ => index.rebuild_index().await.map(|_| ()),
            }
        })
        .await
        .expect("writer lock wait must be bounded");
        let error = result.expect_err("every writer must respect the external lock");
        assert!(
            matches!(&error, Error::Io { source, .. } if source.kind() == std::io::ErrorKind::TimedOut),
            "{error:?}"
        );
        assert!(error.to_string().contains("retry the operation"), "{error}");
        assert!(started.elapsed() >= Duration::from_secs(5));
        assert_eq!(std::fs::read_to_string(index.index_path()).unwrap(), "[]\n");
        // A cache hit is read-only and must not queue behind a writer.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), index.read_index())
                .await
                .unwrap()
                .unwrap()
                .is_empty()
        );
        drop(lock);
        index
            .update_index(|_| vec![serde_json::json!({"id": "after"})])
            .await
            .unwrap();
        assert_eq!(
            index.read_index().await.unwrap(),
            vec![serde_json::json!({"id": "after"})]
        );
    }

    // Each case owns its fixture. Collect panics so a failed assertion cannot
    // cancel siblings before their lock waits and post-release checks finish.
    let (update, write, rebuild) = tokio::join!(
        AssertUnwindSafe(case("update")).catch_unwind(),
        AssertUnwindSafe(case("write")).catch_unwind(),
        AssertUnwindSafe(case("rebuild")).catch_unwind(),
    );
    for outcome in [update, write, rebuild] {
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
}

#[tokio::test]
async fn cancelled_lock_waiter_cannot_apply_its_mutation_later() {
    use orch_infra::storage::index::{IndexManager, ItemFormat};
    let temp = tempfile::tempdir().unwrap();
    let index = IndexManager::<serde_json::Value>::new(temp.path().into(), ItemFormat::Json);
    std::fs::write(index.index_path(), "[]\n").unwrap();
    let lock = hold_index_lock(temp.path());
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let mut waiting = Box::pin(index.update_index(move |items| {
        let _ = started_tx.send(());
        items
    }));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut waiting)
            .await
            .is_err()
    );
    drop(waiting);
    assert!(
        started_rx.await.is_err(),
        "cancelled waiter retained its mutation"
    );
    drop(lock);
    index
        .update_index(|_| vec![serde_json::json!({"id": "after"})])
        .await
        .unwrap();
    assert_eq!(
        index.read_index().await.unwrap(),
        vec![serde_json::json!({"id": "after"})]
    );
}

#[tokio::test]
async fn lock_file_is_ignored_when_rebuilding_yaml_and_json_indexes() {
    use orch_infra::storage::index::{IndexManager, ItemFormat};
    for (format, name, contents) in [
        (ItemFormat::Yaml, "item.yml", "id: item\n"),
        (ItemFormat::Json, "item.json", "{\"id\":\"item\"}\n"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(name), contents).unwrap();
        let index = IndexManager::<serde_json::Value>::new(temp.path().into(), format);
        index.rebuild_index().await.unwrap();
        assert!(
            temp.path().join(".index.lock").is_file(),
            "writer must create the stable lock"
        );
        std::fs::write(temp.path().join(".index.lock"), "not an entity").unwrap();
        assert_eq!(
            index.rebuild_index().await.unwrap(),
            vec![serde_json::json!({"id": "item"})]
        );
    }
}

#[test]
#[ignore = "child process entry point"]
fn index_lock_holder_process() {
    use orch_infra::storage::index::{IndexManager, ItemFormat};
    let root = std::env::var_os("ORCH_INDEX_PROBE_ROOT").unwrap();
    let index = IndexManager::<serde_json::Value>::new(root.into(), ItemFormat::Json);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(index.update_index(|items| {
            println!("LOCKED");
            std::io::stdout().flush().unwrap();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).unwrap();
            items
        }))
        .unwrap();
}

#[tokio::test]
async fn crashed_process_releases_its_index_writer_lock() {
    use orch_infra::storage::index::{IndexManager, ItemFormat};
    let temp = tempfile::tempdir().unwrap();
    let mut child = Writer(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "index_lock_holder_process",
                "--ignored",
                "--nocapture",
            ])
            .env("HOME", temp.path())
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                temp.path().join("global.yml"),
            )
            .env("ORCH_INDEX_PROBE_ROOT", temp.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line.unwrap().contains("LOCKED") {
                tx.send(()).unwrap();
                break;
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let contender = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(temp.path().join(".index.lock"))
        .expect("writer must create the lock");
    assert!(
        matches!(contender.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "writer must hold its advisory lock"
    );
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());
    reader.join().unwrap();
    let index = IndexManager::<serde_json::Value>::new(temp.path().into(), ItemFormat::Json);
    tokio::time::timeout(
        Duration::from_secs(7),
        index.write_index(&[serde_json::json!({"id": "after"})]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        index.read_index().await.unwrap(),
        vec![serde_json::json!({"id": "after"})]
    );
}
