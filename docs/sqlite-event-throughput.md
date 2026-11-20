# Run-event throughput and claim latency

Entry step D of release 1.1. Experimental code only; no product storage changes.
Baseline: `fb6a1e1`. Budget and method written before measurement on 2026-11-20.

## Budget declared before measurement

Proposed budget: claim arrival through committed transaction **p99 <= 50 ms and
max <= 100 ms**, with 1, 6 or 24 continuous event producers. Queue time is included.
The 50 ms target matches the proposed highest writer lane's longest wait, leaving
no excuse for a large event transaction to hide its delay. The maximum is a
separate tail guard, not permission to promise a hard real-time guarantee.
The scheduler is not a request-per-keystroke service; 50 ms is small relative to
starting an agent process while still detecting starvation.

Measure **50 claims/s**, stronger admission pressure than the initial 20/s proposal,
to obtain 1,800 claims per matrix cell across three independent 12-second runs.
There is no request/response pacing: timestamps start at scheduled arrival, including
producer lateness. Report per-repetition percentiles as well as pooled results.

## Method (predeclared)

- rusqlite 0.41.1, bundled SQLite, release build with pinned Rust 1.99.1.
- One owned writer thread, priority claims before each event transaction; WAL,
  synchronous NORMAL, foreign keys on, busy timeout 5 s, BEGIN IMMEDIATE, STRICT.
- Event primary key `(project_id, run_id, sequence)` WITHOUT ROWID. A claim inserts
  a lease and run and changes one eligible task atomically. Use deferred foreign
  keys to allow lease-before-run insertion; an eligibility failure aborts.
- Matrix: 1/6/24 runs, nominal/10x events, 10/50/200 ms global batch windows;
  three repetitions each. Include a no-events claim baseline.
- Nominal synthetic traffic: one event/s/run, repeating four 128-byte JSON records
  and one 4,096-byte JSON record (921.6 bytes/s, 276,480 bytes in five minutes).
  Five minutes is an **assumption**, not a measured run duration; the total matches
  the owner's supplied roughly 279 KB/run. Tenfold means ten events/s/run.
  Runs have staggered phases; also test aligned event bursts at the heaviest load.
- Transaction slices are at most 1,000 rows, with a 20 ms elapsed check after every
  insert. Claims can run between slices; already-running SQLite statements/commit
  cannot be preempted. Keep default 1,000-page automatic WAL checkpointing.
- Each run has a fresh database with 2,538 historical run keys and about 32 MiB of
  synthetic event payload. Add a roughly 700 MB history sensitivity check.
- Record counts (generated must equal committed), claim p50/p99/max, transaction
  durations, event age, physical database growth, WAL peak/end size, checkpoint
  progress and process peak RSS. Run bounded and sequentially on each platform.
- Task/YAML and exact rendered-prompt hash sizing use an external, temporary copy;
  publish aggregates only. Exact whole-prompt deduplication is distinct from common
  prompt prefixes. Do not infer savings from a shared template alone.

## Verdict

**Events in SQLite do not meet the proposed budget with these settings on the
measured macOS host. Under this predeclared budget the file fallback must ship:**
append-only files behind the blob adapter, with one SQLite index row per batch.
This is entry-gate evidence, not authorization to build product storage before
owner approval. No fallback performance claim is made: it has not been measured.

The Linux matrix met the budget in all 54 event-loaded repetitions. Its default
24-run/10x/50 ms cell was p99 **8.314 ms**, max **13.969 ms**, against its claim-only
control p99 **8.501 ms**, max **12.314 ms**. Across both matrices and four
sensitivity cells, **75,600 claims and 108,216 events** were conserved, excluding
the deliberately stalled negative control. A pass in this Linux VM does not
clear the observed failure on supported macOS.

