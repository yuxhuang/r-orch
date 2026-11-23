# 0019: Persist typed attention facts and order decimal IDs numerically

## Status

Proposed. Supplements ADR 0017's attention mapping for T10; runtime activation
and owner merge remain gated.

## Context

The attention port added closed typed facts after schema version 1. Its decimal
TEXT IDs sort lexically: a cursor after 9 misses 10 and 11. Stored diagnostic
prose cannot reconstruct either disk subject or the observed numeric facts.

## Decision

Keep migration 1 immutable. Numbered migration 2 adds a bounded `facts_json`
column holding the serde representation of `AttentionFacts`, including its
closed tagged disk subject. New rows require facts. Legacy rows stay intact
with NULL facts and reads refuse them rather than invent observations.
The spec's T8c/T10 attention section defines the metadata and scope mapping.

Keep canonical positive decimal TEXT IDs through i64::MAX. Both page predicates
and ordering, and bounded expiry selection, cast IDs to INTEGER. A matching
scope-first expression index supports enumeration. Scope-local `attention:id`
counters are initialized from existing rows. An insert trigger advances the
counter with the row in a single atomic SQL statement. Retention leaves the
counter intact, so expired IDs cannot be reused. No dependency is added.

## Consequences

Both disk subjects and project dispatch facts round-trip without parsing prose.
No API or protocol format changes. Existing schema-1 databases migrate using
the checked pre-migration copy and forward-only machinery from ADR 0017.
The prior binary refuses schema 2; recovery uses the checked copy, not a down
migration. Legacy attention rows without facts need operator disposition before
they can be displayed; this inactive mapping never silently drops them.
The caller still owns authorization, the enclosing transaction, error handling,
and the read snapshot. Public Storage composition and runtime policy remain
with T10 and T15/T19.
