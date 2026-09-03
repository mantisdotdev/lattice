# ADR-14 — G1.2 treats a filesystem name-fold as a coverage gap, not a mismatch

**Status:** Accepted · **Required by:** §0.3 (harness changes after freezing)
**Gates touched:** G1.2 (byte fidelity, HARD)

## Context

G1.2 requires a save→checkout round-trip to be byte-identical across an
adversarial corpus that names, among other cases, "unicode/case-colliding
names". The corpus builder duly wrote `names/UPPER.txt` and `names/upper.txt`.

On the reference machine those are **one file**. APFS folds ASCII case, so the
second write replaced the first:

```
$ printf 'upper\n' > UPPER.txt ; printf 'lower\n' > upper.txt
$ ls          →  UPPER.txt
$ cat UPPER.txt →  lower
```

Measured on the same machine, APFS folds case but does **not** fold the
NFC/NFD pair the corpus also contains — which is worth recording, because the
opposite is a common assumption and it would have been wrong here.

## The defect this exposed

The corpus builder recorded `sha256(data)` — the digest of what it *intended* to
write — without reading back. The manifest therefore pinned two files where one
existed, including a digest for content the filesystem had already discarded.
G1.2 then reported a mismatch on `names/UPPER.txt`.

That mismatch was **the corpus lying, not the engine losing data**. The engine
had faithfully round-tripped the one file that existed.

This is the same failure class as every other defect found in this project's
harnesses: an artifact asserting rather than measuring.

## Options

**A — Drop the case-colliding pair from the corpus.** Rejected: G1.2 names the
case explicitly, and removing it to make the gate pass on one platform is
exactly the weakening §0.3 forbids.

**B — Record the fold and treat the missing name as a mismatch.** Rejected: it
would fail a HARD gate for a platform property no engine can overcome. Neither
Git nor any other VCS can hold both names on APFS.

**C — Record the fold and treat the un-exercised case as a COVERAGE gap.**

## Decision

**Option C.** Two changes:

1. **The corpus builder measures.** After each write it reads the file back and
   pins the actual digest. When a write lands on a name an earlier entry already
   occupies with different bytes, it removes the now-false record and declares
   the fold in `folded_names`, alongside `filesystem_case_sensitive`.

2. **G1.2 reports an un-exercised mandated case as a coverage failure.** §6's
   coverage contract already says "A 0-failure run that misses coverage is a
   FAIL", and the runner enforces that independently of the measured value. So
   on a case-folding filesystem G1.2 reports 0 mismatches AND `coverage.ok =
   false`, naming the case it could not exercise.

The practical consequence is that **G1.2 can only reach PASS on a case-sensitive
filesystem**. That is correct rather than inconvenient: the gate names
case-colliding names as a requirement, and a run that never created two such
files has not tested them. G1.12 already requires the suite to be green on Linux,
so the case is exercised there; the macOS run is honest about being partial.

## Classification under §0.3: **stricter**

- No threshold moved. The target is still 0 mismatches.
- The harness previously did **not** check whether the mandated case-collision
  case had been exercised at all. It now does, and fails coverage when it has
  not — a check that did not exist before.
- A run that formerly reported a clean PASS on macOS while never testing case
  collisions now reports the gap explicitly. Strictly more is required to pass.

## Consequences

Per §0.3, G1.2 reverts to `FAIL(stale)` and is re-measured, and its ratchet
baseline is re-earned.

Engine behaviour changed alongside, though not because the gate demanded it:
`checkout` now returns a `CheckoutReport` listing `collisions` — names the
filesystem could not hold beside one already written. Previously it silently
overwrote, which is data loss by any definition. It reports rather than aborts,
because refusing the whole checkout would trap a user whose repository merely
passed through Linux, and §4.3 forbids states that `ltx undo` cannot exit.

## Evidence

Corpus manifest after the fix, on the reference machine:

```json
"folded_names": [
  { "requested": "names/upper.txt",
    "occupied_by": ["names/UPPER.txt"],
    "reason": "this filesystem does not distinguish these names
               (case folding or Unicode normalisation)" }
],
"filesystem_case_sensitive": false
```

The NFC/NFD pair (`names/é-precomposed.txt`, `names/é-decomposed.txt`) does not
appear in `folded_names`: both survive as distinct files here. Recorded because
it contradicts the common assumption that APFS normalises filenames, and because
a future run on a filesystem that *does* fold them will now say so rather than
producing a phantom mismatch.