At the proposed 50 ms window, 24 producers and tenfold events, the macOS pooled
claim p99 was **1,912.886 ms**, max **2,083.163 ms**. The three individual p99s ranged
from **28.492 to 1,989.291 ms**. At nominal events, that same concurrency/window
also failed: pooled p99 **53.518 ms**, max **170.092 ms**. Across 10/50/200 ms
windows at 24 producers and tenfold events, pooled Mac p99s
were 229.558 / 1,912.886 / 232.067 ms; all failed. The claim-only control
was p99 **10.947 ms**, max **20.645 ms**. Keep the failures; neither pooling only
fast repetitions nor selecting a different window establishes clearance.

The budget was proposed here, not approved by the owner. In particular, 50 claims/s
is an admission stress workload, much higher than the turnover of 24 five-minute
runs. These numbers do not establish a lower-rate limit or a universal SQLite
limit. If the owner chooses a different budget/workload, predeclare it and run a
new experiment while preserving this result.

## Environment and limits

macOS 27.2.1, Apple M5 Max, arm64, 18 logical CPUs, 36 GiB RAM, local internal SSD
with APFS. Linux uses the same physical machine in Docker, native arm64 ISA,
Linux 7.0.14-linuxkit, an ext-family local VM volume, 2 CPU quota and 2 GiB cgroup
memory limit. It is **not a second physical host, Linux CI, or an emulated amd64
run**. Rust 1.99.1, default release optimisation, stripped binaries; SQLite 3.54.1
bundled by libsqlite3-sys 0.39.1 on both platforms, no system SQLite linkage.
Linux image: `rust:1.99.1-trixie`,
`sha256:8a4e90c7946b74626488b1446f712c0fe0ed1a596cbd3d871684bc9c5433ae07`.

The host was shared with other agents/builds. Our platform measurements and
sensitivity checks run sequentially; we did not isolate or stop other work.
This measures admitted-load behaviour on this host, not an intrinsic SQLite
service-time bound. In particular, filesystem/cache/scheduling variation remains
uncontrolled. No causal claim that events alone created every stall is justified.

The simulation models two lanes with strict high-first selection, not the
production scheduler's complete aging policy. There is no RPC, Tokio executor,
reader pool, long snapshot, backup, pruning, import or FULL-synchronous workload.
Every claim uses a new task/run/agent lease; claim state accumulates throughout
the 12 seconds and new claim runs do not themselves emit more events. Event
producers remain fixed at 1, 6 or 24. The 32 MiB history payload yields initial
DB sizes of 38,572,032–38,580,224 bytes. The large-history probe seeds 668 MiB
(700,448,768 bytes) of event payload; its physical initial DB is 802,996,224 bytes.
These are hot append workloads, not cold random reads over the historical data.

Traffic is byte-calibrated synthetic JSON, not a production trace. One nominal
producer emits 276,480 bytes over five minutes; its short 12-second sample has
only 12 events, so the exact byte mix varies with the deterministic phase.
Actual per-cell counts/bytes are in the raw results. The highest offered load is
240 events/s and 221,184 payload bytes/s (216 KiB/s), plus 50 claims/s. This is a
sustained-load test, not a measurement of maximum ingestion capacity.

## macOS matrix

Nearest-rank quantiles pool **1,800 claims per cell** (three independent 12-second
repetitions of 600). The p99 range shows the individual repetitions. DB growth is
measured after a complete passive checkpoint; WAL numbers are physical file
lengths, not uncheckpointed bytes. On every matrix run, WAL end size equals its
observed peak. Millisecond precision below reproduces samples; the spread, rather
than a precise millisecond, is the practical claim.

