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
pool, 186 of 200 operations failed on the lock:

```json
{
  "workers": 8,
  "ops_attempted": 200,
  "ops_succeeded": 14,
  "failures": 186,
  "linearizability_violations": 0,
  "verify_errors": 0
}
```

That is `bench/results/raw/adr6-concurrency-unlocked.json`, produced by
`scripts/probe_concurrency.py` — a bounded diagnostic beside G1.4, never a
substitute for it, and weaker in four stated ways.

**That count is not reproducible, and nothing here rests on its exact value.**
It is a race: how many processes happen to hold the lock at a moment when no
other holds it varies from run to run, and an earlier run of the same command
recorded 16 and 184. What is stable across runs is the shape — the
overwhelming majority fail, every failure is the same lock error, and no amount
of retrying by the harness would change it, because nothing is contended for a
bounded time; the second process simply is not allowed in. The locked arm below
is the reproducible one.

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
  workers took 10.9 s wall clock on the reference machine, against 0.5 s for
  the unlocked arm that did almost none of the work — so the two numbers are
  not comparable and no speed claim is made from them. G1.4's real shape is
  80,000 operations over a tree that grows to ~80,000 files, where each `save`
  walks and hashes the whole tree; **whether that fits any time budget is not
  answered here and must be measured before G1.4 is claimed.** ADR-4's 6.3-hour
  figure was about fsync, which group commit addresses within a process; it says
  nothing about a per-command lock held across a full tree walk.
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
