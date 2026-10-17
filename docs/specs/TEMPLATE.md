# <Title>

status: draft
owner: <role or name>
date: YYYY-MM-DD
plan: [../plans/YYYY-MM-DD-<slug>.md](../plans/YYYY-MM-DD-<slug>.md) (or "to be written"; link it when the plan lands)

## Source
> The owner's request, verbatim, items numbered 1..n.

## Requirements

| Id | Requirement | Source | Priority | Acceptance check (test or command; platform) |
|---|---|---|---|---|
| R1 | When <trigger>, <component> shall <response>. | item 1 | must | `crates/<crate>/tests/<file>.rs::<test>`; Linux CI + macOS |
| R2 | <component> shall <response>. | item 2 | should | `orch ... --json` returns `...`; macOS |
| R3 | — | item 3 | won't | Reason it is out of scope. |

## Non-goals
What this deliberately does not do.

## Constraints
Compatibility (on-disk, CLI, `--json`, peer protocol), performance bounds, platforms.

Security checklist. Fill this in if the work listens on a socket, crosses a process boundary or handles
untrusted text; otherwise write "n/a":
- Default bind and exposure:
- Bounds on input size, count and rate:
- Permissions of files it writes:
- Secrets and environment passed to children:
- Untrusted text rendered to a terminal or passed to a shell:

## Verification environment
What runs locally, what only on Linux CI, and what needs owner permission or hardware. Name who runs it.
Dashboard behaviour: the tmux end-to-end tests that cover it, and what tmux cannot cover (native terminal
profiles, the native clipboard, `orch web` in a browser).

## Open questions
- ...

## Verification
Filled in by the PR that completes the work.

| Id | Result (`pass` / `env-limited` / `waived by owner`) | Commit the check ran at | Platform |
|---|---|---|---|
| R1 | | | |

Defects found outside the requirements:
- ...