| Runs | Rate | Batch ms | Seed MiB / aligned | Claim p50 / p99 / max ms | Repetition p99 range ms | DB growth MiB range | WAL peak MiB max |
|---:|---:|---:|---|---|---|---|---:|
| 0 | 1x | 50 | 32 / False | 3.250 / 10.947 / 20.645 | 7.324–13.550 | 0.102–0.102 | 3.941 |
| 1 | 1x | 10 | 32 / False | 3.814 / 146.391 / 339.122 | 15.022–260.312 | 0.113–0.113 | 3.945 |
| 1 | 1x | 50 | 32 / False | 3.608 / 37.283 / 117.136 | 8.053–60.308 | 0.113–0.113 | 3.945 |
| 1 | 1x | 200 | 32 / False | 3.399 / 9.747 / 20.108 | 7.021–10.027 | 0.113–0.113 | 3.941 |
| 1 | 10x | 10 | 32 / False | 3.410 / 58.962 / 114.138 | 6.404–83.253 | 0.227–0.227 | 3.945 |
| 1 | 10x | 50 | 32 / False | 3.767 / 449.443 / 594.312 | 6.591–528.382 | 0.227–0.227 | 3.945 |
| 1 | 10x | 200 | 32 / False | 3.763 / 15.834 / 31.798 | 5.558–19.814 | 0.227–0.227 | 3.945 |
| 6 | 1x | 10 | 32 / False | 4.390 / 235.441 / 421.415 | 10.112–337.678 | 0.172–0.172 | 3.949 |
| 6 | 1x | 50 | 32 / False | 3.994 / 10.640 / 42.376 | 7.016–25.759 | 0.172–0.172 | 3.933 |
| 6 | 1x | 200 | 32 / False | 3.999 / 10.135 / 26.387 | 6.562–10.217 | 0.172–0.172 | 3.957 |
| 6 | 10x | 10 | 32 / False | 3.938 / 44.030 / 218.171 | 34.880–106.648 | 0.844–0.844 | 3.945 |
| 6 | 10x | 50 | 32 / False | 4.114 / 41.953 / 122.788 | 11.916–65.958 | 0.844–0.844 | 3.941 |
| 6 | 10x | 200 | 32 / False | 4.360 / 108.705 / 232.001 | 98.087–120.250 | 0.836–0.836 | 3.953 |
| 24 | 1x | 10 | 32 / False | 3.879 / 57.901 / 129.726 | 6.118–92.529 | 0.395–0.395 | 3.949 |
| 24 | 1x | 50 | 32 / False | 4.280 / 53.518 / 170.092 | 33.241–67.352 | 0.395–0.395 | 3.953 |
| 24 | 1x | 200 | 32 / False | 4.378 / 102.634 / 172.987 | 10.273–132.977 | 0.395–0.395 | 3.953 |
| 24 | 10x | 10 | 32 / False | 4.066 / 229.558 / 329.277 | 146.349–289.390 | 3.090–3.090 | 3.957 |
| 24 | 10x | 50 | 32 / False | 6.034 / 1912.886 / 2083.163 | 28.492–1989.291 | 3.090–3.090 | 4.051 |
| 24 | 10x | 200 | 32 / False | 5.213 / 232.067 / 463.549 | 13.348–351.808 | 3.070–3.070 | 4.075 |

## Linux matrix

