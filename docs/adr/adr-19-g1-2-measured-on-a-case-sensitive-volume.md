# ADR-19 — G1.2 is measured on a case-sensitive volume on the reference machine

**Status:** Accepted · **Amends:** ADR-14 (its "measured on macOS, honestly partial" conclusion)
**Gates touched:** G1.2 (byte fidelity, HARD)

## Context

ADR-14 established that a filesystem name-fold is a **coverage gap**, not a
mismatch, and concluded:

> The practical consequence is that **G1.2 can only reach PASS on a
> case-sensitive filesystem**. … G1.12 already requires the suite to be green on
> Linux, so the case is exercised there; the macOS run is honest about being
> partial.

That left G1.2 permanently FAIL on the reference machine (`bench/ENVIRONMENT.md`
records macOS/APFS), reporting 0 mismatches with `coverage.ok = false` — correct,
and never improving.

Deferring the case to Linux CI does not actually close it either: the gate's
value of record is produced on the reference environment, and CI does not build
the 1.1 GB corpus.

## Decision

macOS can host a **case-sensitive APFS volume inside a sparse disk image**, so
the gap closes on the reference machine rather than moving to a different one.
`scripts/corpus/measure_g1_2_case_sensitive.sh` creates that volume, builds the
corpus on it, and measures the gate there.

Three details are load-bearing, and each was a way to get a confidently wrong
result:

1. **Both halves must be case-sensitive.** The corpus *and* the temporary
   directory the harness copies it into and checks out into.
   `g1_2_byte_fidelity.py:146` uses `tempfile.mkdtemp`, which honours `TMPDIR`.
   A case-sensitive corpus copied into a folding temporary directory folds on
   the way in, and the gate reports a phantom mismatch indistinguishable from an
   engine bug.

2. **The volume is mounted over `corpus/data`, not over the corpus directory.**
   The builder does `rmtree(OUT); OUT.mkdir()`, which can remove neither a mount
   point nor a symlink. With the parent mounted, `adversarial` is an ordinary
   subdirectory. The repository's other corpora (26 GB of mined repositories)
   are hidden for the duration and reappear on unmount; nothing is deleted.

3. **The volume is probed before it is trusted.** The script writes `A.txt` and
   `a.txt` and refuses to continue unless both survive. A silently
   case-insensitive volume would produce a confident, wrong PASS — the exact
   failure mode this ADR exists to remove.

## Result

Recorded by the runner, not by hand: **`bench/results/iteration-9.json`**, gate
`G1.2` — `"status": "PASS"`, `"value": 0.0`, `"unit": "mismatches"`, note
`"0 mismatches over 135 checked entries"` — together with the entry it produced
in `bench/ratchet.json`.

The iteration file stores the runner's verdict and the harness's `detail`
block; it does not persist the `coverage` block, which the runner evaluates and
folds into the status. The harness's own output, which is where `coverage` is
visible, is:

```json
{"gate": "G1.2", "value": 0, "unit": "mismatches",
 "note": "0 mismatches over 135 checked entries",
 "coverage": {"ok": true, "note": ""},
 "detail": {"filesystem_case_sensitive": true, "folded_names": [],
            "checked_entries": 135, "pinned_files": 48, "special_entries": 5,
            "source_paths": 82, "checkout_paths": 82,
            "mismatches": [], "mismatches_truncated": 0},
 "evidence": ["corpus/manifests/g1-2-adversarial.json"]}
```

The prior figure it should be read against is not asserted here either. It is
the manifest this PR replaced, `corpus/manifests/g1-2-adversarial.json` at
`9c17d05^`, produced by the same builder on the case-folding volume:

```json
{"total_files": 47,
 "filesystem_case_sensitive": false,
 "folded_names": [{"requested": "names/upper.txt",
                   "occupied_by": ["names/UPPER.txt"],
                   "reason": "this filesystem does not distinguish these names
                              (case folding or Unicode normalisation)"}],
 "skipped_mandated_cases": []}
```

47 files to 48, and `folded_names` from one entry to none: `names/upper.txt`
now exists as a file of its own rather than being folded onto
`names/UPPER.txt`. That single entry is the whole difference between the gate's
coverage contract being satisfied and being reported as un-exercised, which is
why ADR-14 could not close it here.

G1.2: **FAIL → PASS**, with the mandated case-collision requirement actually
exercised rather than reported as un-exercised.

## Consequence: the manifest and the corpus are a matched pair

`corpus/data/` is gitignored; the manifest is the only committed artifact. The
committed manifest now records `filesystem_case_sensitive: true` and 48 files,
so it describes a corpus that **only a case-sensitive volume can hold**.

Running `build_adversarial_corpus.py` directly on a folding filesystem
regenerates a 47-file, `case_sensitive: false` manifest, and G1.2 then reports
the coverage gap exactly as ADR-14 specifies. That is still the correct
behaviour there, and it is not a regression — it is the same honest partial
result, freshly measured.

What must not happen is the **mixed** state: the case-sensitive manifest beside
a case-folded corpus, where `names/upper.txt` is pinned but absent and the gate
reports a mismatch that is neither an engine defect nor a coverage gap. The
stale case-folded corpus is therefore removed when this ADR lands, leaving the
harness to report "adversarial corpus not built" — which is true, and points at
the builder — until the script rebuilds it.

**Reproducing the measurement of record:**

```bash
LTX_CASE_KEEP=1 scripts/corpus/measure_g1_2_case_sensitive.sh
TMPDIR=corpus/data/tmp python3 scripts/gauntlet measure G1.2
hdiutil detach corpus/data
```

## Classification under §0.3: **no harness change**

`harness/g1/g1_2_byte_fidelity.py` is untouched and still matches its freeze
hash. Only the environment the harness runs in changed, and it changed toward
the one the gate's own text requires. No threshold moved; the target is still
0 mismatches, and the coverage contract is now satisfied rather than waived.
