# PR2 selector implementation addendum
status: active

Implements the approved [tiered-checks plan](2026-11-14-tiered-checks.md), section 5,
and [ADR 0014](../decisions/0014-gate-target-recovery-and-selection-coupling.md)
D39 on the PR1 merge base. This bounded task replaces the conservative selector
seam; gate dispatch, lifecycle, fast runner, CI and policy belong to integration.

| Work | Owned files | Verification |
|---|---|---|
| Recover bounded global-first classification, Git NUL collection, metadata reverse edges and stable schema-1 rendering from frozen G | `xtask/src/gate_selection.rs`, `xtask/src/gate_selection/**` | Inline selector/parser/process tests; independent H black-box tables |
| Recover independent TV20–TV38 tables from frozen H | `xtask/tests/gate_selection.rs` | Run against actual selector; retain every failed command |
| D39: apply CLI→infra coupling after complete normal closure; add no reverse closure from extra edge; no coupled reason if infra already selected | selector module and owned test file | New indirect CLI-only upstream/sentinel and mixed normal-infra cases; old narrower G must fail the indirect discriminator |
| I2: exercise Cargo's actual workspace path dependency metadata | owned test file | Dependency-free real-Cargo normal/build/dev/renamed/optional/target-specific path edges; same parser used by real repository smoke |
| Fail closed and preserve output contract | owned selector and test file | Error/malformed/hostile bytes, Git argv, rename/delete/untracked/type changes, missing base/metadata, JSON/GitHub/text |

No new dependency, timeout relaxation or ignored test. Tests run serially with a
private HOME/config, offline pinned toolchain and a unique target. Broad package,
workspace and native platform acceptance are later frozen-phase obligations.
D40 remains the integrator's gate-controlled empty/changed-marker build-order
probe on Mac/native Linux plus isolated real concurrent_index. Historical H's
hand-written cargo probe cannot close D40. Real fast shared requesters TV61/TV62
and RS1, warm-target exclusion and full PR/native/timing evidence remain due.