| Runs | Rate | Batch ms | Seed MiB / aligned | Claim p50 / p99 / max ms | Repetition p99 range ms | DB growth MiB range | WAL peak MiB max |
|---:|---:|---:|---|---|---|---|---:|
| 0 | 1x | 50 | 32 / False | 1.690 / 8.501 / 12.314 | 5.095–9.238 | 0.102–0.102 | 3.941 |
| 1 | 1x | 10 | 32 / False | 2.297 / 9.252 / 12.590 | 5.231–9.705 | 0.113–0.113 | 3.945 |
| 1 | 1x | 50 | 32 / False | 2.228 / 9.053 / 12.276 | 5.227–9.249 | 0.113–0.113 | 3.945 |
| 1 | 1x | 200 | 32 / False | 2.457 / 9.449 / 11.780 | 5.119–10.252 | 0.113–0.113 | 3.941 |
| 1 | 10x | 10 | 32 / False | 2.285 / 9.561 / 11.516 | 5.220–9.789 | 0.227–0.227 | 3.945 |
| 1 | 10x | 50 | 32 / False | 3.210 / 9.435 / 28.041 | 5.183–9.892 | 0.227–0.227 | 3.945 |
| 1 | 10x | 200 | 32 / False | 3.235 / 9.225 / 11.821 | 5.153–9.785 | 0.227–0.227 | 3.945 |
| 6 | 1x | 10 | 32 / False | 1.711 / 8.750 / 40.244 | 4.880–9.425 | 0.172–0.172 | 3.933 |
| 6 | 1x | 50 | 32 / False | 2.539 / 9.291 / 11.445 | 5.134–9.675 | 0.172–0.172 | 3.933 |
| 6 | 1x | 200 | 32 / False | 1.409 / 8.385 / 10.764 | 5.108–9.370 | 0.172–0.172 | 3.957 |
| 6 | 10x | 10 | 32 / False | 2.209 / 8.346 / 33.050 | 5.225–9.080 | 0.844–0.844 | 3.945 |
| 6 | 10x | 50 | 32 / False | 2.831 / 8.927 / 9.927 | 5.152–9.671 | 0.844–0.844 | 3.941 |
| 6 | 10x | 200 | 32 / False | 2.376 / 8.503 / 35.296 | 5.277–9.065 | 0.836–0.836 | 3.953 |
| 24 | 1x | 10 | 32 / False | 2.063 / 8.011 / 12.686 | 5.235–9.484 | 0.395–0.395 | 3.949 |
| 24 | 1x | 50 | 32 / False | 2.966 / 9.461 / 36.880 | 5.235–9.713 | 0.395–0.395 | 3.953 |
| 24 | 1x | 200 | 32 / False | 2.440 / 9.121 / 13.802 | 5.078–9.646 | 0.395–0.395 | 3.953 |
| 24 | 10x | 10 | 32 / False | 2.527 / 9.270 / 13.658 | 5.126–9.463 | 3.090–3.090 | 3.961 |
| 24 | 10x | 50 | 32 / False | 2.243 / 8.314 / 13.969 | 5.350–8.933 | 3.090–3.090 | 4.051 |
| 24 | 10x | 200 | 32 / False | 2.263 / 9.440 / 16.757 | 5.348–9.808 | 3.070–3.070 | 4.240 |

## Sensitivity checks and timing oracle

All use 24 producers, tenfold events and a 50 ms window, three repetitions and
1,800 claims per cell. The large-history check changes only seed size; the
aligned check changes only producer phases.

macos aarch64 SQLite 3.54.1
| Runs | Rate | Batch ms | Seed MiB / aligned | Claim p50 / p99 / max ms | Repetition p99 range ms | DB growth MiB range | WAL peak MiB max |
|---:|---:|---:|---|---|---|---|---:|
| 24 | 10x | 50 | 668 / False | 3.494 / 11.874 / 34.083 | 5.277–16.920 | 3.078–3.078 | 4.035 |

macos aarch64 SQLite 3.54.1
| Runs | Rate | Batch ms | Seed MiB / aligned | Claim p50 / p99 / max ms | Repetition p99 range ms | DB growth MiB range | WAL peak MiB max |
|---:|---:|---:|---|---|---|---|---:|
| 24 | 10x | 50 | 32 / True | 3.423 / 133.957 / 250.453 | 38.066–190.931 | 3.090–3.090 | 4.145 |

linux aarch64 SQLite 3.54.1
| Runs | Rate | Batch ms | Seed MiB / aligned | Claim p50 / p99 / max ms | Repetition p99 range ms | DB growth MiB range | WAL peak MiB max |
|---:|---:|---:|---|---|---|---|---:|
| 24 | 10x | 50 | 668 / False | 3.768 / 10.045 / 17.542 | 9.872–10.144 | 3.078–3.078 | 4.035 |

linux aarch64 SQLite 3.54.1
| Runs | Rate | Batch ms | Seed MiB / aligned | Claim p50 / p99 / max ms | Repetition p99 range ms | DB growth MiB range | WAL peak MiB max |
|---:|---:|---:|---|---|---|---|---:|
| 24 | 10x | 50 | 32 / True | 2.233 / 9.454 / 13.648 | 9.369–9.708 | 3.090–3.090 | 4.145 |

