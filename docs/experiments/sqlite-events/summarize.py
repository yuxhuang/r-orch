#!/usr/bin/env python3
"""Verify count/budget evidence and print portable Markdown tables from JSONL.

Usage: python3 summarize.py RESULTS_JSONL [RESULTS_JSONL ...]
Exit nonzero for corruption, incomplete three-repetition cells or budget failure.
Negative controls are checked for budget *failure*, never pooled with measurements.
"""
import json
import math
import sys
from collections import defaultdict


def pct(values, p):
    values = sorted(values)
    return values[math.ceil(len(values) * p / 100) - 1] / 1000


failed = False
for source in sys.argv[1:]:
    with open(source) as handle:
        rows = [json.loads(line) for line in handle]
    assert len(rows) > 1, "empty result set"
    env = rows.pop(0)
    assert env["kind"] == "environment" and env["release_build"]
    print(f"\n{env['os']} {env['arch']} SQLite {env['sqlite']}")
    groups = defaultdict(list)
    for row in rows:
        if "claim" not in row:
            print(json.dumps(row, sort_keys=True))
            continue
        assert row["counts_match"] and row["quick_check"] == "ok"
        assert row["events"] == row["runs"] * row["rate_multiplier"] * row["seconds"]
        assert len(row["claim_samples_us"]) == row["claims_per_second"] * row["seconds"]
        assert row["max_slice_rows"] <= 1000
        checkpoint = row["checkpoint"]
        assert checkpoint["busy"] == 0 and checkpoint["frames"] == checkpoint["checkpointed"]
        assert round(pct(row["claim_samples_us"], 99) * 1000) == row["claim"]["p99_us"]
        budget = row["claim"]["p99_us"] <= 50000 and row["claim"]["max_us"] <= 100000
        if row["negative_control_stall_ms"]:
            assert not budget, "negative control must break the budget"
            print(f"Negative control: {row['claim']}")
            continue
        failed |= not budget
        groups[(row["runs"], row["rate_multiplier"], row["window_ms"], row["seed_payload_mib"], row["aligned"])].append(row)
    if any(key[0] == 0 for key in groups):
        seeds = {key[3] for key in groups}
        assert len(seeds) == 1
        seed = next(iter(seeds))
        expected = {(0, 1, 50, seed, False)} | {
            (runs, rate, window, seed, False)
            for runs in [1, 6, 24] for rate in [1, 10] for window in [10, 50, 200]
        }
        assert set(groups) == expected, "incomplete matrix"
    if groups:
        print("| Runs | Rate | Batch ms | Seed MiB / aligned | Claim p50 / p99 / max ms | Repetition p99 range ms | DB growth MiB range | WAL peak MiB max |")
        print("|---:|---:|---:|---|---|---|---|---:|")
    for (runs, rate, window, seed, aligned), group in sorted(groups.items()):
        assert {r["repeat"] for r in group} == {0, 1, 2}, "need exactly three repetitions per cell"
        assert len(group) == 3
        samples = [x for r in group for x in r["claim_samples_us"]]
        p99s = [r["claim"]["p99_us"] / 1000 for r in group]
        growth = [r["db_growth_bytes"] / 1048576 for r in group]
        wal = max(r["wal_peak_bytes"] for r in group) / 1048576
        print(f"| {runs} | {rate}x | {window} | {seed} / {aligned} | {pct(samples,50):.3f} / {pct(samples,99):.3f} / {max(samples)/1000:.3f} | {min(p99s):.3f}–{max(p99s):.3f} | {min(growth):.3f}–{max(growth):.3f} | {wal:.3f} |")
raise SystemExit(1 if failed else 0)
