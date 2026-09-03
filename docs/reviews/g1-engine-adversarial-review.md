# G1 engine — adversarial review and remediation

Before merging the engine (PR #3), the ~3,200 lines of storage and durability
code — reviewed until then only by their own unit tests and one CodeRabbit pass —
were put through a seven-lens adversarial review (data-loss ordering,
group-commit, content integrity, bounds/arithmetic, path/platform, error
handling, spec compliance). Each candidate finding was then independently
verified by a skeptic prompted to *refute* it.

**24 findings were confirmed** (6 critical, 11 high, 7 medium). This document
records what each was, how it was fixed, and — for the ones deliberately left —
why.

The central promise under attack: **no checkpointed data is ever lost, and no
byte is ever silently altered.**

## Fixed in this PR

Every fix ships with a regression test named as a sentence.

### Critical

| # | Finding | Fix |
|---|---|---|
| 1 | `write_head_pointer` renamed HEAD without fsyncing the temp file's contents; a crash left a zero-length HEAD (bricked save/status) or 64 zero bytes (silently orphaned history, exit 0). The durable redb head that could recover it had no callers. | fsync the temp file before rename; `head_checkpoint` falls back to the durable op-log head when the file is missing/empty/torn. |
| 2 | `checkpoint()` trusted the `id` field *inside* a blob, so an ordinary file could impersonate a checkpoint. | Identity is the hash of the body (tree, message, parent, timestamp); a blob is a checkpoint only if its declared id hashes its body **and** the op-log references it. |
| 3 | `Store::read` aborted on the first corrupt copy and never consulted an intact duplicate in an older pack. | On a pack-level error, remember it and keep searching; return a recovered intact copy if any exists, surface the error only if none does. |
| 4 | Index count read with unchecked arithmetic; corruption wrapped past the length check and then panicked during a lookup. | Checked arithmetic + exact-length validation; a garbage count reports `Corrupt`. |
| 5 | Checkout joined stored names onto the destination with no validation — `..` or an absolute name escaped it (arbitrary file write from a hostile repo). | Entry names must be a single safe path component; anything else is refused and reported, never written. |
| 6 | `snapshot_dir` skipped `.lattice` at **every** level, silently dropping any nested user directory named `.lattice`. | Skip only at the repository root. |

### High

| # | Finding | Fix |
|---|---|---|
| 7 | A torn index made `Store::open` fail, bricking the whole repository after a crash mid-index-write. | An unparseable index is treated as crash residue and skipped, like a missing one. |
| 9/10 | Group commit: a failed batch livelocked siblings in an empty-batch spin, and a later commit could flip a sibling's `append()` to `Ok` for an entry that never landed, permanently breaking the Merkle chain. | A failed commit **poisons** the log — this and every in-flight/future append fails honestly; the process exits and reopens from durable state with a continuous chain. A `FlushGuard` covers a panicking commit. |
| 11 | `verify` reported a clean, fully-verified repository when a checkpoint's tree blob (or the whole checkpoint) was missing. | A missing tree is an error; every Save the op-log records must resolve to a present, authentic checkpoint. |
| 8/12/13/15/20 | Corrupt pack-tail / index counts drove allocations and loops → capacity-overflow panics. | Every count and segment extent is validated against the real file size before use. |
| 14 | `Store::read` refused an intact duplicate when a newer pack's copy was corrupt. | Same fix as #3. |
| 16 | No cross-pack dedup: every save re-stored the entire tree; a one-byte edit re-persisted everything (blows G1.8/G1.9). | `save()` filters the pack against content the store already holds. |
| 17 | Checkout into a populated directory reported every pre-existing file as a fold collision and skipped it — silent partial checkout of whole subtrees. | A collision is only a pre-existing path that a *different* placed name folds together with; unrelated files are overwritten (the point of a checkout). |

### Medium

| # | Finding | Fix |
|---|---|---|
| 18 | `verify --complete` printed "verified…" even when the report carried errors, which prose mode never showed. | Unhealthy reports render as "NOT verified" with the problems listed; success sentences print only when healthy. |
| 19 | `save` wrote two blobs sharing one id; `checkpoint()` could return the pre-sequence one. | Single durable write; `oplog_seq` resolved from the op-log at read time. |
| 21 | `verify_chain` sliced `prev`/`id` at `[..12]` and **panicked** on a tampered field shorter than 12 bytes — the very tampering it exists to report. | Safe slicing. |
| — | Recovery text advertised `undo`, `sync`, `adopt` — commands the binary lacks — so following the advice hit "unrecognized subcommand". | Recovery text references only implemented commands; a test enforces it. |
| — | `CheckoutReport.entries_written` was never assigned; every checkout reported "0 entries". | `restore_tree` increments the report directly. |
| — | Unnecessary `unsafe impl Send/Sync for OpLog` disabled auto-trait checking. | Removed; auto-derivation holds. |

## Deliberately deferred (with rationale)

None of these is a data-loss or silent-corruption defect on the platforms this PR
is validated on (macOS, Linux). They are tracked for follow-up.

- **Windows filename WTF-8 round-trip** (`platform.rs`): `os_string_from_bytes`
  rejects non-UTF-8 names on Windows, so a name Windows itself stored via
  `as_encoded_bytes` (WTF-8) cannot be restored there. The correct fix uses a
  WTF-8-aware decode (`from_encoded_bytes_unchecked` guarded by validation) that
  would be `unsafe` and cannot be exercised on macOS/Linux. The current behaviour
  is **safe** — names are refused and reported, never silently mangled. Deferred
  to a Windows-tested change. Same for the Windows relative-symlink type and
  reserved-device-name findings.
- **Op-log tail-truncation attestation**: `verify_chain` walks surviving entries
  and cannot detect a lost tail on its own. This is mitigated by redb, which
  ADR-3 chose precisely because it makes the metadata write crash-atomic; a
  separate length attestation would duplicate that guarantee. Revisit if the
  op-log ever moves off redb.
- **ADR-2 compression parameter exactness** (`store.rs`): per-segment zstd
  level 3 is a faithful realization of ADR-2's segmented scheme; whether it hits
  the measured ratio is a G1.9 *measurement*, not a correctness bug, and G1.9
  will measure it.
- **Challenge 12 enforcement at the CLI surface**: `Operation::is_undoable()`
  already excludes redact/thin and is tested, but there are no `redact`/`thin`/
  `undo` commands yet to enforce it at. Enforcement lands with those commands in
  the next G1 phase.
- **§8 / §4.2 audits** (CLI-logic, noun cap, jargon): measured by G5.5/G2.x
  later; not correctness.
- **Checkpoint lookup performance**: `checkpoint()` scans all chunk addresses.
  Correct but O(store); a checkpoint index is a documented later refinement.

## Deferred after CodeRabbit's review of the remediation (tracked follow-ups)

CodeRabbit reviewed the remediation and raised further findings. The genuinely
open bugs were fixed (destination-symlink traversal / CWE-59, the mode-loss
report text, the §8 checkout policy, duplicate tree names, tree-size bound,
char-boundary id slicing, the poison test seam, the toolchain-action pin, and a
merge-gate bug that had made the finding count silently zero). The following were
deferred with the user's explicit sign-off, and are tracked here:

- **Windows crash-durability protocol** (`platform.rs`, Critical): `sync_dir` is a
  no-op on Windows, so an acknowledged rename is not guaranteed durable across a
  power loss the way it is on Unix. A correct Windows commit protocol needs
  Windows-specific work (not `FlushFileBuffers` on a directory handle, which is
  not a guaranteed barrier) and a Windows host to validate. **Follow-up: a
  Windows-tested durability change.**
- **Windows symlink kind** (`platform.rs`, Critical): `symlink()` picks file-vs-dir
  from `target.is_dir()` resolved against the process CWD, which can mis-create a
  relative or dangling directory link. The fix stores the kind in `Node::Symlink`
  — a tree-format change affecting all platforms — so it is deferred to the same
  Windows-tested effort.
- **Windows folded-sibling detection** (`platform.rs` / `repo.rs`, Critical):
  `file_identity` is inode-based and returns `None` on Windows, so fold detection
  (and thus the no-silent-overwrite guarantee) is Unix-only for now; the
  regression test is `#[cfg(unix)]`. **Follow-up: a Windows file-identity
  implementation and matching test.**
- **Cross-pack dedup trusts the content-address index** (`store.rs`, Critical):
  by design — the standard content-addressed contract (git/restic/borg). Re-reading
  every candidate on each save is O(content) and blows the G1.5 latency budget;
  post-write bit-rot is surfaced by `verify` and recovered by refetch. Documented
  at `PackWriter::retain_unknown`.
- **Streaming save/checkout and a segment cache** (`store.rs`, Major, perf): save
  and checkout buffer whole files, and checkout re-inflates a segment once per
  chunk. The 1.1 GB case passes today; these are optimisations the G1.5/G1.9
  gates will measure, per "optimise only a measured hotspot".
- **Checkout no-follow / TOCTOU hardening** (`repo.rs`, Major): the practical
  traversal vectors are closed — stored names must be single safe components,
  a symlinked destination is refused, and any pre-existing symlink at an entry
  path is removed before writing. The remaining vector is a local TOCTOU race
  (a component swapped for a symlink between the no-follow check and the write),
  whose complete fix is `O_NOFOLLOW` / `openat2(RESOLVE_NO_SYMLINKS)`-relative
  writes — OS-specific facilities not in `std`. **Follow-up: a no-follow write
  path (likely via `cap-std` or raw `openat`).** Ancestors of `dest` are
  deliberately not validated: `dest` is a path the user chose (`--into`), and
  its ancestors resolve as the user's own filesystem dictates (on macOS `/tmp`
  is itself a symlink), so rejecting symlinked ancestors would break ordinary
  checkout. The protections guard the checkpoint-controlled paths written
  beneath `dest`, not the user's choice of `dest`.
- **§4.2 seven-noun vocabulary in CLI text** (`main.rs`, Major): the vocabulary
  lint is gate G2.3's scope, with its own harness; the cleanup lands there rather
  than being pre-empted in the engine PR.
- **G1.2 fold-coverage measured from a committed manifest** (`harness/g1/…`,
  Major): the harness is frozen (§0.3); changing it to probe the live filesystem
  is a harness amendment, not an engine change. ADR-14 records the current
  manifest-driven approach.

## What the exercise confirmed

The most serious finding — the un-fsynced HEAD — was reproduced empirically, and
both of its crash shapes (bricked, and silently-orphaned-at-exit-0) are now
covered by a test. Two of the confirmed bugs (checkpoint impersonation, the
group-commit chain break) were latent data-integrity holes invisible to the
existing tests. This is why the engine was reviewed before merge rather than
after: the foundation every HARD gate stands on had defects its own tests could
not see.
