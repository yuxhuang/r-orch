# Stance

Shared rule for all agent roles in this workspace. Read this when a task prompt gives you a `stance` value — do not duplicate this text into role files.

`stance` is a number from 0.0 to 1.0 that sets how boldly you choose among *defensible* options. It is a direction dial, not a quality dial.

## What stance governs

| Dimension | 0.0 — conservative | 1.0 — progressive |
|---|---|---|
| Solution space | Proven and boring; patterns already used in this codebase | Novel approaches; patterns new to this codebase |
| Scope | Smallest correct delta; leave adjacent code alone | Restructure adjacent code when that removes the root cause |
| Dependencies | Pinned, LTS, widely adopted; avoid adding any | Current major versions; add one when it clearly earns its place |
| Abstraction | Extend existing patterns; duplicate before abstracting | Introduce the right abstraction now |
| Migration | Additive, backward compatible, dual-run | Cutover with a rollback plan; breaking changes with a migration path |
| Unknowns | Avoid; prefer the option whose failure modes are known | Accept bounded unknowns where the upside is large and blast radius is contained |
| Research and writing | Consensus positions, established sources, explicit hedging | Contrarian framings and original synthesis, labeled as such |

## What stance never governs

Stance changes *which* defensible option you pick. It never lowers the floor. At **every** value, including 1.0:

- Correctness, security, boundary validation, and error handling are not negotiable.
- Tests are not optional, and neither is a rollback story for anything that ships.
- Claims still need sources, and uncertainty still gets stated.
- "Progressive" is never a licence for unreviewed, untested, or speculative work presented as finished.

If you find yourself justifying a weaker safety, security, or accuracy property *because* the stance is high, you have misread the dial. Say so and hold the floor.

## Bands

- **0.0–0.2 — Conservative.** Minimal change, proven tech only, reversibility over elegance.
- **0.2–0.4 — Cautious.** Prefer the known option; a new idea needs a clear payoff to win.
- **0.4–0.6 — Balanced.** Weigh novelty and proven-ness on their merits. Default.
- **0.6–0.8 — Progressive.** Prefer the better end state over the smaller diff, and pay the migration cost for it.
- **0.8–1.0 — Aggressive.** Optimize for the best achievable design. Rework, replace, and adopt where it wins — with the risk stated and contained.

## Reporting

When stance drove a choice, say so in one line: the option taken, the option not taken, and the stance-sensitive reason. That line is what lets a reviewer disagree with the dial instead of with you.
