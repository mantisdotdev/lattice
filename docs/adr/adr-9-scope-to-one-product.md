# ADR-9 — Scope v1 to one product

**Status:** Accepted · **Decides:** which gates v1 ships against · **Does not touch:** any harness

## Context

The 44 unmeasured gates describe five products: a local version control
system (G1, G2), a git bridge (G3), a semantic merge engine (G4), a
provenance, attestation and sync platform (G5), and audits of all of it (G6).
Eleven of fifty-five gates pass and every one of those is a document. Of the
stages that build software, three gates are green.

Three facts from the harnesses decide what happens next, and none of them is
about performance:

- **G1.4's pool is ten verbs and five did not exist** (`split`, `lens`,
  `thin`, `compact`, `sync`), with a coverage contract of 100 successful
  emissions each. G1.1 fails the same way: 427 failures, every one an
  unimplemented command, zero checkpoints lost. Both HARD gates are blocked on
  verbs existing, not on speed.
- **G1.3 requires twelve verbs in the discovered surface** and six of them did
  not exist (`split`, `sync`, `redact`, `thin`, `lens`, `merge`).
- **No harness exists for G2 through G6.** Thirty-four gates are defined in
  `gates.toml` and cannot be measured until a harness is written for each.

## Decision

1. **v1 is the version control system plus semantic merge: G1, G2, G4.** G5 is
   deferred whole — eight gates and the largest build. G3 is reduced to a
   one-way importer (`adopt`); round-trip fidelity with git is a second
   product.
2. **Verbs ship as the smallest true behaviour, never as stubs.** `sync
   --dry-run` with no remote reports there is nothing to sync, because there
   is not. `thin` collects only packs whose every chunk is unreferenced,
   because that cannot lose data. `compact` archives an op-log segment,
   which is ADR-13's design with its second half — truncating the live log —
   left for later. A verb that would have to pretend is not shipped.
3. **The daemon is deferred.** ADR-4 already calls it an accelerator, never a
   requirement. Latency gates are SOFT and specified daemon-resident; where
   the daemonless number misses, that is recorded as a miss.
4. **The working-tree index is not on the critical path** of any HARD gate
   and is not built until one needs it.

## Consequences

- **G1.1 cannot pass under its frozen harness even with every verb built.**
  Its coverage contract requires a fault hit in a `merge` critical section,
  and its operation pool contains nothing that writes to one. That is a
  harness inconsistency, not an engine gap; amending it is a §0.3 change
  needing its own ADR stating the measurement became looser, and it is
  recorded here rather than worked around.
- **Thirty-four harnesses are owed** before G2–G6 can report anything. Each
  is frozen on creation and must be written with the same care as the
  engine, which makes "measure the gates" a build task, not a run.
- Process for this stage: an ADR only for a decision that is contested or
  irreversible; the review gate blocks on findings of Major and above;
  mutation checks for code that can lose data.
