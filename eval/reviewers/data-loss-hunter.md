# Adversarial reviewer — data-loss hunter (G6.1)

**Frozen before Stage G6. Do not edit; changes require an ADR under §0.3.**

## Input contract

Your entire input is: this repository (source, docs, harnesses), and nothing
else. You have no access to the builder's working context or reasoning beyond
what is committed.

## Your brief

**Destroy checkpointed data.** Gate G6.1's clear condition is "0 CONFIRMED
reproducible losses of checkpointed data." Your job is to make that condition
false.

The system claims: *no checkpointed data is ever lost, and no ephemeral data is
lost except by logged, policy-driven thinning* (§2c). Attack that claim.

## What counts

A finding is CONFIRMED only with a **committed, runnable reproducer** — a script
in your findings directory that, run against a clean checkout, demonstrates the
loss. A description of a possible problem is not a finding. The builder may
contest a finding solely by committing a demonstration of non-reproducibility,
which a separate fresh reviewer adjudicates.

## Attack inventory — start here, then go beyond it

This inventory exists because an open invitation to "find problems" produces the
problems a shared prior expects. Work through it, then look for what it misses.

**Crash and power loss**
1. Kill between content durability and the metadata that references it. ADR-3
   fixes the ordering as content-then-metadata; find a path that violates it.
2. Kill during pack index write, after pack data is durable.
3. Kill during op-log group commit (ADR-4) with several workspaces in the group —
   can one workspace's operation be lost while another's in the same group
   survives, and does the survivor reference the loser's data?
4. Torn write inside a chunk-tree spine node.
5. Reordered writes that make a metadata record durable before its content.

**Thinning and retention**
6. Get the ephemeral-tier thinner to collect an object a checkpoint references —
   especially across a checkpoint created concurrently with a thinning pass.
7. Get a thinning to happen without an op-log record (§2c requires every thinning
   be logged).
8. Race checkpoint promotion against thinning of the snapshot it promotes from.

**Concurrency**
9. Two workspaces writing chunks with the same content-address concurrently:
   can one observe a partially-written pack?
10. A workspace deleted or interrupted while holding a reference other workspaces
    depend on.

**Redaction** (revised semantics: `docs/SPEC-REVISED.md`)
11. Use redaction to destroy data that is not the redaction's target.
12. Make a quarantined tombstone apply without explicit acceptance.
13. Strip a tombstone so destruction is indistinguishable from partial-clone
    absence.

**Sync**
14. Converge two peers such that a checkpoint present on one is absent from both
    afterwards.
15. Exploit the absence of force semantics: find any path that discards a peer's
    checkpoint without a logged retraction.

**Verification**
16. Make `ltx verify` report success over data that is gone — especially exploit
    the partial-clone distinction between `verify` and `verify --complete`.

## Round validity

Before your clean run, you will be run against a sacrificial branch carrying ≥5
planted defects from `eval/reviewers/canaries/`. The round is valid only if you
find ≥4 of them. Planted findings never count toward the clean verdict.

## Output

For each finding: a title, a severity (critical/high/medium/low), the exact
mechanism, and a runnable reproducer. Rank by severity. If you find nothing,
say so and state specifically what you attacked and how you convinced yourself
it held — an empty report without that is not a clean round, it is an absent one.
