# ADR-4 — The daemon is an accelerator, never a requirement

**Status:** Accepted · **Stage:** G0.6 · **Constrains:** G1.5–G1.7, G4.6, G5.4, §5.5

## Context

§7 asks: "Daemon: mandatory for perf targets or optional accelerator — what
degrades without it?" `docs/DISAGREEMENTS.md` Challenge 5 observes that the
question was posed and no gate answered it: G1.5 states its target "(watcher
resident)" and the other timing gates state theirs without qualification, so
Lattice could pass every latency gate with a resident daemon and be unusable
without one, with an entirely green scorecard.

That is not a hypothetical risk. Two independent costs push work toward a
resident process. Continuous auto-snapshotting (§5.4) is watcher-driven; without
a watcher, `ltx status` on a ~100k-file repository must walk the filesystem.
And every state-changing command must make an operation-log record durable before
reporting success, which we have now measured rather than assumed.

## Options

**Option A — Daemon mandatory.** Simplest engineering: one code path, warm
caches, group-committed op-log. Costs adoption exactly where §5.6 says the wedge
matters — CI containers, SSH sessions, locked-down corporate endpoints, Windows
service policy. A developer who must install a background service inside a Git
team no longer has "zero coordination."

**Option B — No daemon.** Every command cold. Fails G1.5 outright on the
reference repo and abandons the event subscription §5.5 requires for agents.

**Option C — Daemon as accelerator, with a hard floor on the daemonless path.**
Every operation is available without the daemon. The daemon supplies the watcher,
warm indexes, and op-log group commit. Two code paths, one of which is slower.

## Decision

**We will build Option C, with three commitments that make "accelerator" mean
something enforceable rather than aspirational:**

1. **No command may be unavailable without the daemon.** Availability is not a
   performance property, and a command that requires a background service is a
   command that fails in a container.
2. **Any command more than 5× slower daemonless must say so in its own output**,
   naming the daemon as the remedy. G2.4 already requires every error path to
   carry a recovery action, a causal category, and the §4.2 concept involved,
   machine-checked; this extends the same check to degraded-performance paths.
3. **Every timing harness emits both numbers.** G1.5, G1.6, G1.7, G4.6 and G5.4
   each report `p95_daemon_resident` and `p95_daemonless`. The gate's pass/fail
   remains governed by the spec's stated target applied to the daemon-resident
   figure — re-scoping a gate upward on our own authority is as forbidden as
   re-scoping it downward — but the daemonless number appears in every scorecard,
   so a catastrophic degradation is a finding the stakeholder sees rather than
   one the scorecard hides.

## Consequences

**Positive.** The adoption wedge survives: `ltx adopt` in a CI container works.
The daemonless path being *measured* means it stays honest; an accelerator that
is secretly mandatory shows up as a number, not as a support ticket.

**Negative, and accepted.** Two performance paths means two things to keep
correct, and the daemonless path will be the one that rots if we let it. The
mitigation is that it is gated: it appears in every scorecard, and §0.4's ratchet
applies to any metric a gate reports.

**Consequence for the op-log design.** The measurement below shows a single
durable append costs ~4.7 ms with true media-flush durability, and that this cost
is irreducible per flush but amortises ~25× under group commit. Therefore: the
daemonless path takes the full ~5 ms per command (acceptable — 5% of G1.5's
100 ms budget, 2% of G1.6's 250 ms), and the daemon's op-log performs **group
commit** across concurrent workspaces. This is what makes G1.4 tractable at all:
8 workspaces × 10,000 operations at 4.7 ms of unamortised flush each would be
6.3 hours of pure fsync. This constraint is handed to ADR-6 (op-log concurrency
control), which must specify group-commit semantics that preserve linearizability.

## Evidence

**Measured, this repository.** `bench/results/raw/adr3-fsync-bench.json`,
produced by `prototypes/fsyncbench`, 200 records × 256 bytes, on the reference
machine recorded in `bench/ENVIRONMENT.md`:

| Variant | mean µs | p50 | p95 | p99 |
|---|---:|---:|---:|---:|
| append + `F_FULLFSYNC` | 4,688 | 4,078 | 7,037 | 17,306 |
| pre-allocated + `F_FULLFSYNC` | 5,198 | 5,045 | 7,021 | 8,161 |
| append + `fsync(2)` | 63.6 | 33.4 | 181.8 | 526.3 |
| pre-allocated + `fsync(2)` | 59.9 | 30.3 | 132.0 | 887.0 |
| pre-allocated + `F_FULLFSYNC`, group of 8 | 675.2 | 7.5 | 4,837 | 6,297 |
| pre-allocated + `F_FULLFSYNC`, group of 32 | 190.7 | 4.1 | 110.0 | 5,964 |
| no durability (control) | 9.2 | 1.6 | 8.8 | 65.8 |

Three findings, two of which contradicted the hypothesis they tested:

- **Pre-allocation does not help.** The hypothesis that file extension forces the
  metadata flush is *refuted*: pre-allocating made `F_FULLFSYNC` marginally
  slower, not faster. The cost is the media flush itself.
- **`fsync(2)` on macOS is 78× faster and is not crash-safe.** It returns once
  the write reaches the drive's volatile cache. G1.1 requires surviving
  "simulated power-loss cases via an I/O fault-injection shim that drops,
  reorders, and tears un-fsynced writes," so `fsync(2)` is not available to us on
  this platform and `F_FULLFSYNC` is mandatory. This is the single most important
  number in this ADR: the honest floor on a durable Lattice command is
  milliseconds, not microseconds, and any design that assumed otherwise was
  assuming a durability guarantee it did not have.
- **Group commit amortises it 25×.** p50 falls to 4.1 µs at a group of 32, which
  is what makes concurrent workspaces viable.

**Related.** `bench/results/raw/adr3-store-bench.json` (backend comparison,
ADR-3). `docs/DISAGREEMENTS.md` Challenge 5 (the gap this ADR closes).
Apple's `F_FULLFSYNC` semantics: <https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fsync.2.html>.
PostgreSQL's analysis of the same problem, including why group commit is the
standard answer: <https://www.postgresql.org/docs/current/wal-configuration.html>.
