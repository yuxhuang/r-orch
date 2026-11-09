# 0012. One explicit project daemon owns state

- Status: proposed
- Date: 2026-11-09
- Supersedes: ADR [0011](0011-mcp-server.md) decision 7 and the D10 concurrent-writer acceptance, when accepted and the client conversion is complete. Other MCP decisions remain except where the implementation is explicitly replaced below.

## Context

The owner chose daemon-owned state instead of accepting multiple independent file-store writers
([spec](../specs/2026-11-09-daemon-owned-state.md), [plan](../plans/2026-11-09-daemon-owned-state.md)).
The owner amended the request on 2026-11-09: clients never start a daemon, and never fall back to direct stores.
The approved v2 plan retains an owner requirements approval at gate 2; this proposed ADR does not replace it.

Today `build_light` creates file stores for one-shot commands and each MCP call. The full engine separately
owns dispatch and live processes. The PID lock protects orchestrators, not all writers. Existing store locks
serialize index updates but not every complete read/modify/write across services. Context reads can purge
expired entries. The MCP FIFO lane protects one MCP process only. Entity and index writes remain separate.
A socket alone would merely move those races into a single process: engine callbacks and RPC services must
also share an operation boundary.

Source inspection also found two decisions requiring disposition before implementation: the current PID
lock can treat a live PID as stale after 60 seconds, and strict version rejection makes an upgraded binary's
`daemon stop` fail. The spec records concrete alternatives as G1/G2. Neither issue licenses a client recovery
side effect.

## Decision

1. **One daemon per canonical project owns `.orchestry` and agent processes.** It holds project ownership
   through startup reconciliation, dispatch ON or OFF, and shutdown. All CLI, TUI, web, MCP and agent-invoked
   state operations use a local socket. No direct-store fallback exists, including reads. `init` uses a narrow
   bootstrap entry point to create the project and prints the explicit-start command without starting it.
   Pure help/version rendering does not require state access. Discovery metadata is the sole client read
   exception under `.orchestry`.

2. **NO AUTO-START is an owner decision.** The reason is: **the owner wants to know when a long-lived process
   starts on their machine**. `orch daemon start` is explicit and foreground by default; `--detach` is explicit
   convenience. `orch serve` and `orch run --watch` are explicit foreground starts with dispatch ON. No
   client connection, retry, MCP initialize, TUI/web launch, error path or bootstrap command starts the daemon.
   Clients never remove socket/discovery files. There is no idle exit, watchdog or automatic process replacement.

3. **Fail fast with a single NoDaemon contract and exit 7.** Existing exit codes 1–6 retain their meanings.
   The message names the canonical project and `orch daemon start`, with a safe detail for absent/stale/refused
   socket, handshake failure or version skew. TUI presents the same text and retry/quit; web exits 7 before
   startup or returns HTTP 503 after a live outage. MCP returns the same message in initialize/tool errors with
   structured exit_code 7, remains a stdio bridge and exits 0 on EOF. The protocol code is not confused with the
   MCP process exit status. No absence case grants store access.

4. **Three crate boundaries enforce ownership.** `orch-proto` has domain types plus serde/Tokio framing;
   `orch-client` depends only on domain/proto among workspace crates and contains no process creation or store
   access; `orch-daemon` composes engine, infra, peer and proto. The CLI depends on daemon only for the
   allowlisted initialization and explicit-start wrappers. The whole-edge crate-graph check and bidirectional
   source-site allowlist include dev/build dependencies. TUI remains domain-only. The CLI's desktop editor,
   clipboard and web PTY concerns remain frontend effects, while all agent execution belongs to the daemon.
   No daemon API re-exports stores or arbitrary execution closures to clients.

5. **Reuse orch-peer's bounded NDJSON framing, not its protocol enum.** Lift generic reader/serializer code
   into orch-proto; retain the 1 MiB line bound. Local messages are typed hello/welcome/reject, req, ok/err,
   part and ev frames. The spec's protocol appendix records the catalogue, operation mapping, sequencing,
   bounded admission and chunk handling. Peer-specific oversize event/outcome policies stay in orch-peer.
   Its TCP Frame, PROTOCOL=1, optional version fields and mixed 0.5.x/current compatibility remain unchanged.
   Local and peer versions are independent; no strict local binary equality rule leaks into TCP links.

6. **Handshake requires protocol u32 and exact binary semver equality in both directions.** Newer client with
   older daemon and older client with newer daemon fail before any request can execute, even with equal
   protocol numbers. Error text names both versions and instructs explicit owner stop then start. Same semver
   development builds are not cryptographic binary identity; this design does not claim to detect that case.
   Identity (`--agent` over ORCH_AGENT_ID, existing per-call sender rules) is attribution, not authorization.
   Same-euid kernel peer credentials are the socket boundary in both directions.

   The strict rule also rejects `daemon stop` from a mismatched binary. G1 recommends retaining a matching
   binary for the owner's stop, or independently verified owner SIGTERM, before starting the new version.
   A version-independent stop control would be a separately approved exception; none is assumed here.

7. **Dispatch is a mode inside the owner.** Fresh `daemon start` defaults OFF; explicit `--dispatch on` opts
   in. `daemon dispatch on|off` changes future scheduling via socket without dropping ownership or killing
   current agents. `run --once` requests one tick; `run <id>` launches and observes that task via socket;
   `run --all` remains a finite batch operation. The proposed `serve --once` mapping preserves its existing
   until-terminal result semantics through a daemon operation, distinct from one tick (G3). TUI watch controls
   toggle dispatch, not process lifecycle. Expiry and peer handling remain active with dispatch OFF.

