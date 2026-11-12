# 0012. One explicit project daemon owns state

- Status: accepted
- Date: 2026-11-09
- Revision: 2, after round-1 challenge; accepted by the owner at gate 2, 2026-11-09. Decision 14 added in PR 4, 2026-11-11
- Supersedes: ADR [0011](0011-mcp-server.md) decision 7 and the D10 concurrent-writer acceptance, when the client conversion is complete. Other MCP decisions remain except where the implementation is explicitly replaced below.

## Context

The owner chose daemon-owned state instead of accepting multiple independent file-store writers
([spec](../specs/2026-11-09-daemon-owned-state.md), [plan](../plans/2026-11-09-daemon-owned-state.md)). The
owner amended the request on 2026-11-09: clients never start a daemon, and never fall back to direct stores.
The owner approved the requirements and accepted this ADR at gate 2 on 2026-11-09.

Today `build_light` creates file stores for one-shot commands and each MCP call. The full engine separately
owns dispatch and live processes. The PID lock protects orchestrators, not all writers. Existing store locks
serialize index updates but not every complete read/modify/write across services. Context reads can purge
expired entries. The MCP FIFO lane protects one MCP process only. Entity and index writes remain separate. A
socket alone would merely move those races into a single process: engine callbacks and RPC services must
also share an operation boundary.

Round 1 found mixed ownership in the old PR split (RT1), an unusable skew-stop instruction (RT2/SE-8),
indistinguishable development builds (RT3), incomplete workflow recovery (RT4), conflicting batch/global
scheduling intent (RT5), and an unstated single-host boundary (RT6). The lead accepted these and the
security conditions. This revision incorporates those rulings; the owner confirmed Q2=stop-intent and
Q5=regrouped at gate 2 on 2026-11-09. That approval permits implementation under the plan; it does not
authorize client recovery side effects, release or merge.

## Decision

1. **One daemon per canonical project on one host owns `.orchestry` and agent processes.** It holds project
   ownership through startup reconciliation, dispatch ON or OFF, and shutdown. All CLI, TUI, web, MCP and
   agent-invoked state operations use a local socket. No direct-store fallback exists, including reads.
   `init` uses a narrow bootstrap entry point to create the project and prints the explicit-start command
   without starting it. Pure help/version rendering does not require state access. Discovery metadata is the
   sole client read exception under `.orchestry`. Compare project identity as st_dev/st_ino from O_DIRECTORY
   + O_NOFOLLOW fstat of `.orchestry`, not canonical path text. ORCH_SOCKET selects the endpoint,
   ORCH_PROJECT_ROOT selects the expected project ahead of cwd ancestor discovery; overrides cannot bypass
   identity. Distinct initialized checkouts are distinct projects. Simultaneous multi-host/shared-volume
   state is unsupported and untested; use separate local projects and TCP peers. Single-host network storage
   is unverified and requires working advisory locks, exclusive create, atomic rename and stable inode
   identity; unsupported locks fail closed.

2. **NO AUTO-START is an owner decision.** The reason is: **the owner wants to know when a long-lived
   process starts on their machine**. `orch daemon start` is explicit and foreground by default; `--detach`
   is explicit convenience. `orch serve` and `orch run --watch` are explicit foreground starts with dispatch
   ON. No client connection, retry, MCP initialize, TUI/web launch, error path or bootstrap command starts
   the daemon. Clients never remove socket/discovery files. There is no idle exit, watchdog or automatic
   process replacement. Agent-context start/serve/run --watch refuses exit 2 without explicit --from-agent;
   its NoDaemon text says "Do not start it; report this to the owner and stop." This is accident prevention,
   not a same-uid sandbox.

3. **Fail fast with a single NoDaemon contract and exit 7.** Existing exit codes 1–6 retain their meanings.
   The message names the canonical project and `orch daemon start`, with a safe detail for
   absent/stale/refused socket, handshake failure or version skew. TUI presents the same text and
   retry/quit; web exits 7 before startup or returns HTTP 503 after a live outage. MCP returns the same
   message in initialize/tool errors with structured exit_code 7, remains a stdio bridge and exits 0 on EOF.
   The protocol code is not confused with the MCP process exit status. No absence case grants store access.
   Session exhaustion instead returns daemon_busy, exit 7 without a start hint, using a separate
   pre-handshake budget. An unauthenticated web 503 never exposes project/socket paths. The spec's normative
   recovery table defines first-failed MCP initialization, scripts, agent failure, terminal closure and
   unknown mutation outcomes.

