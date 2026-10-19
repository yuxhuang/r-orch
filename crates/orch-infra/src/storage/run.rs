//! File-based run store.
//!
//! Metadata is `runs/<id>.json` (atomic write); events are `runs/<id>.jsonl`
//! (append-only). There is **no `_index.json`** — `list*` scans the directory.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use orch_core::error::{Error, Result};
use orch_core::storage::RunStore;
use orch_domain::{Run, RunEvent};
use tokio_util::sync::CancellationToken;

use super::fs_utils::{
    append_jsonl, close_append_handle, ensure_dir, list_files, path_exists, read_json, read_jsonl,
    read_jsonl_tail, write_json,
};
use super::paths::Paths;

/// How many run files are read at once: macOS defaults to a
/// 256-descriptor limit and an unbounded concurrent read over a long run
/// history hits `EMFILE`.
const READ_BATCH: usize = 64;

/// `streamEvents` waits this long for the events file to appear before giving
/// up, polling every 100 ms.
const STREAM_WAIT: Duration = Duration::from_secs(30);
const STREAM_POLL: Duration = Duration::from_millis(100);

/// Max characters per line, matching the shared line reader.
const MAX_LINE_LEN: usize = 16_384;

#[derive(Debug)]
pub struct FileRunStore {
    paths: Paths,
}

impl FileRunStore {
    pub fn new(paths: Paths) -> Self {
        FileRunStore { paths }
    }

    /// Newest first, by `started_at`.
    async fn list_filtered(&self, keep: impl Fn(&Run) -> bool) -> Result<Vec<Run>> {
        ensure_dir(&self.paths.runs_dir()).await?;
        let files = list_files(&self.paths.runs_dir(), Some(".json")).await?;

        let mut runs = Vec::new();
        for batch in files.chunks(READ_BATCH) {
            let mut reads = Vec::with_capacity(batch.len());
            for file in batch {
                let id = file.strip_suffix(".json").unwrap_or(file);
                reads.push(self.read_run(id));
            }
            for run in futures::future::join_all(reads).await {
                if let Some(run) = run?
                    && keep(&run)
                {
                    runs.push(run);
                }
            }
        }

        // `new Date(b.started_at) - new Date(a.started_at)` — descending.
        // Comparing the ISO strings is equivalent for the fixed-width,
        // always-UTC format the clock produces, and does not
        // need a date parser.
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(runs)
    }

    async fn read_run(&self, id: &str) -> Result<Option<Run>> {
        read_json(&self.paths.run_path(id)?).await
    }
}

#[async_trait]
impl RunStore for FileRunStore {
    async fn save(&self, run: &Run) -> Result<()> {
        ensure_dir(&self.paths.runs_dir()).await?;
        write_json(&self.paths.run_path(&run.id)?, run).await
    }

    async fn get(&self, id: &str) -> Result<Option<Run>> {
        self.read_run(id).await
    }

    async fn list_all(&self) -> Result<Vec<Run>> {
        self.list_filtered(|_| true).await
    }

    async fn list_for_task(&self, task_id: &str) -> Result<Vec<Run>> {
        self.list_filtered(|run| run.task_id == task_id).await
    }

    async fn list_for_agent(&self, agent_id: &str) -> Result<Vec<Run>> {
        self.list_filtered(|run| run.agent_id == agent_id).await
    }

    async fn append_event(&self, run_id: &str, event: &RunEvent) -> Result<()> {
        ensure_dir(&self.paths.runs_dir()).await?;
        append_jsonl(&self.paths.run_events_path(run_id)?, event).await
    }

    async fn read_events(&self, run_id: &str) -> Result<Vec<RunEvent>> {
        read_jsonl(&self.paths.run_events_path(run_id)?).await
    }

    async fn read_events_tail(&self, run_id: &str, count: usize) -> Result<Vec<RunEvent>> {
        read_jsonl_tail(&self.paths.run_events_path(run_id)?, count).await
    }

    /// Read the events file once, from the beginning, to EOF.
    ///
    /// **This does not tail-follow**, despite what the name suggests: the
    /// stream ends at EOF. The token only lets a consumer stop early. `logs --follow` re-invokes it.
    ///
    /// Waits up to 30 s for the file to appear, because a run can be
    /// dispatched before its first event lands.
    fn stream_events(
        &self,
        run_id: &str,
        token: CancellationToken,
    ) -> BoxStream<'static, Result<RunEvent>> {
        let path = match self.paths.run_events_path(run_id) {
            Ok(path) => path,
            Err(err) => return Box::pin(stream::once(async move { Err(err) })),
        };