8. **One shared state-operation boundary closes in-process races.** Services, dispatch/spawn decisions,
   collectors, peer persistence and expiry serialize entire read/validate/modify/save transitions. Long agent
   execution, remote/network waits and human editors run outside that boundary with revalidation on commit.
   Pause orders the pending-task cancellation decision before any later launch; live task cancel reaches the
   process registry and process manager; context expiry is an explicit daemon step, never a client-read side
   effect. Config changes refresh subsequent service/dispatch defaults without replacing the live registry.
   Existing engine services and storage traits remain the behavioral base; this adds coordination, not a second
   implementation of their business rules.

9. **Explicit ownership precedes stale-file replacement.** Default socket is `.orchestry/orch.sock`; an
   absolute path exceeding 100 bytes uses `$TMPDIR/orch-<sha256(canonical root)[..16]>/orch.sock`. Validate the
   final path too; ORCH_SOCKET is an explicit absolute override. Same-owner directories are 0700, socket 0600,
   and daemon.json 0600 records endpoint, versions, pid, start time and instance. Clients read it and connect;
   project identity is checked at handshake so a wrong override cannot cross projects. Only explicit start,
   after ownership, may replace a proven stale own socket. Symlinks/regular files/foreign ownership are errors.

   G2 proposes a stable advisory `.daemon-owner.lock` held for the lifetime plus the unchanged decimal PID
   file. This prevents the existing live-PID age eviction and concurrent stale-unlink race without changing
   entity/index formats. Never unlink the advisory guard; do not inherit its descriptor into children. The lead
   must record this coordination refinement before building; the old age-based PID algorithm alone is not
   adequate proof of one owner.

10. **Stop and signals preserve existing reconciliation semantics.** A compatible socket stop and
    SIGTERM/SIGINT/SIGHUP stop admission/dispatch, kill live processes using existing grace, finish runs and
    resolve task retry state, then remove only the owner's endpoint/discovery and release ownership last.
    A crash leaves stale metadata; the next client reports NoDaemon. Explicit owner start reconciles
    claimed/preparing/running records before publishing readiness. Failure to clean up is reported, never
    labelled a clean stop.

11. **Push is bounded coarse invalidation plus bus events.** Register a subscription before fetching its
    initial views. Each subscription has sequence numbers and a daemon instance; a gap or changed instance
    requires refetch. Slow subscribers receive resync or disconnect without blocking state transitions. No
    durable replay is promised. TUI consumes push instead of polling files; web's existing TUI PTY children use
    the same client. MCP sends resources/updated for subscribed orch:// resources and removes its redundant
    per-server store serialization lane while keeping bounded admission/mode checks.

12. **Security and tests are part of the boundary.** Validate every frame and operation before mutation; bound
    admission, frame/depth, transfers and queues; never log protocol payloads or secrets. Listener configuration
    never inherits agent/request env; child adapter env is constructed explicitly. Same UID is trusted authority,
    not a sandbox against the local owner. Existing remote and web security remain. Mandatory security review,
    real Unix lifecycle tests on macOS/Linux, in-process fake-engine harnesses, isolated explicit test daemons,
    the no-daemon matrix with process-table assertions, deterministic pause/cancel races and multi-client stress
    are required. The spec records the exact targets and owner-Mac QA requirements.

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
  explicit bootstrap wrappers, with crate/source gates enforcing capability separation.
- **Socket-presence or PID age as sole authority:** stale metadata and a suspended live daemon invalidate that
  inference. Clients never decide ownership, and explicit start needs a sound exclusion primitive (G2).

## Consequences

- One cooperating daemon owns project stores and agent processes; ordered operations remove concurrent lost
  updates, pause/spawn races and context renewal/expiry races. Local cancellation stops live processes.
- This is **not** a transaction upgrade. A crash/disk error between entity and index writes can leave divergent
  files; a lost RPC reply can hide a committed mutation. No automatic replay, persisted CAS, exactly-once
  delivery or multi-call transaction is promised. Global preferences retain existing cross-project semantics.
- Old binaries/manual filesystem writers must be stopped before using this model; same-UID tools that ignore
  ownership are outside the guarantee. Live directory deletion remains unsupported. Offline repair guidance
  remains necessary, clearly separated from obsolete concurrent-MCP advice.
- The owner must explicitly start a daemon before state commands or the first MCP call succeed. Strict skew
  increases the operational cost of upgrades; G1 must settle usable owner instructions before approval.
- Tests mostly retain their in-process model. Binary tests explicitly start an isolated test daemon and never
  connect to a real project or real global configuration. Mac QA and Linux CI cover different OS boundaries.
- Four stacked PRs deliver the transition. The full invariant is claimed only after all client conversion and
  enforcement land; intermediate entry points must be labelled/tested as transitional and never encouraged to
  run legacy writers beside the experimental daemon. Owner merges; no release/tag/version bump here.
- Rollback requires explicit owner stop and verification that agent children are reconciled, then selecting
  the previous binary. File formats remain readable; the old multiple-writer limitations return.
- At stance 0.5, choose shared operation coordination and coarse push over a per-entity transactional framework:
  enough restructuring to remove the actual races without replacing storage.
