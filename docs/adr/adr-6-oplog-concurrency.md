# ADR-6 — Concurrent access to one repository

**Status:** Accepted · **Discharges:** ADR-4's deferral of op-log concurrency
**Constrains:** the `workspace` slice, which cannot be designed before this
**Gates:** G1.4 (concurrency, HARD), G1.1 (crash safety, HARD), G1.12 (cross-platform, HARD)

## Context

ADR-4 chose the daemon-as-accelerator, committed that "no command may be
unavailable without the daemon", and then handed one problem forward:

> The op-log therefore uses **group commit** across concurrent workspaces. This
> is what makes G1.4 tractable at all: 8 workspaces × 10,000 operations at
> 4.7 ms each would be 6.3 hours of pure fsync. This constraint is handed to
> ADR-6 (op-log concurrency …).

ADR-6 was never written. `docs/adr/` goes from 4 to 12. In the meantime the
group commit was built — inside `OpLog`, amortising a flush across threads that
are already inside one process.

That is not where the contention is. **A second `ltx` process cannot open a
repository at all.** redb takes a non-blocking exclusive `flock` on its database
file, so the second process fails immediately:

```json
{"category":"io","concept":"none","error":"Database already open. Cannot acquire lock.","ok":false}
```

<!-- evidence: verbatim stdout of `ltx save --json` from a second concurrent process, reproduced by scripts/probe_concurrency.py; see bench/results/raw/adr6-concurrency-unlocked.json for the run this came from -->

Group commit never engages across processes, because the processes never get
far enough to commit. G1.4 draws 8 concurrent workspaces × 10,000 operations
against a target of **0 failures**; measured on the implemented half of its
pool, the overwhelming majority of 200 operations failed on the lock, and every
one of them failed with the message above.

`bench/results/raw/adr6-concurrency-unlocked.json` is that run, produced by
`scripts/probe_concurrency.py` — a bounded diagnostic beside G1.4, never a
substitute for it, and weaker in four stated ways.

**No count from that arm is quoted here, deliberately.** It is a race: how many
processes happen to find the lock free varies from run to run, and three
successive runs of the identical command recorded 184, 186 and 193 failures out
of 200. Pinning any of them in this document would dress a coin toss as a
measurement — and would make the ADR wrong again the next time the probe is
run. What is stable is the shape, and the shape is what the argument needs: the
second process is not contended out for a bounded time, it is not admitted at
all, so no amount of retrying changes the outcome. The locked arm below is the
reproducible one, and it is the one carrying weight.

This was found while designing the `workspace` slice, and it is why that slice
stopped: eight working trees that cannot be used concurrently would be the
appearance of the feature without the property the gate exists to measure.

## Decision

### 1. One writer at a time, by an explicit repository lock

A `Repo` takes an exclusive lock on `.lattice/lock` before it opens the store or
the op-log, and holds it until it is dropped.

This is **mutual exclusion, not concurrency**, and the honesty of that word
matters: two commands against one repository do not run at the same time, they
queue. What the gate measures is that 10,000 operations from 8 workspaces
produce no corruption, no deadlock, and a linearizable op-log — none of which
requires them to overlap, and the first of which is far easier to guarantee if
they do not.

Measured on the same probe, the same pool, the same seed:

```json
{
  "workers": 8,
  "ops_attempted": 200,
  "ops_succeeded": 200,
  "failures": 0,
  "linearizability_violations": 0,
  "unsequenced_operations": 0,
  "verify_errors": 0
}
```

That is `bench/results/raw/adr6-concurrency-locked.json`.

### 2. The lock waits, and the wait is bounded

`std::fs::File::lock` blocks with no deadline. A caller that waits forever
cannot tell a busy repository from a deadlocked one, and G1.4 records any
operation that outlives its 120-second watchdog as a **deadlock** — which would
be a false report about an engine that was merely queueing. So the wait is
polled with capped exponential backoff against a 60-second deadline, comfortably
inside that watchdog, and expiry is an error the engine raises itself:

> another command has held this repository for longer than 60 seconds

`Error::Busy` is its own variant rather than an `Io`. Nothing failed and nothing
is damaged; the repository was in use, which is a state the model allows. Its
recovery says to let the other command finish — advice that no other error's
recovery text gives, which is the test for whether a variant earns its place.

### 3. std, not a dependency

`File::lock`, `try_lock` and `unlock` were stabilised in Rust 1.89;
`rust-toolchain.toml` pins 1.96.0. They are `flock(LOCK_EX)` on Unix and
`LockFileEx` on Windows, so G1.12 gets one implementation on all three
platforms and G1.13 gets no new dependency to license-scan.