The separate negative control deliberately held the writer for 250 ms after a
50 ms start delay. With 100 scheduled claims it produced p99 **292.575 ms**, max
**312.444 ms**, while preserving every row. This confirms that scheduled-arrival
latency exposes backlog, including requests generated while the writer is busy.
It is excluded from all performance tables and verdict calculations.

## Findings, in cost order

1. **Tail latency and accumulated queueing fail the budget.** In the worst macOS
   matrix repetition, claim transactions themselves reached 498.867 ms and claim
   producer lateness was at most 9.900 ms. With a 2,083.163 ms worst claim, at least
   1,574.396 ms lay outside its producer lateness and own transaction, even using
   those conservative maxima. See the priority selection in
   [the writer](experiments/sqlite-events/src/main.rs#L185) and
   [claim commit](experiments/sqlite-events/src/main.rs#L164).
   This is a measured queue/transaction problem, not merely percentile averaging.
2. **A row/time slice is cooperative, not a preemption bound.** That repetition's
   event transactions reached 592.361 ms with at most 12 rows per slice. The 20 ms
   [check](experiments/sqlite-events/src/main.rs#L218) occurs after an insert; an already-running statement or commit can take
   longer. The source times BEGIN through COMMIT; it does not separately profile
   fsync/checkpoint versus other calls. The specific syscall cause remains
   unproven. Increasing row-count limits or tuning away a few allocations is not
   a supported remedy for this tail.
3. **No unbounded WAL or memory growth was observed in this bounded workload.**
   All matrix checkpoints fully progressed, all generated events and expected
   claims were stored, and every quick_check returned `ok`. The WAL retained a
   roughly 4 MiB reusable file under no-reader conditions. A pinned reader could
   change that; these tests do not measure it.
4. **Whole-prompt deduplication saves no content in the measured corpus.** Hashing
   shared-looking prefixes is not equivalent to hashing the whole rendered text.
   Task row sizing also gives no basis to assume SQL rows are always smaller than
   individual YAML files; see the measured sizes below.

## Task/YAML and rendered-prompt sizing

An external temporary copy contained **3,012 task YAML files** and **2,547 run JSON
files**. No copied file changed size or mtime during its copy; this is not an
atomic project snapshot. Only aggregate counts/bytes are published. The private
copy and sizing DB were erased after analysis; no prompt/hash/entity data enters
these artifacts.

| Quantity | Measured bytes |
|---|---:|
| Task YAML file content, total | 31,923,571 |
| Task files allocated in the external copy | 37,826,560 |
| Compact JSON representation of the same tasks | 31,920,820 |
| SQLite growth for the task rows **and queue index** | 36,368,384 |
| Rendered prompts inline, total UTF-8 content | 164,151,665 |
| Distinct whole prompts / all prompts | 2,547 / 2,547 |
| Distinct prompt content bytes | 164,151,665 |
| Duplicate prompt content bytes saved | **0** |
| Modeled content + binary object keys + run hash references | 164,314,673 |

The task proxy averages **12,074.5 bytes/row**, against **10,598.8
bytes/YAML file**: **13.92% larger** than YAML content, or **3.85%
smaller** than copied-file allocation. It uses provisional typed hot columns,
UTC-ms timestamps, synthetic keys and remaining fields as JSON. It retains all
text, including text that the final 64 KiB rule may place in blobs. Empty schema
allocation is excluded. This is not a forecast of the final schema/blob layout,
and excludes the legacy whole-entity `_index.json` cache.

A separate, later external copy of that cache contained 3,019 entries
and 32,390,644 bytes; it did not change during copy. It is not the same
atomic snapshot as the YAML sample. The current implementation caches full
entities ([IndexManager](../crates/orch-infra/src/storage/index.rs#L3)), so a
comparison of whole legacy storage must include that duplication as well as YAML.
Do not interpret the individual-file comparison as total old-versus-new storage.

A 32-byte binary object key plus a 32-byte hash reference per run adds **163,008
bytes (0.099%)** to the logical prompt content here, before table/index/filesystem
overhead. There is no exact whole-prompt duplication to recover those bytes.
383 prompts exceed 65,536 UTF-8 bytes and contain 90,325,383 bytes in aggregate;
the goal's existing blob threshold applies to those regardless of deduplication.
This does not revoke the goal's content-addressed prompt design; it removes an
unsupported storage-savings assumption for this sample.

## Recommendations and what is not worth fixing

- Carry the failed budget and fallback requirement to the owner gate and existing
  event-storage implementation task. Keep payloads behind the blob adapter with
  one SQL index row per batch if this budget remains. No production code or
  fallback implementation is included here. No claim-latency improvement for that
  alternative has been measured.
- Reuse this workload against the actual writer/fallback before claiming a latency
  improvement: same machine/build, rates, windows and three repetitions; target
  p99 <=50 ms and max <=100 ms. Profile the slow claim/event transactions and
  compare with claim-only controls. A fallback must not be called a performance
  fix solely because it removes event rows.
- Do not prioritize whole-prompt deduplication as a space optimisation: measured
  duplicate content is zero. Do not spend this slice replacing small allocations
  or increasing row caps: the worst default-window event slice held only 12 rows.
- Do not infer that bigger history alone caused the observed tails: the macOS
  803 MB DB probe's p99 was 11.874 ms, against 1,912.886 ms in the smaller-history
  default cell. That is sensitivity evidence, not a claim that history size never
  matters. Changing only the batch window did not clear every Mac workload.

The existing event implementation task already carries the conditional fallback
requirement; this result is its implementation handoff. A separate dependent
performance qualification task reuses this workload against the actual writer.
Product changes remain behind the goal's approval checkpoint. PR/repository-gate
status is recorded in the
commit-stamped handoff, separately from the measurement verdict.

## Harnesses and verification

[Sources, lockfile and exact commands](experiments/sqlite-events/README.md) stay
outside the product workspace. The JSONL retains all claim samples and per-run
transaction percentiles, counts, DB sizes, WAL sizes and checkpoint progress.
`measure.py` records wall time and OS process peak RSS; it stops at 30 minutes or
sampled RSS above 512 MiB, with the additional Linux 2 GiB cgroup cap.
`summarize.py` checks count conservation, complete matrix/repetition inventory,
quantiles and the budget. Exit 1 for a real budget
failure is the expected result, not a failed data-integrity audit.

- [Raw macOS matrix](experiments/sqlite-events/results/macos-matrix.jsonl), [Linux matrix](experiments/sqlite-events/results/linux-matrix.jsonl).
- [All case/count/budget audits](experiments/sqlite-events/results/validation.json), [resource measurements](experiments/sqlite-events/results/resources.json).
- [Sizing aggregates](experiments/sqlite-events/results/corpus-sizing.jsonl), [copy metadata](experiments/sqlite-events/results/corpus-copy-aggregates.json), [later legacy cache copy](experiments/sqlite-events/results/legacy-index-sizing.json).
- [Timing negative control](experiments/sqlite-events/results/negative-control.jsonl), [source digests](experiments/sqlite-events/results/source-sha256.json).
- Per-repetition sensitivity JSONL and generated tables are in the same results directory.

| Platform / workload | Wall seconds | Process peak RSS MiB |
|---|---:|---:|
| macos-matrix | 857.261 | 10.53 |
| macos-history | 101.181 | 10.59 |
| macos-aligned | 39.046 | 10.47 |
| linux-matrix | 725.262 | 10.42 |
| linux-history | 82.965 | 10.74 |
| linux-aligned | 37.582 | 10.87 |

Standalone release builds, fmt, clippy `-D warnings` and dependency audit commands
are listed in the reproduction instructions. Repository gates are separate from
these measurements and cannot turn a failed latency budget into a pass.
