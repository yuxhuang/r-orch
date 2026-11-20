//! Throwaway, bounded entry-step experiment. No orch or external-service calls.
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const CLAIM_HZ: usize = 50;
const HISTORICAL_RUNS: usize = 2538;
const SQL: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA foreign_keys=ON;
PRAGMA busy_timeout=5000;
PRAGMA wal_autocheckpoint=1000;
CREATE TABLE tasks (
 project_id TEXT NOT NULL, task_id TEXT NOT NULL, status TEXT NOT NULL,
 priority INTEGER NOT NULL, assignee TEXT, revision INTEGER NOT NULL,
 title TEXT NOT NULL, description TEXT NOT NULL, created_ms INTEGER NOT NULL,
 updated_ms INTEGER NOT NULL, extra_json TEXT NOT NULL,
 PRIMARY KEY(project_id,task_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX task_queue ON tasks(project_id,status,priority,task_id);
CREATE TABLE runs (
 project_id TEXT NOT NULL, run_id TEXT NOT NULL, task_id TEXT NOT NULL,
 started_ms INTEGER NOT NULL, PRIMARY KEY(project_id,run_id),
 FOREIGN KEY(project_id,task_id) REFERENCES tasks(project_id,task_id)
) STRICT, WITHOUT ROWID;
CREATE TABLE leases (
 project_id TEXT NOT NULL, task_id TEXT NOT NULL, run_id TEXT NOT NULL,
 agent_id TEXT NOT NULL, epoch INTEGER NOT NULL, expires_ms INTEGER NOT NULL,
 PRIMARY KEY(project_id,task_id), UNIQUE(project_id,agent_id),
 FOREIGN KEY(project_id,task_id) REFERENCES tasks(project_id,task_id),
 FOREIGN KEY(project_id,run_id) REFERENCES runs(project_id,run_id)
 DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;
CREATE TABLE run_events (
 project_id TEXT NOT NULL, run_id TEXT NOT NULL, sequence INTEGER NOT NULL,
 payload TEXT NOT NULL, PRIMARY KEY(project_id,run_id,sequence),
 FOREIGN KEY(project_id,run_id) REFERENCES runs(project_id,run_id)
) STRICT, WITHOUT ROWID;
"#;

fn connect(path: &Path) -> Result<Connection> {
    if path.exists() {
        return Err("refusing existing database".into());
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(SQL)?;
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    let sync: i64 = conn.query_row("PRAGMA synchronous", [], |r| r.get(0))?;
    let fk: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    assert_eq!((mode.as_str(), sync, fk), ("wal", 1, 1));
    Ok(conn)
}

fn payload(bytes: usize) -> String {
    let mut text = String::from("{\"type\":\"agent_output\",\"text\":\"");
    while text.len() + 2 < bytes {
        // SQLite does not compress text; use bounded original synthetic content.
        text.push((b'a' + (text.len() % 26) as u8) as char);
    }
    text.push_str("\"}");
    assert_eq!(text.len(), bytes);
    text
}

fn seed(conn: &mut Connection, claims: usize, runs: usize, mib: usize) -> Result<usize> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    {
        let mut task = tx.prepare_cached("INSERT INTO tasks VALUES ('p',?1,'todo',2,NULL,0,'Synthetic task','Small claim fixture',0,0,'{}')")?;
        for i in 0..claims + HISTORICAL_RUNS + runs {
            task.execute([format!("t{i:08}")])?;
        }
        let mut run = tx.prepare_cached("INSERT INTO runs VALUES ('p',?1,?2,0)")?;
        for i in 0..HISTORICAL_RUNS {
            run.execute(params![
                format!("history{i:08}"),
                format!("t{:08}", claims + i)
            ])?;
        }
        for i in 0..runs {
            run.execute(params![
                format!("producer{i:08}"),
                format!("t{:08}", claims + HISTORICAL_RUNS + i)
            ])?;
        }
    }
    tx.commit()?;
    let data = payload(4096);
    let rows = mib * 1024 * 1024 / data.len();
    for offset in (0..rows).step_by(1000) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut insert = tx.prepare_cached("INSERT INTO run_events VALUES ('p',?1,?2,?3)")?;
            for i in offset..rows.min(offset + 1000) {
                insert.execute(params![
                    format!("history{:08}", i % HISTORICAL_RUNS),
                    (i / HISTORICAL_RUNS) as i64,
                    data
                ])?;
            }
        }
        tx.commit()?;
    }
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    Ok(rows)
}

struct Claim {
    id: usize,
    due: Instant,
    sent: Instant,
}
struct Event {
    run: String,
    sequence: usize,
    due: Instant,
    large: bool,
}
#[derive(Default)]
struct Stats {
    claims: Vec<u64>,
    enqueue: Vec<u64>,
    claim_tx: Vec<u64>,
    event_tx: Vec<u64>,
    event_rows: usize,
    event_max_age_us: u64,
    max_slice_rows: usize,
    wal_peak: u64,
}
fn us(duration: Duration) -> u64 {
    duration.as_micros() as u64
}
fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}
fn until(due: Instant) {
    if let Some(delay) = due.checked_duration_since(Instant::now()) {
        thread::sleep(delay);
    }
}
fn claim(conn: &mut Connection, request: Claim, stats: &mut Stats) -> Result<()> {
    let begin = Instant::now();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let task = format!("t{:08}", request.id);
    let run = format!("claim{:08}", request.id);
    tx.prepare_cached("INSERT INTO leases VALUES ('p',?1,?2,?3,1,300000)")?
        .execute(params![task, run, format!("agent{:08}", request.id)])?;
    tx.prepare_cached("INSERT INTO runs VALUES ('p',?1,?2,0)")?
        .execute(params![run, task])?;
    let changed = tx.prepare_cached("UPDATE tasks SET status='in_progress',revision=revision+1,assignee=?1 WHERE project_id='p' AND task_id=?2 AND status='todo' AND revision=0")?
        .execute(params![format!("agent{:08}", request.id), task])?;
    if changed != 1 {
        return Err("claim eligibility failed".into());
    }
    tx.commit()?;
    let committed = Instant::now();
    stats.claims.push(us(committed.duration_since(request.due)));
    stats
        .enqueue
        .push(us(request.sent.duration_since(request.due)));
    stats.claim_tx.push(us(committed.duration_since(begin)));
    Ok(())
}
fn writer(
    mut conn: Connection,
    high: Receiver<Claim>,
    low: Receiver<Vec<Event>>,
    wal: PathBuf,
) -> Result<(Connection, Stats)> {
    let mut stats = Stats::default();
    let mut pending = VecDeque::new();
    let small = payload(128);
    let large = payload(4096);
    let mut high_done = false;
    let mut low_done = false;
    while !(high_done && low_done && pending.is_empty()) {
        match high.try_recv() {
            Ok(request) => claim(&mut conn, request, &mut stats)?,
            Err(error) => {
                high_done = matches!(error, TryRecvError::Disconnected);
                if pending.is_empty() && !low_done {
                    match low.recv_timeout(Duration::from_micros(200)) {
                        Ok(batch) => pending.extend(batch),
                        Err(mpsc::RecvTimeoutError::Disconnected) => low_done = true,
                        Err(mpsc::RecvTimeoutError::Timeout) => (),
                    }
                    // A claim may have arrived while the low receiver waited.
                    continue;
                }
                if !pending.is_empty() {
                    let begin = Instant::now();
                    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let mut oldest = Instant::now();
                    let mut rows = 0;
                    {
                        let mut insert =
                            tx.prepare_cached("INSERT INTO run_events VALUES ('p',?1,?2,?3)")?;
                        while rows < 1000 {
                            let Some(event) = pending.pop_front() else {
                                break;
                            };
                            oldest = oldest.min(event.due);
                            insert.execute(params![
                                event.run,
                                event.sequence as i64,
                                if event.large { &large } else { &small }
                            ])?;
                            rows += 1;
                            if begin.elapsed() >= Duration::from_millis(20) {
                                break;
                            }
                        }
                    }
                    tx.commit()?;
                    stats.event_tx.push(us(begin.elapsed()));
                    stats.event_max_age_us = stats.event_max_age_us.max(us(oldest.elapsed()));
                    stats.event_rows += rows;
                    stats.max_slice_rows = stats.max_slice_rows.max(rows);
                } else if !high_done {
                    thread::sleep(Duration::from_micros(100));
                }
            }
        }
        // Observe after every commit, before a following commit can reset the WAL.
        stats.wal_peak = stats.wal_peak.max(file_size(&wal));
    }
    Ok((conn, stats))
}