**This raises the workspace MSRV from 1.80 to 1.89**, which is a real cost and
is paid deliberately: the alternative was a third-party locking crate, and a
version floor is cheaper than a dependency in a project whose licence
compatibility is itself a gate. The floor is set to what the code needs, not to
the 1.96 CI pins — those are different facts and conflating them would hide a
compatibility change inside a toolchain bump.

The alternative was a custom `redb::StorageBackend` that takes a blocking lock
instead of redb's non-blocking one — the seam exists, and this repository
already uses a custom backend to inject sync failures in tests. It is rejected
as **more mechanism for the same answer**: it would put our locking policy
inside a trait implementation whose other five methods exist to do something
else entirely, and it would still leave the repository — packs included, not
just the redb file — without a lock of its own.

### 4. The lock is released by the operating system, never by us

Dropping the file releases it, and so does the process exiting for any reason,
including `SIGKILL`. That property is the reason to lock a file rather than to
write a lockfile containing a PID: a stale lock left by a crashed process is a
repository nobody can open again, which is a worse failure than the contention
it was guarding. G1.1 kills processes at arbitrary points by design, so this is
not a hypothetical.

## Consequences

- **`workspace` can now be designed.** Its ADR inherits a repository that
  several processes may address, and needs to answer only what a workspace *is*
  — not whether concurrent access works at all.
- **Throughput is serial, and unmeasured at scale.** 200 operations across 8
  workers took 10.3 s wall clock on the reference machine (`wall_clock_s` in the
  locked artifact). The unlocked arm is far quicker only because it did almost
  none of the work, so the two are not comparable and no speed claim is made
  from them. G1.4's real shape is 80,000 operations over a tree that grows to
  ~80,000 files. **Whether that fits any time budget was left unanswered here;
  it has since been measured, and the answer is no** — see the section below.
  ADR-4's 6.3-minute figure was about fsync, which group commit addresses within
  a process; it says nothing about what a command costs before it ever reaches
  the log.

## Measured afterwards: G1.4 does not fit, and the lock is not why

`scripts/probe_scaling.py` answers the question this ADR declined to.
`bench/results/raw/adr6-scaling.json` is the run:

```json
{
  "files": 10000,
  "first_save_s": 0.23,
  "incremental_save_s": 4.184,
  "status_s": 13.121
}
```

**The ratio carries the argument, not the absolute.** These are wall-clock
timings on one shared machine and they move between runs: the attribution below
records 11.524 s for the same `status` this run puts at 13.121 s, on the same
machine and the same tree size. Both are committed, and the gap between them is
the reason no absolute here is worth arguing about. What is stable is the shape,
across every run and both arms: the first save of a ten-thousand-file tree is a
fraction of a second, the **next** save — changing one file — is an order of
magnitude more for a fraction of the work, and `status`, which saves nothing at
all, is slower still.

So the cost is not the tree walk, and it is not the repository lock either.

**A checkpoint's identity is not its storage address.** `Checkpoint::body_id`
hashes `(tree, message, parent, at_unix_ms)`; the blob is stored under the hash
of the whole serialised struct. Nothing maps one to the other, so finding a
checkpoint means reading and deserialising every chunk in the store until one
matches. The source says so where it happens — "a checkpoint is
content-addressed like everything else, but its own address is over its body
rather than its serialised form, so the lookup is by scanning the addresses we
know ... a checkpoint index is a later refinement."

Timing each command separately puts it beyond doubt. `probe_scaling.py
--attribute` does exactly that, and `bench/results/raw/adr6-attribution.json` is
the run:

```json
{
  "files": 10000,
  "samples": 3,
  "internals_oplog_s": 0.035,
  "line_list_s": 0.036,
  "log_forensic_s": 7.782,
  "status_s": 11.524
}
```

What is indexed is fast and what is scanned is not, with nothing in between.
`internals oplog` and `line list` answer from redb and cost tens of milliseconds
whatever the repository holds. `log --forensic` goes through `checkpoints()`,
which reads every blob, and costs two hundred times as much. `status` calls
`head_checkpoint` and `checkpoints()`, so it pays twice and is the slowest
command in the product — while writing nothing and walking no tree.

`save` pays it too, through `head_checkpoint`. A second cost, of a different
shape, sits in `PackWriter::retain_unknown`: it asks `Store::contains` once per
chunk offered, and `contains` binary-searches the index of every pack. It reads
no payload and decompresses nothing, so it is not a scan of the same kind, and
at the handful of saves this probe makes it is nothing. What it grows with is
the number of packs — and a save writes a pack. At G1.4's 80,000 operations
that is 80,000 index searches per chunk offered, which is why it is worth
fixing even though it is not what dominates here.

