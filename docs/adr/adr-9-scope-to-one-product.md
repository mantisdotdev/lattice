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

   > **Corrected the same evening.** Something adjacent to it was on the
   > critical path, and the plan said otherwise: G1.4 on the new verbs ran at
   > roughly a hundred operations a minute and was still slowing at its
   > few-hundredth operation of eighty thousand. Not the lock. Every `switch`
   > pruned the working tree and rewrote all of it, and the gate's eight
   > workspaces each gain a file per operation. Measured with
   > `scripts/probe_scaling.py`, which gained a `switch_s` row for the
   > purpose — a switch between two lines that differ by one file — against
   > a binary built before the change (`bench/results/raw/adr9-scaling-before.json`):
   >
   > ```json
   > {
   >   "files": 10000,
   >   "first_save_s": 0.376,
   >   "incremental_save_s": 0.372,
   >   "status_s": 0.062,
   >   "switch_s": 35.835
   > }
   > ```
   >
   > and after it (`bench/results/raw/adr9-scaling.json`):
   >
   > ```json
   > {
   >   "files": 10000,
   >   "first_save_s": 0.595,
   >   "incremental_save_s": 0.652,
   >   "status_s": 0.098,
   >   "switch_s": 0.806
   > }
   > ```
   >
   > Thirty-six seconds to under one, for a one-file difference. The old
   > cost also grew faster than the tree — 1.8 s at 1,000 files, 18.5 s at
   > 5,000 — because each written entry was checked against every entry
   > already written in its directory. Materialisation now reconciles the
   > snapshot every caller already takes against the target and touches only
   > what differs; what remains is the snapshot walk. Both arms ran on one
   > machine while two gates were also running, which is why the after arm's
   > saves are slower than the before arm's: the ratio is the finding, and
   > no absolute here is quiet-machine.
   >
   > The index proper — reading only what the metadata says changed — is
   > still not built, and this correction does not change that. It changes
   > what "not on the critical path" was allowed to mean.

   > **Corrected again the same night, from the rerun.** With switches fixed,
   > G1.4 ran at 239 operations a minute at op-log entry 7,000 and 14 a
   > minute at entry 18,400, and was stopped there. Not the lock, and not
   > materialisation this time. `bench/results/raw/adr9-g1-4-stall.json`
   > records the entry size by sequence, from the archive segments `compact`
   > wrote:
   >
   > ```json
   > {
   >   "op_log_seq_reached": 18437,
   >   "redb_bytes": 350445568
   > }
   > ```
   >
   > Entries averaged 1,160 bytes over the first 1,800 and about 10,000 bytes
   > by entry 16,000. An `assign .` records every path it moved, and each
   > workspace gains a file per operation — so entry size grows with the run.
   > On its own that is linear. What makes it quadratic is that `undo`,
   > `thin` and `log` load and deserialise the entire log to answer: at entry
   > 18,000 that is roughly 120 MB of JSON per call, and the pool draws one of
   > those every few operations. Same shape as the switch defect, one layer
   > down.
   >
   > The fix needs no format change and is the first thing tomorrow: enumerate
   > checkpoints from the `SAVED` index that already maps id to sequence, and
   > have undo scan from the tail and stop at the first eligible entry rather
   > than loading all of them. Shrinking the entries themselves — paths
   > serialise as arrays of integers — would need a format break, because the
   > chain hashes the serialisation, and is not the bottleneck once nothing
   > loads the whole log.
   >
   > G1.3 did not report either: its in-process half ran into the frozen
   > harness's own 7,200-second budget on a disk shared with G1.4. It is
   > fsync-bound at a hundred thousand fresh repositories and needs the
   > machine to itself.

   > **Corrected a third time, 2026-09-12, from the full run.** With the
   > empty change records closed (ADR-17) the run held 1,037 operations a
   > minute at entry 4,000 and 299 at entry 16,000, still falling. Timing every
   > pool command on a clone of the run at entry 16,609 (2,014 packs) found two
   > terms that grow with history: every command paid ~115 ms before doing
   > anything, because opening the store read every pack index; and
   > `internals thin` cost 970 ms, because its "bounded" walk starts at the
   > oldest unanchored candidate, and a pack a switch captured whose file a
   > later save reused is unanchored but reached — so it stays the oldest
   > candidate forever and every thinning walks all of history from it.
   > `bench/results/raw/adr9-g1-4-thin-and-open.json`, the two binaries run
   > alternately on the same clone:
   >
   > ```json
   > {
   >   "thin_before_ms": 1227,
   >   "thin_after_ms": 177,
   >   "switch_before_ms": 277,
   >   "switch_after_ms": 88,
   >   "workspace_list_before_ms": 129,
   >   "workspace_list_after_ms": 50
   > }
   > ```
   >
   > Two changes, neither a format change. The thin ledger records the
   > candidates a thinning walked for and found live; a saved checkpoint is
   > never unsaved, so they are left out of every later thinning's candidates
   > and the walk's starting point moves forward (a pack recorded live is never
   > reconsidered, so the worst case is a leak, never a loss). And the store
   > opens its packs on first use rather than on every command: the commands
   > that never look up a chunk — assign, split, lens, sync, compact, the undo
   > of a save — no longer pay for the ones that do. What remains and still
   > grows is the cost of the commands that must read content, which reaches
   > every pack index; that is the pack count, and bounding it (merging packs)
   > is blocked on the same ordering thin leans on, recorded above for
   > `rewrite_pack_without`.

   > **Corrected a fourth time, 2026-09-12, from the next full run.** With
   > the store opening its packs lazily and thin's walk bounded, the run still
   > slowed: 71 ms an operation at entry 8,000, 176 at 20,000. Sampling `save`
   > on a clone at entry 23,630 (2,785 packs) put 43 of 94 samples inside the
   > `open` syscall, one per pack index, ~36 µs each on APFS: every command
   > that reads content was opening every pack. The store now keeps one
   > append-only index cache beside the packs — every pack's index bytes in a
   > single file, a tombstone when a pack is removed, a torn tail read up to
   > the tear and rewritten whole by the next write — and a pack id is never
   > reused while the cache remembers it, which is the one way a cached index
   > could describe a pack it was not written for. Each record also carries
   > the pack's byte length, checked against the file when the packs open, so
   > a torn pack is skipped exactly as an unindexed one is and never vouched
   > for; both cache writes are fsynced. Derived and disposable: the directory
   > listing still says which packs exist.
   > `bench/results/raw/adr9-g1-4-index-cache.json`, the two binaries run
   > alternately on the same clone by
   > `python3 scripts/probe_ab_commands.py OLD NEW CLONE/repo --runs 3`, which
   > creates a fresh workspace, writes a new file before every run, and prints
   > the medians it quotes:
   >
   > ```json
   > {
   >   "save_before_ms": 305,
   >   "save_after_ms": 153,
   >   "undo_before_ms": 270,
   >   "undo_after_ms": 137,
   >   "thin_before_ms": 206,
   >   "thin_after_ms": 90
   > }
   > ```
   >
   > What remains is linear in the working tree — a save or a capture hashes
   > every file, and this harness adds one per operation per workspace — and
   > the cache read itself, about half a kilobyte per pack.

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