fn percentiles(values: &[u64]) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort();
    let n = sorted.len();
    json!({"n":n, "p50_us":sorted[(n * 50).div_ceil(100)-1],
        "p99_us":sorted[(n * 99).div_ceil(100)-1], "max_us":sorted[n-1]})
}
#[derive(Clone, Copy)]
struct Case {
    runs: usize,
    rate: usize,
    window: u64,
    repeat: usize,
    seconds: usize,
    mib: usize,
    aligned: bool,
    stall_ms: u64,
}
fn scenario(root: &Path, case: Case) -> Result<Value> {
    let Case {
        runs,
        rate,
        window,
        repeat,
        seconds,
        mib,
        aligned,
        stall_ms,
    } = case;
    let path = root.join("events.db");
    let wal = root.join("events.db-wal");
    let mut conn = connect(&path)?;
    let expected_claims = seconds * CLAIM_HZ;
    let seeded = seed(&mut conn, expected_claims, runs, mib)?;
    let initial = file_size(&path);
    let (claim_send, claim_recv) = mpsc::sync_channel(512);
    let (event_send, event_recv) = mpsc::sync_channel(8);
    // Fixed future start gives each producer time to be scheduled before t=0.
    let start = Instant::now() + Duration::from_millis(100);
    let claims = thread::spawn(move || -> Result<()> {
        for id in 0..expected_claims {
            let due = start + Duration::from_secs_f64(id as f64 / CLAIM_HZ as f64);
            until(due);
            claim_send.send(Claim {
                id,
                due,
                sent: Instant::now(),
            })?;
        }
        Ok(())
    });
    let events = thread::spawn(move || -> Result<(usize, usize)> {
        let mut seq = vec![0; runs];
        let mut generated = 0;
        let mut bytes = 0;
        let end = start + Duration::from_secs(seconds as u64);
        let mut deadline = start + Duration::from_millis(window);
        if runs == 0 {
            return Ok((0, 0));
        }
        loop {
            until(deadline.min(end));
            let cutoff = deadline.min(end);
            let mut batch = Vec::new();
            for (run, sequence) in seq.iter_mut().enumerate() {
                let phase = if aligned {
                    0.0
                } else {
                    run as f64 / runs as f64
                };
                loop {
                    let due =
                        start + Duration::from_secs_f64((*sequence as f64 + phase) / rate as f64);
                    if due >= cutoff {
                        break;
                    }
                    let large = (*sequence + run) % 5 == 4;
                    batch.push(Event {
                        run: format!("producer{run:08}"),
                        sequence: *sequence,
                        due,
                        large,
                    });
                    *sequence += 1;
                    generated += 1;
                    bytes += if large { 4096 } else { 128 };
                }
            }
            if !batch.is_empty() {
                event_send.send(batch)?;
            }
            if cutoff == end {
                break;
            }
            deadline += Duration::from_millis(window);
        }
        Ok((generated, bytes))
    });
    let worker = thread::spawn(move || {
        // Negative control: a deliberately nonpreemptible low-lane stall must
        // appear in scheduled-arrival claim latency. Never enabled in the matrix.
        if stall_ms > 0 {
            until(start + Duration::from_millis(50));
            thread::sleep(Duration::from_millis(stall_ms));
        }
        writer(conn, claim_recv, event_recv, wal)
    });
    claims.join().map_err(|_| "claim producer panicked")??;
    let (generated, bytes) = events.join().map_err(|_| "event producer panicked")??;
    let (conn, stats) = worker.join().map_err(|_| "writer panicked")??;
    let elapsed = start.elapsed();
    let claims_stored: i64 = conn.query_row("SELECT count(*) FROM leases", [], |r| r.get(0))?;
    let total_events: i64 = conn.query_row("SELECT count(*) FROM run_events", [], |r| r.get(0))?;
    if claims_stored != expected_claims as i64
        || stats.claims.len() != expected_claims
        || generated != stats.event_rows
        || total_events != (generated + seeded) as i64
    {
        return Err("count conservation failed".into());
    }
    let wal_end = file_size(&root.join("events.db-wal"));
    let before_checkpoint = file_size(&path);
    let checkpoint: (i64, i64, i64) =
        conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    let final_bytes = file_size(&path);
    let integrity: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if integrity != "ok" || checkpoint.0 != 0 || checkpoint.1 != checkpoint.2 {
        return Err("integrity/checkpoint check failed".into());
    }
    let result = json!({
        "runs":runs,"rate_multiplier":rate,"window_ms":window,"repeat":repeat,
        "seconds":seconds,"claims_per_second":CLAIM_HZ,"seed_payload_mib":mib,
        "aligned":aligned,"negative_control_stall_ms":stall_ms,
        "elapsed_ms":elapsed.as_millis(),"events":generated,"payload_bytes":bytes,
        "claim":percentiles(&stats.claims),"producer_lag":percentiles(&stats.enqueue),
        "claim_transaction":percentiles(&stats.claim_tx),"event_transaction":percentiles(&stats.event_tx),
        "event_max_age_us":stats.event_max_age_us,"max_slice_rows":stats.max_slice_rows,
        "db_initial_bytes":initial,"db_before_checkpoint_bytes":before_checkpoint,
        "db_final_bytes":final_bytes,"db_growth_bytes":final_bytes-initial,
        "wal_peak_bytes":stats.wal_peak,"wal_end_bytes":wal_end,
        "checkpoint":{"busy":checkpoint.0,"frames":checkpoint.1,"checkpointed":checkpoint.2},
        "quick_check":integrity,"counts_match":true,
        "claim_samples_us":stats.claims
    });
    drop(conn);
    fs::remove_file(path)?;
    Ok(result)
}
fn string(value: &mut Value, key: &str) -> String {
    value
        .as_object_mut()
        .and_then(|o| o.remove(key))
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}
fn timestamp(value: &mut Value, key: &str) -> Result<i64> {
    let text = string(value, key);
    Ok(chrono::DateTime::parse_from_rfc3339(&text)?.timestamp_millis())
}
fn sizing(copy: &Path, root: &Path) -> Result<Value> {
    let mut conn = connect(&root.join("sizing.db"))?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    let initial = file_size(&root.join("sizing.db"));
    let mut yaml_bytes = 0;
    let mut json_bytes = 0;
    let mut tasks = 0;
    let mut task_logical_bytes = 0;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for entry in fs::read_dir(copy.join("tasks"))? {
        let path = entry?.path();
        if !matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("yaml" | "yml")
        ) {
            continue;
        }
        let bytes = fs::read(path)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err("task input over 4 MiB".into());
        }
        let mut value: Value = serde_yaml_ng::from_slice(&bytes)?;
        if !value.is_object() {
            return Err("task must be an object".into());
        }
        json_bytes += serde_json::to_vec(&value)?.len();
        yaml_bytes += bytes.len();
        let _ = string(&mut value, "id");
        let title = string(&mut value, "title");
        let description = string(&mut value, "description");
        let status = string(&mut value, "status");
        let priority = value
            .as_object_mut()
            .and_then(|o| o.remove("priority"))
            .and_then(|v| v.as_i64())
            .ok_or("task priority missing")?;
        let assignee = value.as_object_mut().and_then(|o| o.remove("assignee"));
        let assignee = assignee.as_ref().and_then(Value::as_str);
        let created = timestamp(&mut value, "created_at")?;
        let updated = timestamp(&mut value, "updated_at")?;
        let extra = serde_json::to_string(&value)?;
        task_logical_bytes += title.len()
            + description.len()
            + status.len()
            + extra.len()
            + assignee.map(str::len).unwrap_or(0);
        tx.execute(
            "INSERT INTO tasks VALUES ('p',?1,?2,?3,?4,0,?5,?6,?7,?8,?9)",
            params![
                format!("t{tasks:08}"),
                status,
                priority,
                assignee,
                title,
                description,
                created,
                updated,
                extra
            ],
        )?;
        tasks += 1;
    }
    tx.commit()?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    let task_db_growth = file_size(&root.join("sizing.db")) - initial;
    let mut prompts: HashMap<[u8; 32], (usize, usize)> = HashMap::new();
    let mut prompt_total = 0;
    let mut runs = 0;
    let mut missing_prompts = 0;
    for entry in fs::read_dir(copy.join("runs"))? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json")
            || path.file_name().and_then(|s| s.to_str()) == Some("_index.json")
        {
            continue;
        }
        let bytes = fs::read(path)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("run input over 8 MiB".into());
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        let Some(prompt) = value.get("prompt").and_then(Value::as_str) else {
            missing_prompts += 1;
            continue;
        };
        let hash: [u8; 32] = Sha256::digest(prompt.as_bytes()).into();
        let entry = prompts.entry(hash).or_insert((prompt.len(), 0));
        entry.1 += 1;
        prompt_total += prompt.len();
        runs += 1;
    }
    let unique_bytes: usize = prompts.values().map(|p| p.0).sum();
    let unique = prompts.len();
    // Logical model only: one 32-byte binary key per object, one per-run reference.
    let hash_layout = unique_bytes + 32 * (unique + runs);
    let result = json!({"tasks":tasks,"task_yaml_bytes":yaml_bytes,"task_json_bytes":json_bytes,
        "task_text_bytes":task_logical_bytes,"task_db_growth_bytes_including_queue_index":task_db_growth,
        "runs_with_prompt":runs,"missing_prompt_runs":missing_prompts,"unique_prompts":unique,
        "prompt_inline_bytes":prompt_total,"unique_prompt_bytes":unique_bytes,
        "prompt_duplicate_bytes":prompt_total-unique_bytes,"prompt_hash_layout_bytes":hash_layout,
        "prompt_net_logical_savings_bytes":prompt_total as i64-hash_layout as i64,
        "hash_reference_bytes":32,"private_content_emitted":false});
    drop(conn);
    fs::remove_file(root.join("sizing.db"))?;
    Ok(result)
}
fn number(args: &[String], key: &str, default: usize) -> Result<usize> {
    args.iter()
        .position(|s| s == key)
        .map(|i| {
            args.get(i + 1)
                .ok_or("missing argument value")?
                .parse()
                .map_err(Into::into)
        })
        .unwrap_or(Ok(default))
}
fn argument(args: &[String], key: &str) -> Result<PathBuf> {
    let i = args
        .iter()
        .position(|s| s == key)
        .ok_or("missing required path argument")?;
    Ok(PathBuf::from(
        args.get(i + 1).ok_or("missing path argument value")?,
    ))
}
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("help");
    if !matches!(mode, "matrix" | "case" | "sizing") {
        return Err("usage: sqlite-events-bench matrix|case|sizing --scratch NEW_DIR --output NEW_JSONL [--seconds 12 --repeats 3 --seed-mib 32 --runs 24 --rate 10 --window-ms 50 --aligned --stall-ms 0 --copy COPY_DIR]".into());
    }
    let root = argument(&args, "--scratch")?;
    let output = argument(&args, "--output")?;
    let seconds = number(&args, "--seconds", 12)?;
    let repeats = number(&args, "--repeats", 3)?;
    let mib = number(&args, "--seed-mib", 32)?;
    if seconds == 0 || seconds > 60 || repeats == 0 || repeats > 5 || mib > 1024 {
        return Err("duration/repetition/seed exceeds experiment safety cap".into());
    }
    fs::create_dir(&root)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    writeln!(
        output,
        "{}",
        json!({"kind":"environment","os":std::env::consts::OS,"arch":std::env::consts::ARCH,"sqlite":rusqlite::version(),"release_build":!cfg!(debug_assertions),"claim_budget_p99_us":50000,"claim_budget_max_us":100000})
    )?;
    if mode == "sizing" {
        let result = sizing(&argument(&args, "--copy")?, &root)?;
        writeln!(output, "{result}")?;
        println!("{result}");
    } else {
        let mut cases = Vec::new();
        let base = Case {
            runs: 0,
            rate: 1,
            window: 50,
            repeat: 0,
            seconds,
            mib,
            aligned: false,
            stall_ms: 0,
        };
        if mode == "matrix" {
            // Alternate order across repetitions to reduce monotonic thermal bias.
            for repeat in 0..repeats {
                cases.push(Case { repeat, ..base });
                let mut cells = Vec::new();
                for runs in [1, 6, 24] {
                    for rate in [1, 10] {
                        for window in [10, 50, 200] {
                            cells.push(Case {
                                runs,
                                rate,
                                window,
                                repeat,
                                ..base
                            });
                        }
                    }
                }
                if repeat % 2 == 1 {
                    cells.reverse();
                }
                cases.extend(cells);
            }
        } else {
            let runs = number(&args, "--runs", 24)?;
            let rate = number(&args, "--rate", 10)?;
            let window = number(&args, "--window-ms", 50)? as u64;
            let stall_ms = number(&args, "--stall-ms", 0)? as u64;
            if runs > 24
                || ![1, 10].contains(&rate)
                || window == 0
                || window > 200
                || stall_ms > 500
            {
                return Err("case exceeds experiment safety cap".into());
            }
            for repeat in 0..repeats {
                cases.push(Case {
                    runs,
                    rate,
                    window,
                    repeat,
                    aligned: args.iter().any(|s| s == "--aligned"),
                    stall_ms,
                    ..base
                });
            }
        }
        for case in cases {
            let result = scenario(&root, case)?;
            writeln!(output, "{result}")?;
            output.flush()?;
            println!(
                "runs={} rate={} window={} repeat={} claim={} events={} wal_peak={}",
                case.runs,
                case.rate,
                case.window,
                case.repeat,
                result["claim"],
                result["events"],
                result["wal_peak_bytes"]
            );
        }
    }
    fs::remove_dir(root)?;
    Ok(())
}