<!-- evidence: the `retain_unknown` paragraph is read from the source of Store::contains and PackWriter::retain_unknown, not measured; every number above it is quoted from bench/results/raw/adr6-attribution.json or bench/results/raw/adr6-scaling.json -->

**ADR-8 answers this.** A checkpoint's blob is now stored at the address its id
already named, so the scan is a lookup: `status` at 10,000 files falls from
seconds to tens of milliseconds and stops growing with the tree. What remains in
a save is the tree walk, which is work rather than accident. The paragraphs
below stand as the record of what was measured here and why.

Both predate this ADR and both are acknowledged where they are written ("small
and adequate for the current history sizes; a checkpoint index is a later
refinement"). `bench/results/raw/adr6-scaling-baseline.json` is the same probe
against a binary built from `main`, before the workspace slice, and the two arms
track each other — so the finding cannot be mistaken for a regression from the
lock.

Extrapolating — and this part IS extrapolation, labelled as such in the artifact
— a mean tree of 40,000 files puts an incremental save in the tens of seconds,
which over 80,000 operations is hundreds of hours. The extrapolation is linear
from measured points that are growing *worse* than linearly, so it is a floor
rather than an estimate, and the precise figure is not worth arguing about: no
plausible correction brings it near a budget anyone would accept.

### It is not only G1.4

The same two scans sit under three performance gates, whose targets are in
`harness/gates.toml` and whose reference repo `scripts/corpus/build_reference_repo.py`
describes as "a ~100k-file, ~2 GB-history reference repo":

| Gate | Target | Measured at 10,000 files — a tenth of that repo |
|---|---|---|
| G1.5 `ltx status` p95 | < 100 ms | 13,121 ms |
| G1.6 `ltx save` p95 | < 250 ms | 4,184 ms |
| G1.7 `ltx log` p95 | < 100 ms | shares `checkpoints()` with `status` |

**These are inferences from measurement, not gate results.** None of the three
has been run — they need the reference repo, which is not built in this
checkout — and the figures above are from the scaling probe, not from their
harnesses. What the comparison establishes is the order of magnitude: at a tenth
of the reference repo's size, `status` is already about 130× its target. No
amount of measurement noise closes that.

So the work below is not a tax paid for G1.4 alone. **G1.4, G1.5, G1.6 and G1.7
are all waiting on the same two indexes**, which is worth knowing before
deciding what to build next.

### What is owed

**G1.4 cannot be claimed until this is addressed**, and addressing it is its own
slice with its own ADR. The shape is not open: the op-log already indexes
checkpoint id → op-log sequence in its `SAVED` table, written in the same
transaction as the `Save` that records it. One more column there — checkpoint id
→ the chunk address its blob is stored at — turns every one of these scans into
a lookup, at the cost of one key per checkpoint. That it is not decided here is
deliberate; it is not a concurrency question.
- **Readers are excluded too, and need not be.** `ltx log`, `status` and
  `change list` mutate nothing, and could hold a shared lock — but redb takes an
  exclusive lock on its own file regardless, so a shared lock here would buy
  nothing without also replacing redb's backend. Recorded as the first thing to
  revisit if the serial cost above proves to be the constraint.
- **The daemon's role is unchanged, and ADR-4's commitment survives.** Nothing
  here is unavailable daemonless. A daemon would let commands share one process
  and so one lock, which is exactly the amortisation ADR-4 describes — it makes
  the same operations faster, not more possible.
- **Two `Repo` handles to one repository inside one process now deadlock**
  against each other until the wait expires. That is a real footgun for callers
  and for tests, and it is bounded rather than eternal for exactly that reason.

## Open conflicts recorded, not resolved

1. **The op-log is a chain, and two peers cannot both extend it.**
   `docs/prior-art/mercurial-evolve.md` §Trap 8 argues the log must be a DAG,
   because "two workspaces, or a peer sync, produce two heads that cannot both
   extend one chain". This ADR does not settle that: it makes local processes
   take turns, so on one machine the chain stays linear and totally ordered by
   lock acquisition, which the probe confirms. **Sync is where the DAG question
   actually bites**, and it is not answered here.
2. **Undo scope under concurrency**, inherited from ADR-16 open conflict 2 and
   ADR-17 open conflict 2. Serialising commands does not make the LIFO lemma
   hold across workspaces: whose operation `ltx undo` reverses, when eight
   working trees share one log, is a question about the *model*, not about
   locking. It belongs to the workspace slice, which can now ask it.
