# ADR-3 — Store backend: append-only packs for content, embedded transactional store for metadata

**Status:** Accepted · **Stage:** G0.6 · **Constrains:** G1.1, G1.4, G1.8, G1.9, §5.1

## Context

§5.1 leaves the backend open and says how to close it: "embedded proven store
(redb/RocksDB) *or* custom append-only packs — decide by benchmark, not taste."
The store must serve three access patterns that pull in different directions:

- **A — bulk chunk write.** Many immutable content-addressed blobs inserted
  during one `ltx save`. Throughput matters. Per-item durability does not: a
  chunk is invisible until the checkpoint referencing it is durable.
- **B — random chunk read.** Point lookups by 32-byte key during checkout, diff
  and verify, over a working set larger than RAM. Latency matters, and it feeds
  G1.5/G1.7 directly.
- **C — durable metadata mutation.** The operation log's append, plus reference
  and checkpoint updates, which must be atomic and durable before a command
  reports success. This is on the critical path of every `ltx` invocation.

Pattern C also carries the requirement that makes this decision hard: G1.1 is a
HARD gate demanding survival of ≥1,000 simulated power-loss cases with torn and
reordered un-fsynced writes. Whatever holds metadata must be crash-atomic, and
writing that correctly from scratch is exactly the kind of thing §0.8's novelty
budget says to buy rather than invent.

## Options

**Option A — Everything in redb.** One dependency, one crash-recovery story,
transactional by construction. Risk: a B-tree with copy-on-write pages is a poor
container for hundreds of thousands of multi-kilobyte immutable blobs.

**Option B — Everything in custom append-only packs.** Optimal for immutable
content; matches git's and restic's shape. Risk: metadata mutation and crash
atomicity become ours to implement, against a HARD gate.

**Option C — Split by access pattern.** Append-only packs plus a sorted index for
chunk content (A and B); an embedded transactional store for the op-log,
references, checkpoints and the provenance index (C). Two mechanisms, each used
where it is strong.

## Decision

**We will build Option C.**

- **Chunk content: custom append-only packs** with an immutable sorted index per
  pack. Packs are written once, fsynced once, and never mutated; a pack's index
  is written and fsynced after its data, so a crash mid-write leaves an
  unreferenced pack that recovery discards. Immutability is what makes this safe
  to hand-roll: there is no update-in-place to get wrong.
- **Metadata: redb.** The op-log, references, checkpoint graph, changesets, lens
  definitions and the provenance index live in a single embedded transactional
  store. We buy crash atomicity rather than building it.
- **The op-log uses group commit** (ADR-4), because the measured cost of a true
  media flush makes per-command flushing untenable under concurrency.

## Consequences

**Positive.** Each pattern gets the mechanism that measured best: packs are 2.3×
faster to write, 2× faster to read, and use **3.3× less disk** than redb for the
same chunk content. Metadata keeps a transactional store's crash story, which is
the part G1.1 punishes hardest. Packs also give partial clone (§5.6, G5.8) a
natural transfer unit, and give verification a natural boundary.

**Negative, and accepted.** Two storage mechanisms means two recovery paths and a
consistency invariant between them: a checkpoint in redb may reference chunks in
a pack. We fix the ordering — *content is durable before the metadata that
references it* — so a crash can only ever leave unreferenced chunks (garbage,
collected later), never a dangling reference. That invariant is the single most
important thing G1.1's fault-injection harness must attack, and it is written
into the harness's declared critical sections.

**Rejected sub-option, recorded.** RocksDB was not benchmarked. It is a larger
dependency with a C++ build, LSM write amplification that is worse than redb's
for pattern A, and compaction pauses that would land on the p95 of gated
latencies. Given redb measured *adequately* for pattern C and packs won pattern
A/B decisively, adding a third candidate would not have changed the split. This
is a judgment, not a measurement, and is recorded as such.

## Evidence

**Measured, this repository.** `bench/results/raw/adr3-store-bench.json` from
`prototypes/storebench` — 40,000 chunks × 8 KiB (312.5 MiB of content), 5,000
random reads, 400 durable appends, on the machine in `bench/ENVIRONMENT.md`:

| Backend | Operation | per-op µs | p50 | p95 | p99 | on disk |
|---|---|---:|---:|---:|---:|---:|
| redb | bulk chunk write | 133.7 | — | — | — | **1,028.5 MiB** |
| redb | random chunk read | 6.0 | 5.1 | 9.2 | 23.4 | — |
| redb | durable op-log append | 3,311.9 | 3,124 | 4,824 | 7,849 | 1.5 MiB |
| append-only packs | bulk chunk write | **58.7** | — | — | — | **314.2 MiB** |
| append-only packs | random chunk read | **3.0** | 2.5 | 3.8 | 5.2 | — |
| append-only packs | durable op-log append | 9,412.7 | 9,592 | 19,186 | 26,283 | 0.1 MiB |

The decisive number is the disk column. redb stored **1,028.5 MiB** for 312.5 MiB
of chunk content — **3.3× write amplification** from copy-on-write B-tree pages
and free-page tracking. G1.9 caps the total store at 1.25× restic and 1.5× a
git-gc pack; a backend that triples the content before any of Lattice's own
overhead is applied cannot meet that, so pattern A settles itself.

The op-log row looked like it argued the other way, and it was investigated
rather than accepted (`prototypes/fsyncbench`, tabulated in ADR-4). The finding:
the packs figure is the *true* cost of `F_FULLFSYNC`, a real media flush, and
redb's apparent advantage is amortisation, not a different durability guarantee.
Once group commit is applied the two converge, so the op-log row does not
override the disk row — it constrains how the op-log must be written. Notably,
the hypothesis that pre-allocating the log would remove the cost was **refuted**
by measurement.

**Related.** restic's repository format, the reference design for content packs:
<https://restic.readthedocs.io/en/stable/100_references.html#repository-format>.
redb's design and its copy-on-write page model:
<https://github.com/cberner/redb/blob/master/docs/design.md>.
`docs/prior-art/cdc-storage.md` for how borg, casync and Xet resolve the same split.
