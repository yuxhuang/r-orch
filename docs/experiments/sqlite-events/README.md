# Throwaway event/claim experiment

Entry step D; outside the Cargo workspace and all product crates. Original project
code, synthetic traffic only. The [report](../../sqlite-event-throughput.md) gives
the predeclared budget, historical measurements, assumptions and decision boundary.
No migration or application storage is changed; no product data directory is opened.
Dependencies use the library recommendation; the standalone lockfile is intentional.

## Reproduce on macOS

From the repository root with its pinned Rust toolchain, Python 3 and local disk.
Use a private umask for all scratch/results, particularly optional corpus sizing:

```sh
umask 077
```

```sh
cargo build --release --locked --manifest-path docs/experiments/sqlite-events/Cargo.toml
cargo fmt --check --manifest-path docs/experiments/sqlite-events/Cargo.toml
cargo clippy --release --locked --manifest-path docs/experiments/sqlite-events/Cargo.toml -- -D warnings
cargo deny --manifest-path docs/experiments/sqlite-events/Cargo.toml --config deny.toml check
```

Choose a new external directory for the outputs; `--scratch` must not exist. The
binary refuses existing databases/results and removes only its own successful
scratch database. It uses no orch daemon or AI CLI. Failed/interrupted experiments
may leave their scratch directory for inspection; remove it yourself afterwards.
Do not use a product directory for any path below.

```sh
event_outputs=$(mktemp -d)
event_bench=docs/experiments/sqlite-events/target/release/sqlite-events-bench
python3 docs/experiments/sqlite-events/measure.py \
  "$event_outputs/matrix-metrics.json" "$event_outputs/matrix.log" \
  "$event_bench" matrix --scratch "$event_outputs/matrix-data" \
  --output "$event_outputs/matrix.jsonl"
python3 docs/experiments/sqlite-events/measure.py \
  "$event_outputs/history-metrics.json" "$event_outputs/history.log" \
  "$event_bench" case --seed-mib 668 --runs 24 --rate 10 --window-ms 50 \
  --scratch "$event_outputs/history-data" --output "$event_outputs/history.jsonl"
python3 docs/experiments/sqlite-events/measure.py \
  "$event_outputs/aligned-metrics.json" "$event_outputs/aligned.log" \
  "$event_bench" case --aligned --runs 24 --rate 10 --window-ms 50 \
  --scratch "$event_outputs/aligned-data" --output "$event_outputs/aligned.jsonl"
python3 docs/experiments/sqlite-events/summarize.py \
  "$event_outputs/matrix.jsonl" "$event_outputs/history.jsonl" "$event_outputs/aligned.jsonl"
```

Default: 12 seconds/cell, three repetitions, 50 claims/s, 32 MiB history payload.
The matrix contains 18 event cells plus a claim-only control per repetition,
alternating forward/reverse cell order. About 12 minutes per platform, excluding
compilation and optional sensitivity checks. Every sample is scheduled-arrival to
commit; the raw microsecond arrays are retained in JSONL for pooling and audit.
All quantiles use nearest rank; do not average percentile values.

`measure.py` stops a binary at 30 minutes or sampled RSS above 512 MiB (0.5-second
sampling, so this is not a hard memory isolation boundary). It records OS high-water
RSS using wait4 for the benchmark process. Linux also uses a 2 GiB Docker cgroup.
The binary caps runs at 24, rate multiplier at 10, seed at 1 GiB, duration at 60 s,
and repetitions at five. Bounded channels: 512 claims and eight event batches.
A receiver preserves remaining event rows between transaction slices. No separate
reader, pruning, import or backup traffic is simulated.

Prove that queue delay is visible with the negative control (should violate budget):

```sh
"$event_bench" case --seconds 2 --repeats 1 --seed-mib 1 --stall-ms 250 \
  --scratch "$event_outputs/negative-data" --output "$event_outputs/negative.jsonl"
python3 docs/experiments/sqlite-events/summarize.py "$event_outputs/negative.jsonl"
```

The 250 ms stall blocks the writer immediately before processing the first queued
claims. It is disabled in every measured cell. This checks the timing oracle,
not the realism of a slow-disk model. Count conservation, quick_check and full
passive-checkpoint progress must succeed on every run; errors abort the binary.

## Linux (Docker linux/arm64, native ISA)

Use the same source/lock and Rust image as recorded in the report. Give this
experiment its own build directory and data volume, not another worktree's target.
The source and cargo registry mounts are read-only. Build offline after host fetch.
Use a **Docker data volume** for scratch, not a shared host filesystem; the latter
would measure the file-sharing implementation. Export only synthetic results.
For example, after creating `event_outputs` as above, create an isolated build
and volume (the volume name contains only a public task label and timestamp):

```sh
event_linux_build=$(mktemp -d)
event_linux_volume="sqlite-events-bench-$(date +%s)"
docker volume create "$event_linux_volume"
```

Then build and run:

```sh
docker run --rm --platform linux/arm64 --memory 2g --cpus 2 --network none \
  -v "$PWD/docs/experiments/sqlite-events:/source:ro" \
  -v "$HOME/.cargo/registry:/usr/local/cargo/registry:ro" \
  -v "$event_linux_build:/build" -e CARGO_TARGET_DIR=/build/target \
  rust:1.99.1-trixie cargo build --release --offline --locked --manifest-path /source/Cargo.toml

docker run --rm --platform linux/arm64 --memory 2g --cpus 2 --network none \
  -v "$PWD/docs/experiments/sqlite-events:/source:ro" \
  -v "$event_linux_build:/build:ro" -v "$event_outputs:/results" \
  -v "$event_linux_volume:/data" rust:1.99.1-trixie \
  python3 /source/measure.py /results/linux-metrics.json /results/linux.log \
  /build/target/release/sqlite-events-bench matrix \
  --scratch /data/matrix-data --output /results/linux-matrix.jsonl
```

Run the two `case` sensitivity checks analogously. Stop and remove only this
experiment's containers/volume after results have been copied. This is a Linux VM
measurement on the same physical Mac, not native-Linux hardware or Linux CI.

## Private corpus sizing

Optional `sizing --copy COPY_DIR --scratch NEW_DIR --output NEW_JSONL` reads only a
previously copied directory with `tasks/*.yaml` or `*.yml` and `runs/*.json`.
Copy outside the repository first; never pass the live directory. Do not share the
copy or its sizing database. Inputs are capped at 4 MiB/task and 8 MiB/run. This
mode expects trusted local-copy data, not arbitrary importer input. It emits only
aggregate counts/bytes, never identifiers, prompt content or hashes.

Task sizing uses the experiment's provisional typed columns, remaining fields as
JSON, and a queue index. It measures DB growth after checkpoint, excluding empty
schema allocation, not a final production row layout. Timestamps become UTC ms.
Prompt savings count **exact complete rendered prompt bytes** by SHA-256. The hash
model charges a 32-byte binary key per unique object and a 32-byte reference per
run; it excludes filesystem/table/index overhead and is not physical blob sizing.

Keep only aggregate sizing JSON in the repository; erase the external copy and
sizing database afterwards. A new copy can differ from the historical snapshot.