        Box::pin(
            stream::once(async move { read_events_after_wait(path, token).await }).flat_map(
                |result| match result {
                    Ok(events) => stream::iter(events.into_iter().map(Ok)).boxed(),
                    Err(err) => stream::once(async move { Err(err) }).boxed(),
                },
            ),
        )
    }

    fn close_run_events(&self, run_id: &str) {
        // Sync by contract (it is a cache eviction), but the handle map is
        // behind an async mutex. The run store is always called from inside a
        // runtime, so hand the removal to it rather than blocking a worker.
        if let Ok(path) = self.paths.run_events_path(run_id)
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            handle.spawn(async move { close_append_handle(&path).await });
        }
    }
}

async fn read_events_after_wait(
    path: std::path::PathBuf,
    token: CancellationToken,
) -> Result<Vec<RunEvent>> {
    let deadline = tokio::time::Instant::now() + STREAM_WAIT;
    while !token.is_cancelled() && tokio::time::Instant::now() < deadline {
        if path_exists(&path).await {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep(STREAM_POLL) => {}
            _ = token.cancelled() => break,
        }
    }
    if token.is_cancelled() || !path_exists(&path).await {
        return Ok(Vec::new());
    }
    read_events_file(&path).await
}

/// Largest run-events file read whole, matching `read_jsonl`'s threshold.
const MAX_EVENTS_READ_SIZE: u64 = 50 * 1024 * 1024;

/// Bytes read from the end of an oversize events file.
const EVENTS_TAIL_BYTES: u64 = 1024 * 1024;

/// The last [`EVENTS_TAIL_BYTES`] of `path`, starting at a line boundary.
///
/// The first line of the window is dropped: seeking to a byte offset almost
/// always lands mid-line, and half a JSON object is not a record.
async fn read_events_tail_bytes(path: &Path, size: u64) -> Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|err| Error::io(format!("open {}", path.display()), path, err))?;
    let offset = size.saturating_sub(EVENTS_TAIL_BYTES);
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|err| Error::io(format!("seek {}", path.display()), path, err))?;

    let mut buffer = Vec::with_capacity(EVENTS_TAIL_BYTES as usize);
    file.read_to_end(&mut buffer)
        .await
        .map_err(|err| Error::io(format!("read {}", path.display()), path, err))?;

    if offset > 0 {
        match buffer.iter().position(|byte| *byte == b'\n') {
            Some(first_newline) => buffer.drain(..=first_newline),
            // No newline in a megabyte: one enormous line, and there is no
            // record boundary to trust.
            None => buffer.drain(..),
        };
    }
    Ok(buffer)
}

/// Parse the events file with the shared line-reading semantics: split on `0x0A` only — `\r` is retained — skip empty
/// lines, and cap each line at 16384 characters.
///
/// WP-2 owns the pipe-reading equivalent in `orch_infra::process`. The two
/// must keep the same cap and empty-line rule; this one reads a file, so it
/// cannot simply call that one.
async fn read_events_file(path: &Path) -> Result<Vec<RunEvent>> {
    // Bounded like `read_jsonl`: an event log is append-only and unbounded in
    // principle, so reading it whole makes its size the memory ceiling. A
    // runaway agent, or a hand-placed file, should not be able to choose how
    // much we allocate.
    //
    // This cannot delegate to `read_jsonl`, whose `lines()` strips `\r`;
    // The contract is splitting on `0x0A` only and retaining `\r`. So the cap
    // is applied here with the same threshold, and the oversize path reads a
    // bounded tail rather than the whole file.
    let size = match tokio::fs::metadata(path).await {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::io(format!("stat {}", path.display()), path, err)),
    };

    let content = if size > MAX_EVENTS_READ_SIZE {
        tracing::warn!(
            path = %path.display(),
            megabytes = size / 1024 / 1024,
            "run events file too large, reading the tail only"
        );
        read_events_tail_bytes(path, size).await?
    } else {
        tokio::fs::read(path)
            .await
            .map_err(|err| Error::io(format!("read {}", path.display()), path, err))?
    };

    let mut events = Vec::new();
    for raw in content.split(|byte| *byte == b'\n') {
        if raw.is_empty() {
            continue;
        }
        let line = String::from_utf8_lossy(raw);
        let capped: String = line.chars().take(MAX_LINE_LEN).collect();
        if capped.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(&capped) {
            Ok(event) => events.push(event),
            Err(_) => {
                let preview: String = capped.chars().take(200).collect();
                tracing::warn!(line = %preview, "skipping corrupt jsonl line");
            }
        }
    }
    Ok(events)
}

