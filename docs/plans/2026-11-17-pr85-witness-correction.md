# PR85 witness ownership correction
status: active

Implements W1–W3 in the tiered-checks spec under the existing owner correction
loop. Parent is the frozen PR85 head. No push or whole-workspace gate in this task.

Hermetic commands explicitly default a fixed witness-writer control to `unit`,
overriding inherited state. Only the separate fast witness command overrides it
to `standalone`. Export environment data lists its name, never its value. The
real unit test uses the existing exclusive private write API. The std-only helper
writes for explicitly designated standalone calls, or when planted in a non-xtask
fixture package; real xtask helper copies otherwise remain non-writing. Keep the
existing record grammar and verifier. Missing data cannot be repaired by a helper.

Reject swallowing AlreadyExists, truncation, per-helper last-writer-wins and
removing tests: each can hide a missing or bad actual suite witness. The existing
names-only helper test explicitly designates its isolated child writer; its
assertion remains unchanged. Update the synthetic environment-name fixture and the explicit gate-control
name assertion for the added fixed control; keep the forbidden-prefix checks.

| Task | Requirements | Scope | Depends | Verify | PR |
|---|---|---|---|---|---|
| Correct ownership and prove old-fails/new-passes | W1–W3 | `xtask/src/gate/{hermetic,suite}.rs`, `xtask/tests/gate_{witness,evidence,protections}.rs`, correction docs, changelog | frozen PR85 head | focused ownership regression; `cargo test -p xtask --test gate_witness`; `cargo test -p xtask --test gate_fast`; fmt, affected check/clippy | 85 delta |

Use private HOME/config/resource-only toolchain and blockers under workload022;
explicit0600 raw captures/immediate0700. Acquire existing host admission without
stealing/unlinking. Retain all failures. Bound child commands, finish and reap
owned children before assertions/cleanup. Run the new regression against original
production source (plus only the regression), then corrected source. No native
acceptance, broad receipt campaign, dependency/version/rule/CI changes. Independent
original reviewers and lead consume the immutable corrected commit before any
publication or corrected-head acceptance. Full goal acceptance remains pending.
