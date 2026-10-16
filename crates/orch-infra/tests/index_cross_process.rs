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

// Re-enter this test executable, never a real AI CLI. Pipes release all
// writers together for each save; no polling or retry of the index oracle.
#[test]
#[ignore = "child process entry point"]
fn task_writer_process() {
    let root = std::env::var_os("ORCH_INDEX_PROBE_ROOT").unwrap();
    let writer = std::env::var("ORCH_INDEX_PROBE_WRITER").unwrap();
    let store = FileTaskStore::new(Paths::new(root));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut input = std::io::stdin().lock();
    for n in 0..TASKS {
        let mut line = String::new();
        assert_ne!(input.read_line(&mut line).unwrap(), 0);
        let mut task = make_task();
        task.id = format!("tsk_probe_{writer}_{n}");
        runtime.block_on(store.save(&task)).unwrap();
        println!("SAVED {n}");
        std::io::stdout().flush().unwrap();
    }
}

#[test]
fn simultaneous_task_saves_keep_every_durable_task_in_the_index() {
    let iterations = std::env::var("ORCH_INDEX_PROBE_ITERATIONS")
        .map(|v| v.parse::<usize>().unwrap())
        .unwrap_or(20);
    assert!((1..=20).contains(&iterations));
    let mut total_lost = 0;
    for iteration in 0..iterations {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        std::fs::create_dir_all(paths.tasks_dir()).unwrap();
        std::fs::write(paths.tasks_dir().join("_index.json"), "[]\n").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut writers = Vec::new();
        let mut readers = Vec::new();
        for writer in 0..WRITERS {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "task_writer_process", "--ignored", "--nocapture"])
                .env("HOME", temp.path())
                .env("XDG_CONFIG_HOME", temp.path().join("config"))
                .env(
                    "ORCHESTRY_GLOBAL_CONFIG_PATH",
                    temp.path().join("global.yml"),
                )
                .env("ORCH_INDEX_PROBE_ROOT", temp.path())
                .env("ORCH_INDEX_PROBE_WRITER", writer.to_string())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let stdout = child.stdout.take().unwrap();
            let tx = tx.clone();
            readers.push(std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    if line.unwrap().contains("SAVED ") {
                        let _ = tx.send(writer);
                    }
                }
            }));
            writers.push(Writer(child));
        }
        for _ in 0..TASKS {
            for writer in &mut writers {
                writeln!(writer.0.stdin.as_mut().unwrap(), "save").unwrap();
            }
            let mut completed = BTreeSet::new();
            for _ in 0..WRITERS {
                completed.insert(
                    rx.recv_timeout(Duration::from_secs(15))
                        .expect("bounded writer completion"),
                );
            }
            assert_eq!(completed.len(), WRITERS);
        }
        for writer in &mut writers {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(status) = writer.0.try_wait().unwrap() {
                    assert!(status.success(), "writer failed: {status}");
                    break;
                }
                assert!(Instant::now() < deadline, "writer did not exit");
                std::thread::yield_now();
            }
        }
        for reader in readers {
            reader.join().unwrap();
        }
        let items: Vec<serde_json::Value> =
            serde_json::from_slice(&std::fs::read(paths.tasks_dir().join("_index.json")).unwrap())
                .unwrap();
        let indexed: BTreeSet<_> = items
            .iter()
            .map(|v| v["id"].as_str().unwrap().to_owned())
            .collect();
        let durable: BTreeSet<_> = std::fs::read_dir(paths.tasks_dir())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "yml"))
            .map(|p| p.file_stem().unwrap().to_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            durable.len(),
            WRITERS * TASKS,
            "all acknowledged saves must have YAML"
        );
        let missing: Vec<_> = durable.difference(&indexed).cloned().collect();
        println!(
            "PROBE iteration={iteration} writers={WRITERS} tasks_per_writer={TASKS} yaml={} indexed={} lost={} missing={missing:?}",
            durable.len(),
            indexed.len(),
            missing.len()
        );
        total_lost += missing.len();
    }
    assert_eq!(
        total_lost, 0,
        "durable task YAML lost from the published index"
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
    use orch_core::error::Error;
    use orch_infra::storage::index::{IndexManager, ItemFormat};
    for operation in ["update", "write", "rebuild"] {
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