4. **Three crate boundaries enforce ownership.** `orch-proto` has domain types plus serde/Tokio framing;
   `orch-client` depends only on domain/proto among workspace crates and contains no process creation or
   store access; `orch-daemon` composes engine, infra, peer and proto. The CLI depends on daemon only for
   the allowlisted initialization and explicit-start wrappers. The whole-edge crate-graph check and
   bidirectional source-site allowlist distinguish production src/ from tests/: whole-edge checking includes
   declared dev/build edges, while tests/ may use the daemon testing API and the source-site rule scans src/
   only. TUI remains domain-only. The CLI's desktop editor, clipboard and web PTY concerns remain frontend
   effects, while all agent execution belongs to the daemon. No daemon API re-exports stores or arbitrary
   execution closures to clients. The limits of the automatic check are in decision 16.

5. **Reuse orch-peer's bounded NDJSON framing, not its protocol enum.** Lift generic reader/serializer code
   into orch-proto; retain the 1 MiB line bound. Local messages are typed hello/welcome/reject, req, ok/err,
   part and ev frames. The spec's protocol appendix records the catalogue, operation mapping, sequencing,
   bounded admission and chunk handling. Peer-specific oversize event/outcome policies stay in orch-peer.
   Its TCP Frame, PROTOCOL=1, optional version fields and mixed 0.5.x/current compatibility remain
   unchanged. Local and peer versions are independent; no strict local binary equality rule leaks into TCP
   links.

6. **Freeze a small handshake envelope and allow one version-invariant stop intent.** hello/welcome/reject
   tolerate unknown fields, keep known field types/semantics stable and are bounded to 16 KiB/2 s. Kernel
   same-euid is checked in both directions (client before bytes); daemon checks project dev/ino before any
   control. Ordinary requests require equal protocol u32 and exact package semver in either direction; no
   operation payload is decoded after mismatch. No downgrade negotiation exists.

   `hello.intent: "stop"` is the sole exception, after those uid/project checks. It selects normal clean
   stop, not an entity-edit operation; shutdown still cancels/reconciles live agents. Frozen welcome
   stop_state accepted/completed and reject stop_failed distinguish admission from completed cleanup without
   decoding a version-specific response. EOF after accepted is unknown, never success or permission to
   replay/start. The existing hint "orch daemon stop, then orch daemon start" now works with either-version
   executable, even when the old installed executable has been removed (RT2/G1). SE-8's reasoning: same-uid
   callers already have SIGTERM authority; the socket avoids asking an owner to select a potentially reused
   PID by hand. For a hung daemon only, the RUNBOOK retains independently verified owner SIGTERM, never an
   automated fallback from a stale metadata PID. The owner approved this Q2 refinement at gate 2 on 2026-11-09.

   Developer contract (RT3): stop before switching builds; start the intended absolute executable. Discovery
   and live status include current_exe() captured at start and started_at. Any wire or operation-semantic
   change within the stack bumps LOCAL_PROTOCOL (PR 1 starts at 1); the package version stays unchanged.
   Same semver + same LOCAL_PROTOCOL + different build is deliberately undetectable. Provenance is not a
   build fingerprint; build.rs/git hash plumbing is out of scope. Client-resolved identity is fixed per
   session, attribution only; kernel pid is used for logs where available, never a claimed pid.

7. **Dispatch is a mode inside the owner.** Fresh `daemon start` defaults OFF; explicit `--dispatch on` opts
   in. `daemon dispatch on|off` changes future scheduling via socket without dropping ownership or killing
   current agents. `run --once` requests one tick; `run <id>` launches and observes that task via socket;
   `run --all` remains a finite batch operation. `serve --once` requires global OFF (ON rejects explicitly),
   runs batch ticks without autonomous seeding and waits for all tracked non-terminal tasks, including
   new/dependent work. One batch at a time; later explicit dispatch ON or OFF ends the batch, and neither
   completion nor disconnect restores a stale mode. Disconnect ends batch scheduling while agents continue,
   unlike the old unconditional stop. The batch never changes the global switch. This preserves
   until-terminal result semantics, distinct from one tick (G3/RT5). TUI watch controls toggle dispatch, not
   process lifecycle. Expiry and peer handling remain active with dispatch OFF. Start/status and TUI/web
   show dispatch state; restart defaults OFF. tui --observe/--listen are removed; peer listening is a daemon
   start option. Doctor is a client, with discovery-only NoDaemon diagnosis (decision 14); help/version remain offline.