use futures::StreamExt;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fs_utils::{atomic_write, clear_ensured_dirs};
    use orch_core::test_support::make_run;
    use orch_domain::{RunEventType, RunStatus};

    fn store(root: &Path) -> FileRunStore {
        clear_ensured_dirs();
        FileRunStore::new(Paths::new(root))
    }

    fn run(id: &str, task: &str, agent: &str, started_at: &str) -> Run {
        Run {
            id: id.into(),
            task_id: task.into(),
            agent_id: agent.into(),
            started_at: started_at.into(),
            ..make_run()
        }
    }

    fn event(n: u32) -> RunEvent {
        RunEvent {
            timestamp: format!("2026-10-13T16:37:5{n}.000Z"),
            event_type: RunEventType::AgentOutput,
            data: serde_json::Value::String(format!("line {n}")),
        }
    }

    fn ids(runs: &[Run]) -> Vec<&str> {
        runs.iter().map(|r| r.id.as_str()).collect()
    }

    #[tokio::test]
    async fn an_oversize_events_file_reads_a_bounded_tail() {
        // Reading an append-only log whole makes its size the memory ceiling.
        // The file here is deliberately just over the cap, and built from
        // records so the tail is parseable.
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("run_big.jsonl");

        let line = format!(
            "{}\n",
            serde_json::json!({
                "timestamp": "2026-10-13T16:37:33.426Z",
                "type": "agent_output",
                "data": "x".repeat(900)
            })
        );
        let mut file = std::fs::File::create(&path).unwrap();
        use std::io::Write;
        let mut written = 0u64;
        while written <= MAX_EVENTS_READ_SIZE {
            file.write_all(line.as_bytes()).unwrap();
            written += line.len() as u64;
        }
        file.flush().unwrap();
        drop(file);

        let events = read_events_file(&path).await.unwrap();
        assert!(!events.is_empty(), "the tail must still parse");
        // Bounded by the tail window, not by the file: the whole file holds
        // ~52k records.
        assert!(
            events.len() < 2_000,
            "read {} records, so the cap did not apply",
            events.len()
        );
    }

    #[tokio::test]
    async fn saves_and_reads_run_metadata_as_json() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());

        assert!(store.get("run_test1").await.unwrap().is_none());
        store.save(&make_run()).await.unwrap();
        assert_eq!(store.get("run_test1").await.unwrap().unwrap(), make_run());

        // JSON, not YAML, and with the trailing newline.
        let raw = tokio::fs::read_to_string(store.paths.run_path("run_test1").unwrap())
            .await
            .unwrap();
        assert!(raw.starts_with("{\n  \"id\": \"run_test1\""), "{raw}");
        assert!(raw.ends_with("}\n"));
    }

    #[tokio::test]
    async fn there_is_no_index_file() {
        // RunStore scans its directory.
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store.save(&make_run()).await.unwrap();
        store.list_all().await.unwrap();

        let files = list_files(&store.paths.runs_dir(), None).await.unwrap();
        assert_eq!(files, vec!["run_test1.json".to_owned()]);
    }

    #[tokio::test]
    async fn list_sorts_newest_first_and_filters_by_task_and_agent() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        for r in [
            run("run_old", "tsk_1", "agt_1", "2026-10-13T10:00:00.000Z"),
            run("run_new", "tsk_1", "agt_2", "2026-10-13T12:00:00.000Z"),
            run("run_mid", "tsk_2", "agt_1", "2026-10-13T11:00:00.000Z"),
        ] {
            store.save(&r).await.unwrap();
        }

        assert_eq!(
            ids(&store.list_all().await.unwrap()),
            vec!["run_new", "run_mid", "run_old"]
        );
        assert_eq!(
            ids(&store.list_for_task("tsk_1").await.unwrap()),
            vec!["run_new", "run_old"]
        );
        assert_eq!(
            ids(&store.list_for_agent("agt_1").await.unwrap()),
            vec!["run_mid", "run_old"]
        );
        assert!(store.list_for_task("nope").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn listing_an_absent_runs_directory_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        assert!(store.list_all().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unreadable_run_file_is_an_error_not_a_silent_skip() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store.save(&make_run()).await.unwrap();
        atomic_write(&store.paths.runs_dir().join("broken.json"), "{not json")
            .await
            .unwrap();

        // Malformed JSON is an error; only a missing file is tolerated.
        assert!(store.list_all().await.is_err());
    }

    #[tokio::test]
    async fn reads_more_runs_than_one_batch() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        for n in 0..(READ_BATCH * 2 + 5) {
            let id = format!("run_{n:04}");
            store.save(&Run { id, ..make_run() }).await.unwrap();
        }
        assert_eq!(store.list_all().await.unwrap().len(), READ_BATCH * 2 + 5);
    }

    #[tokio::test]
    async fn appends_and_reads_events() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());

        assert!(store.read_events("run_test1").await.unwrap().is_empty());
        for n in 0..5 {
            store.append_event("run_test1", &event(n)).await.unwrap();
        }
        let events = store.read_events("run_test1").await.unwrap();
        assert_eq!(events.len(), 5);
        assert_eq!(events[0].data, serde_json::json!("line 0"));

        let tail = store.read_events_tail("run_test1", 2).await.unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].data, serde_json::json!("line 3"));
    }

    #[tokio::test]
    async fn the_events_file_is_the_contract_line_shape() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store.append_event("run_test1", &event(0)).await.unwrap();
        store.close_run_events("run_test1");
        // Let the spawned eviction run.
        tokio::task::yield_now().await;

        let raw = tokio::fs::read_to_string(store.paths.run_events_path("run_test1").unwrap())
            .await
            .unwrap();
        // Keys in this order.
        assert_eq!(
            raw,
            "{\"timestamp\":\"2026-10-13T16:37:50.000Z\",\"type\":\"agent_output\",\"data\":\"line 0\"}\n"
        );
    }

    #[tokio::test]
    async fn stream_events_yields_the_file_then_ends() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        for n in 0..3 {
            store.append_event("run_test1", &event(n)).await.unwrap();
        }

        let streamed: Vec<RunEvent> = store
            .stream_events("run_test1", CancellationToken::new())
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert_eq!(streamed.len(), 3);
        assert_eq!(streamed[2].data, serde_json::json!("line 2"));
    }

    #[tokio::test]
    async fn stream_events_stops_immediately_when_already_cancelled() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store.append_event("run_test1", &event(0)).await.unwrap();

        let token = CancellationToken::new();
        token.cancel();
        let streamed: Vec<_> = store.stream_events("run_test1", token).collect().await;
        assert!(streamed.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn stream_events_gives_up_on_a_file_that_never_appears() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());

        // With the clock paused, the 30 s wait costs no wall time.
        let streamed: Vec<_> = store
            .stream_events("run_never", CancellationToken::new())
            .collect()
            .await;
        assert!(streamed.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn stream_events_picks_up_a_file_that_appears_during_the_wait() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let path = store.paths.run_events_path("run_late").unwrap();

        let writer = tokio::spawn({
            let path = path.clone();
            async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let line = format!("{}\n", serde_json::to_string(&event(7)).unwrap());
                tokio::fs::create_dir_all(path.parent().unwrap())
                    .await
                    .unwrap();
                tokio::fs::write(&path, line).await.unwrap();
            }
        });

        let streamed: Vec<RunEvent> = store
            .stream_events("run_late", CancellationToken::new())
            .map(|r| r.unwrap())
            .collect()
            .await;
        writer.await.unwrap();
        assert_eq!(streamed.len(), 1);
        assert_eq!(streamed[0].data, serde_json::json!("line 7"));
    }

    #[tokio::test]
    async fn stream_events_skips_a_corrupt_line_and_keeps_the_rest() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let path = store.paths.run_events_path("run_test1").unwrap();
        let good = serde_json::to_string(&event(1)).unwrap();
        atomic_write(&path, &format!("{good}\n{{not json\n\n{good}\n"))
            .await
            .unwrap();

        let streamed: Vec<RunEvent> = store
            .stream_events("run_test1", CancellationToken::new())
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert_eq!(streamed.len(), 2);
    }

    #[tokio::test]
    async fn stream_events_retains_carriage_returns_and_skips_empty_lines() {
        // Split on 0x0A only, `\r` kept, empty lines skipped.
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let path = store.paths.run_events_path("run_cr").unwrap();
        let event = serde_json::json!({
            "timestamp": "t", "type": "agent_output", "data": "has\rcr"
        });
        atomic_write(
            &path,
            &format!("\n\n{}\n\n", serde_json::to_string(&event).unwrap()),
        )
        .await
        .unwrap();

        let streamed: Vec<RunEvent> = store
            .stream_events("run_cr", CancellationToken::new())
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert_eq!(streamed.len(), 1);
        assert_eq!(streamed[0].data, serde_json::json!("has\rcr"));
    }

    #[tokio::test]
    async fn an_invalid_run_id_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        assert!(store.get("../escape").await.is_err());
        assert!(store.append_event("a/b", &event(0)).await.is_err());

        let streamed: Vec<_> = store
            .stream_events("a/b", CancellationToken::new())
            .collect()
            .await;
        assert_eq!(streamed.len(), 1);
        assert!(streamed[0].is_err());
    }

    #[tokio::test]
    async fn a_run_with_tokens_and_an_error_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let run = Run {
            status: RunStatus::Failed,
            finished_at: Some("2026-10-13T16:38:00.000Z".into()),
            pid: Some(4242),
            error: Some("boom".into()),
            tokens: Some(orch_domain::create_token_usage(100, 50, None)),
            ..make_run()
        };
        store.save(&run).await.unwrap();
        assert_eq!(store.get("run_test1").await.unwrap().unwrap(), run);
    }
}
