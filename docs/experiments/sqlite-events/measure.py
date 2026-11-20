#!/usr/bin/env python3
"""Throwaway experiment driver: deadline, sampled RSS cap and OS peak RSS.

Usage: python3 measure.py METRICS_JSON STDOUT_LOG BINARY [BINARY_ARGS...]
Uses wait4 for this binary's own high-water RSS, excluding sampler children.
This is not a wrapper around repository gates or test tooling.
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path


def rss(pid):
    if sys.platform == "linux":
        try:
            for line in Path(f"/proc/{pid}/status").read_text().splitlines():
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) * 1024
        except FileNotFoundError:
            return 0
    else:
        result = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)],
                                capture_output=True, text=True)
        return int(result.stdout.strip() or "0") * 1024
    return 0


if len(sys.argv) < 4:
    raise SystemExit(__doc__)
metrics, log = map(Path, sys.argv[1:3])
if metrics.exists() or log.exists():
    raise SystemExit("refusing existing output")
start = time.monotonic()
sampled_peak = 0
reason = None
with log.open("x") as output:
    child = subprocess.Popen(sys.argv[3:], stdout=output, stderr=subprocess.STDOUT)
    while True:
        pid, status, usage = os.wait4(child.pid, os.WNOHANG)
        if pid:
            child.returncode = os.waitstatus_to_exitcode(status)
            break
        sampled_peak = max(sampled_peak, rss(child.pid))
        if sampled_peak > 512 * 1024 * 1024:
            reason = "sampled RSS exceeded 512 MiB"
        if time.monotonic() - start > 1800:
            reason = "deadline exceeded"
        if reason:
            child.kill()
            _, status, usage = os.wait4(child.pid, 0)
            child.returncode = os.waitstatus_to_exitcode(status)
            break
        time.sleep(0.5)
peak = usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024)
metrics.write_text(json.dumps({"exit_code": child.returncode, "wall_seconds": time.monotonic()-start,
                              "peak_rss_bytes": peak, "sampled_peak_rss_bytes": sampled_peak,
                              "sampled_rss_limit_bytes": 512*1024*1024,
                              "rss_sample_seconds": 0.5, "deadline_seconds": 1800,
                              "stopped_reason": reason}, indent=2)+"\n")
raise SystemExit(child.returncode)