8. **One shared state-operation boundary closes in-process races.** Services, dispatch/spawn decisions,
   collectors, peer persistence and expiry serialize entire read/validate/modify/save transitions. Long
   agent execution, remote/network waits and human editors run outside that boundary with revalidation on
   commit. Pause orders the pending-task cancellation decision before any later launch; live task cancel
   reaches the process registry and process manager; context expiry is an explicit daemon step, never a
   client-read side effect. Config changes refresh subsequent service/dispatch defaults without replacing
   the live registry. Full-record goal edits must serialize read through save with pause/dispatch: B0 F2
   reproduced an edit restoring Active after pause, so R12 adds a barrier test preserving Paused plus the
   new title. Mutating responses expose instance-scoped commit_seq (T0's write_seq), without persisted
   revisions. Explicit maintenance.expire_context uses the same expiry handler; expiry errors must not skip
   reconciliation or feed the fatal tick breaker (B0 F1 fix is separately reviewed). Existing
   services/storage traits remain the base; this adds coordination, not a second implementation of their
   business rules.

9. **Explicit ownership precedes stale-file replacement.** Default socket is `.orchestry/run/orch.sock`;
   absolute paths over 100 bytes use `$TMPDIR/orch-<sha256(canonical root)[..16]>/orch.sock`. Validate the
   final path including /run; ORCH_SOCKET is an absolute override, never a different authority boundary.
   Bind the socket inside the daemon-created 0700 run/fallback directory (or an existing directory
   verified as euid-only and never chmodded), set it to 0600 and re-check socket type and mode before
   publishing daemon.json; the actual parent of every endpoint, including ORCH_SOCKET, must pass the
   same directory checks. Do not chmod `.orchestry`; it
   must be euid-owned and not group/world-writable. Foreign-euid start refuses, including root on a
   user-owned project. Create fallback with mkdir, then O_DIRECTORY/O_NOFOLLOW + fstat on existing paths;
   never chmod/chown an existing unsafe directory. Discovery open uses O_NOFOLLOW/O_NONBLOCK then fstat and
   16 KiB bound, preventing FIFO hangs and symlink redirection. Metadata is 0600 and records project
   identity, endpoint, versions, pid, executable, started_at, instance; runtime files are gitignored.

   **Recorded deviation from gate-2-approved R2 wording (fact F2, lead option A):** this adopts
   SE-3's in-directory bind alternative, confirmed security-equivalent by the security engineer
   (`VERDICT: pass`); only the mechanism changes, with the outcome and acceptance tests unchanged.
   The owner may reverse this decision to option B: bind before runtime under a temporarily restricted
   creation mask, then restore it.

   Why: the process umask is never changed because setting umask 0177 before runtime makes mkdir(0700) yield untraversable 0600 directories (run/, workspaces and logs unusable; bind fails EACCES), while bind runs on the Tokio runtime (crates/orch-daemon/src/lifecycle.rs:455), where a process-wide change races other threads; the private 0700 parent closes the bind-to-chmod window measured by SE-3 (confirmed by the fact F2 security probes), and same-uid remains the trust boundary (R8/R38).

   Hold stable `.orchestry/.daemon-owner.lock` advisory guard throughout startup to shutdown; never unlink
   or inherit its fd into agent children. Winner writes PID after lock, loser re-reads ≤2 s; any lock error
   other than contention fails start. Under guard, keep unchanged decimal PID text and refuse any live
   legacy PID regardless of age. An unverified/non-daemon PID blocks with exit 4 and a ps verification hint.
   Refresh PID lock on an independent ≤15 s timer even OFF; lost/foreign ownership disables dispatch and
   reports ownership_lost, never silently recreates. Timer cannot protect a suspended daemon from
   uncooperative old writers; stop all old processes including persistent MCP before upgrade. Only explicit
   start holding ownership replaces a proven stale own socket; clients never unlink or decide ownership.
   `(dev,ino)` identity is weaker on inode-reusing filesystems; `remove_if_same` is safe under the project
   guard, while the residual ABA window is two different projects sharing one misconfigured `ORCH_SOCKET` path.

10. **Stop and signals preserve existing reconciliation semantics.** A compatible socket stop,
    version-invariant hello stop intent and SIGTERM/SIGINT/SIGHUP stop admission/dispatch, kill live
    processes using existing grace, finish runs and resolve task retry state, then remove only the owner's
    endpoint/discovery and release ownership last. A crash leaves stale metadata; the next client reports
    NoDaemon. Explicit owner start reconciles claimed/preparing/running records before publishing readiness.
    Failure to clean up is reported, never labelled a clean stop. Foreground SIGHUP is shared-service
    shutdown, not simply detaching that pane. Explicit --detach creates a setsid session, stdin /dev/null,
    stdout/stderr private capped `.orchestry/daemon.log` (10 MiB plus one 10 MiB backup), readiness
    signalled after publication; it survives terminal close, not promised logout/reboot. No
    launchd/KeepAlive is supplied. Restart does not resurrect an agent conversation, recover a lost reply or
    guarantee all external MCP clients retry; use the normative recovery table and explicit owner/caller
    actions.

11. **Push is bounded coarse invalidation plus bus events.** Register a subscription before fetching its
    initial views. Each subscription has sequence numbers and a daemon instance; a gap or changed instance
    requires refetch. Slow subscribers receive resync or disconnect without blocking state transitions. No
    durable replay is promised. TUI consumes push instead of polling files; web's existing TUI PTY children
    use the same client. MCP sends resources/updated for subscribed orch:// resources and removes its
    redundant per-server store serialization lane while keeping bounded admission/mode checks. PR 2
    initially refreshes TUI through RPC on the existing tick, without a disk observer; PR 3 replaces RPC
    polling with push.
    *PR 3 clarifications, 2026-11-10; the decision above is unchanged.* Two wire refinements inside the
    approved contract, with `LOCAL_PROTOCOL` raised for them: (1) the subscribe result's `seq` is the
    subscription cursor's origin, always 0, the first event has `seq` 1, and the baseline commit ordinal is
    the subscribe call's terminal `commit_seq`; (2) a change event's entity `id` is optional, and an event
    without one is an ordinary ordered store-wide change (`kind` updated) for singleton stores and for a known
    store whose changed entities are not known, while resync stays rare: `lagged`, plus one reason for a
    failure that leaves the affected store set unknown. These bounds cover commit metadata and session
    queues; the engine's per-operation domain-event buffer is not capped and holds one operation's events
    until release, growing with the number emitted (follow-up R-evbuf). Wording:
    [spec, Protocol and service reference](../specs/2026-11-09-daemon-owned-state.md#protocol-and-service-reference-approved).

12. **Security and tests are part of the boundary.** Validate every frame and operation before mutation;
    bound admission by count and bytes, frame/depth, transfers and queues. Typed DTOs contain no untyped
    Value or flatten; unknown fields fail outside the frozen envelope, depth is pre-scanned, duplicate
    fields rejected. An exhaustive op-class match with no wildcard enforces fixed session modes in daemon,
    independently of the bridge; no new MCP tool or destructive permission is introduced. Separate
    handshake/session capacity and reserved control prevent ordinary saturation from becoming a false
    absence report.

    Capture a sanitized env snapshot before runtime/threads, then env_clear + snapshot + configured env +
    trusted identity/project/socket inserts for every child. The spec pins the denylist and prohibits
    per-call env/PATH/cwd/argv/adapter-command execution overrides; runtime process env is never mutated.
    The daemon's startup credentials persist until explicit restart. Logs never contain err.message/serde
    Display or payload, including parse/service failure paths; control attribution uses kernel pid/run. Same
    UID includes confined or containerized processes that see the socket, and remote principals admitted
    through web or peer task submission; they have owner authority. CLI/TUI absolutize attachment paths at
    caller cwd, MCP at project root, daemon rejects relative paths. Confined callers can still exercise
    daemon host-file authority (M3). General attachment upload was not selected: existing bounded
    image/document transfers do not cover the general attachment DTO/lifetime/size contract at zero
    additional cost. Existing web/peer security remains. Mandatory security review, real Unix lifecycle
    tests on macOS/Linux, in-process fake-engine harnesses, isolated explicit test daemons, the no-daemon
    matrix with process-table assertions, deterministic pause/cancel/edit races and real-TUI stress are
    required. The spec records the exact targets and owner-Mac QA requirements.

13. **One production cutover, then push and enforcement.** The regrouped ordered stack (RT1; lead plan §8):
    PR 1 protocol/daemon/explicit lifecycle/engine guarantees+harness/spec/plan/ADR, gated by
    ORCH_DAEMON_DEVELOPMENTAL=1 and a legacy-writers-not-excluded banner, no converted clients; PR 2
    orch-client + all CLI/MCP/TUI/web state paths together, remove developmental gate, migrate binary
    fixtures, no-daemon matrix, real orch tui in tmux + 4 CLI + 2 MCP stress, web preflight/PTY routing,
    README upgrade and CHANGELOG; PR 3 subscriptions/TUI+web push/MCP resources/delete disk observer/QA1; PR
    4 crate/source capability gates, remove obsolete dependencies/FIFO/dead code, architecture/runbook/
    manual sweep/QA2. PR 2's >3,000-line size is justified by the mixed-writer failure of splitting it. The
    single-writer behavior must hold at PR 2 head; PR 4 adds the structural check described, with its limits, in decision 16. The owner approved regrouping
    at gate 2 and merges each green PR. SE-20 developmental gating ends in PR 2, not the old PR 4 proposal.

14. **With no daemon, `orch doctor` is discovery-only, and the CLI's local store and execution frontends are
    removed.** Two PR 4 behaviour changes that make the code match decisions 4 and 7 and spec row R38.

    *`orch doctor` without a daemon.* Until PR 4 a failed connection made doctor run its own checks first: it
    version-probed five adapters (claude, shell, pi, grok, antigravity; codex, cursor and opencode were not
    probed), ran `git`, the git-repository, `.gitignore` and toolchain checks in the current directory, looked
    for `.orchestry/`, printed `N of 5 adapters ready`, and only then returned the no-daemon error. Now, when
    the connection fails, doctor starts no child process (no adapter, git or shell probe) and opens no store.
    It prints a header and two facts and returns the client error unchanged, with no exit-code mapping of its
    own:

    | Where doctor runs | Facts printed | Error and exit |
    |---|---|---|
    | Initialized project, no daemon | `project root: <root>` and `daemon discovery: available` or `unavailable` | the uniform no-daemon message and `Start it with: orch daemon start`, exit 7 |
    | Directory with no project | `project discovery: unavailable` | the existing `Not initialized` diagnosis and `Run: orch init`, exit 3 |

    `daemon discovery` says whether the client could read the project's `daemon.json` record with the same
    bounded reader that connection setup uses; it does not say that a daemon answers, and nothing is probed to
    find out. The adapter and dependency checks exist only inside the daemon, so a connected doctor is
    unchanged (eight adapters, project line, exit 0 even when checks fail), and so is the MCP `doctor` tool.
    The spec's R38 sentence names exit 7 and, for a directory with no project, exit 3. The no-project case exits 3 because that is what every other
    client gives in a directory with no project, and a start hint there would name a command that itself
    exits 3; this is the reading built.

    *Legacy frontends under the CLI's `serve` path.* PR 2 moved every command onto the socket but left the
    v0.13.0 in-process paths compiled and uncalled: `serve::run` and `run_with` over a `FullEngine` (stores,
    orchestrator and peer runtime built inside the CLI process), the `--once` terminal-state rule, the CLI's
    peer-runtime starter, the in-process log pump and the local `logs` reader, and the bare-`orch` first-run
    initializer (`tui::first_run`). No command reached any of them. PR 4 deletes them with their unit tests.
    What stays is the structured logger and its sink options, the event formatting functions, offline `orch
    init` behind the daemon's bootstrap facade, and the socket-backed `serve`, `run` and `logs` commands. The
    contracts the old tests pinned (start-up diagnostics, ordered and complete log delivery, peer lifecycle,
    a finished client batch leaving the daemon up, and the terminal-batch rule except for one case) are now
    pinned by daemon tests instead. The one case is dropped: the old `--once` code reported all tasks done
    (or a failure) when a stop arrived after every task was already terminal; the daemon checks a pending
    stop first and reports the batch as interrupted, and no test pins either order. That precedence is not a
    daemon contract.

    Reasons: R3 and source item 2 of the spec say no client links the stores. Decision 4's crate-graph and
    source gates cannot be tightened while `orch-cli` still contains code that builds a store container,
    starts the orchestrator or spawns a doctor probe, even uncalled. (`orch init`'s adapter detection is a
    probe that stays and is called: see the exception in the Consequences.) Deleting the code also removes
    any code a later change could reach for as a "fallback" when no daemon answers, which the owner ruled
    out in decision 1.

    Consequences for the owner: to learn which agent CLIs are installed, start the daemon first (it starts
    with dispatch off), then run `orch doctor`; before that, doctor only says where the project is and
    whether it left a discovery record. A script that expected exit 0 from `orch doctor` in a directory with
    no project, or the `of 5 adapters ready` line, must change: it now gets exit 3 or exit 7 and no probe
    summary. With `--json`, the offline output is the same plain-text note, not JSON; the JSON report needs the daemon. No other command
    changes: the deleted frontends had no caller, so `orch serve`, `orch run`, `orch logs` and bare `orch`
    behave as decisions 2, 3 and 7 describe. The unpublished `orch_cli` library no longer exports
    `serve::run`, `serve::run_with`, `serve::ServeArgs`, `serve::ServeOrchestrator`, `serve::OnceResult`,
    `serve::ONCE_POLL_INTERVAL`, the `serve::once` module (with `run_once`), `logs::run`, `logs::LogsDeps`,
    `doctor::run`, `doctor::container_less_registry` or `tui::first_run`.

15. **R39, the stop acknowledgement for agent-originated `daemon stop` and `daemon dispatch off`, is deferred
    by the owner and is not built in this goal.** The owner decided this on 2026-11-11. The spec's R39 asked the
    daemon to recognise such a request from the kernel-reported process group of the caller and to require
    `--from-agent`.

    Reasons. An agent with a shell can always run the command, so an acknowledgement cannot stop it. And no
    design satisfies both R39 and the unconditional stop of an old-version daemon (R6 and R14): the stop
    request is a frozen envelope that works across versions, and the check would have to run in the daemon
    being stopped.

    Consequence the owner accepted. `orch daemon stop`, `orch daemon dispatch on` and `orch daemon dispatch
    off` run in an agent's environment (`ORCH_TASK_ID` or `ORCH_AGENT_ID` set) succeed without
    `--from-agent`. The only agent-context protection is the environment-based refusal of `orch daemon start`,
    `orch serve` and `orch run --watch` (exit 2 without `--from-agent`). It prevents an accidental start by an
    agent; it is not a barrier against a process of the same user, which can unset the variables. The
    same-user peer-credential check and the project check on every connection are unchanged.

16. **The client boundary is checked by `cargo xtask crate-graph`, and the owner accepted that check with the
    gaps its documentation lists.** Decision 4 sets the crate boundaries between the clients and the stores and
    checks them with the crate graph and a source-site allowlist. This decision records how far that check
    reaches and what was accepted.

    What the check covers for the CLI. `cargo xtask crate-graph` checks the CLI crate's declared dependencies
    and scans its production source against the rules in
    [ARCHITECTURE.md section 3.1](../ARCHITECTURE.md#31-dependency-direction): the paragraph on the CLI's
    dependencies and the `orch-cli → orch-daemon` row of its table. This entry does not restate that row.

    Its limits. The source half is a text check. The same row lists the cases it does not catch, under **Not
    caught**, and says of its checks that they "flag architectural changes for review; they do not hold against
    an author deliberately defeating the scan". The cases are in that row; this entry adds nothing to them.

    What the owner accepted. On 2026-11-12 the owner accepted the check with those documented gaps. The security
    review rated them Medium (CWE-184), and its block on the check stands, as accepted by the owner. The owner
    chose not to rewrite the check's parser. The block is not lifted: this entry records the owner's
    acceptance, not a reviewer's clearance.

    The check does not analyse what code in a crate the client may depend on does with the filesystem. No automatic check rejects project-state access placed in a helper of such a crate; that is covered by source review only.

## Alternatives considered

- **Keep multiple writers and patch locks individually:** does not provide one ownership boundary across
  services, reads with maintenance, process registry and peer callbacks. The owner chose to remove that class.
- **SQLite now:** can improve crash transactions later, but violates the explicit storage non-goal and would
  combine two migrations. Keep existing file traits and formats.
- **A second framing implementation or gRPC/tarpc:** adds a load-bearing dependency/schema/tooling stack while
  leaving ownership, lifecycle and state ordering unsolved. The existing bounded framing fits local streams.
- **Move the peer Frame into orch-proto:** imports core/infra serialization into a domain-only boundary and
  couples independent compatibility surfaces. Lift only generic framing.
- **An RPC FIFO alone or locks around individual store methods:** engine collectors and peer writers still
  race; read-modify-write spans calls. Coordinate the whole operation and external-work commit points.
- **Hold a project mutex for an entire run:** agents issue orch calls and cancellation must reach them, so this
  can deadlock progress. Hold only state transitions and the final ordered spawn decision.
- **A separate orchd executable:** packaging cost without a needed boundary; the existing orch binary has
  explicit bootstrap wrappers, with crate/source gates checking capability separation (their limits are in decision 16).
- **Strictly reject stop on skew:** advertised recovery fails after installed-binary replacement (RT2).
  A retained old binary is not reliable; automatic PID signalling is less safe than SE-8's authenticated
  same-project stop. Only that frozen intent crosses skew; no general compatibility negotiation is added.
- **Partial CLI/MCP cutover before TUI/web:** RT1 demonstrates supported same-build lost updates; warnings do
  not serialize writers. Move minimal TUI/web RPC into PR 2 and defer only push to PR 3.
- **Build fingerprint or distributed ownership:** adds machinery beyond the stated scope; protocol revision
  discipline plus provenance covers the supported developer workflow, single-host topology is explicit.
- **Socket-presence or PID age as sole authority:** stale metadata and a suspended live daemon invalidate that
  inference. Clients never decide ownership, and explicit start needs a sound exclusion primitive (G2).
- **Keep the local probes in offline doctor:** useful on a machine with no project yet, but it makes a
  client spawn adapter processes and look in the project directory, which decision 1 (no client reads `.orchestry`
  beyond discovery metadata) and R38 exclude, and a second set of checks (five adapters) would drift from the
  daemon's (eight). The daemon's report is the single source.
- **Keep the uncalled in-process `serve` as a reference or emergency path:** kept compiled, it is the
  direct-store fallback decision 1 excludes, one wiring mistake away, and it keeps the CLI's dependency on the
  stores that decision 4 removes. Rejected.

## Consequences

- One cooperating daemon owns project stores and agent processes; ordered operations remove concurrent lost
  updates, pause/spawn races and context renewal/expiry races. Local cancellation stops live processes.
- This is **not** a transaction upgrade. A crash/disk error between entity and index writes can leave divergent
  files; a lost RPC reply can hide a committed mutation. No automatic replay, persisted CAS, exactly-once
  delivery or multi-call transaction is promised. Global preferences retain existing cross-project semantics.
- Old binaries/manual filesystem writers must be stopped before using this model; same-UID tools that ignore
  ownership are outside the guarantee. Live directory deletion remains unsupported. Offline repair guidance
  remains necessary, clearly separated from obsolete concurrent-MCP advice.
- The owner must explicitly start a daemon before state commands or the first MCP call succeed. Strict ordinary skew
  increases upgrade cost, but the approved frozen stop defines recovery with either binary; two-executable acceptance remains unmet at
  PR 1 and is assigned to PR 2 T2a (harness skew is not that evidence).
- With no daemon, `orch doctor` tells the owner where the project is and whether a discovery record exists,
  and nothing about installed agent CLIs; those checks need a running daemon (decision 14). Apart from the
  adapter detection of `orch init`, which stays because init runs without a daemon (it runs `--version` on
  each agent CLI when `--adapter` is not given), the CLI no longer contains any code that builds stores,
  runs the orchestrator or probes adapters in its own process; `orch doctor` has none.
- Tests mostly retain their in-process model. Binary tests explicitly start an isolated test daemon and never
  connect to a real project or real global configuration. Mac QA and Linux CI cover different OS boundaries.
- Four stacked PRs deliver the transition under decision 13. PR 1 is explicitly developmental; PR 2 has no
  supported mixed-writer frontend, with real-path stress evidence at that head. PR 3/4 finish push and the structural
  check (its limits are in decision 16). Owner merges; no release/tag/version bump here.
- Rollback requires explicit owner stop and verification that agent children are reconciled, then selecting
  the previous binary. File formats remain readable; the old multiple-writer limitations return.
- At stance 0.5, choose shared operation coordination and coarse push over a per-entity transactional framework:
  enough restructuring to remove the actual races without replacing storage.
