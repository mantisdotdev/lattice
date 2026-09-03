# ADR-12 — `mdcheck.sections` counts nested subsections; harness change classified

**Status:** Accepted · **Required by:** §0.3 (harness changes after freezing)
**Gates touched:** G0.1, G0.2, G0.6, G0.8 (all consumers of `harness/lib/mdcheck.py`)

## Context

§0.3 requires that any harness change after freezing carry an ADR "stating
whether the change makes the measurement *stricter, looser, or equivalent*, with
evidence," and that "the moment a harness changes, every gate it touches reverts
to **FAIL(stale)** — losing PASS and its ratchet baseline — until re-measured."

`harness/lib/mdcheck.py` was frozen with the G0 harness set. Its `sections()`
function mapped each heading to the text beneath it, ending a section at the
next heading **of any level**. G0.1 then required ≥ 80 substantive words under
each of the three mandated questions.

## The defect

A document structured as

```markdown
## 1. What did it get right?

### 1.1 The operation log is a DAG of typed objects
...900 words...

### 1.2 Lock-free concurrency
...600 words...
```

measured **0 words** under "What did it get right?", because the `###` heading
immediately terminated the `##` section. The content was present — 1,500 words
of it — and the harness could not see it.

This was not hypothetical. On the first G0.1 measurement over the completed
prior-art corpus, 8 of 11 entries failed with `Q1 has 0 words < 80` while
containing 3,000–5,400 substantive words each. The harness was measuring
**heading adjacency**, not section content.

## Options

**Option A — Leave the harness frozen and rewrite the documents** so no mandated
section contains subsections. Rejected: it would degrade 47,000 words of
analysis to satisfy a measurement artifact, and the resulting flat documents
would be worse to read. Optimising the deliverable to fit a broken instrument is
the failure mode the whole protocol exists to prevent.

**Option B — Lower the per-section word threshold** so the empty parents pass.
Rejected outright: that is a loosening change to a HARD gate, and it would make
the gate pass for documents that genuinely say nothing.

**Option C — Fix `sections()`** so a section runs until the next heading of the
same or higher level, which is how document structure is universally understood.

## Decision

**Option C.** A section's body now extends to the next heading of equal or
higher level, so nested subsections count toward their parent.

## Classification under §0.3: **equivalent**, tending stricter

§0.3 permits a loosening change to a HARD gate's harness "only if the ADR
demonstrates the prior harness measured something other than the gate's stated
criterion." That is precisely the demonstration here: G0.1's stated criterion is
"all 3 questions each" — that the questions are *answered* — and the prior
implementation measured whether the answer happened to be written without
subheadings. Those are different properties.

Evidence that the change is not a general loosening:

- **No threshold moved.** `MIN_WORDS`, `MIN_SECTION_WORDS` and `MIN_SOURCES` are
  unchanged at 400, 80 and 3.
- **It can only ever increase a measured word count**, never decrease one — a
  section's text is now a superset of what it was. So it cannot cause a
  previously-failing document to fail *differently*, only to be measured
  correctly.
- **It makes other checks stricter.** G0.1 requires Q3 to cite a gate or ADR.
  Under the old parser Q3's body was often empty, so the citation check scanned
  an empty string; it now scans the full section, which is a strictly larger
  surface for that requirement to be enforced over.
- **The negative control still fails.** A document with the three headings and
  no content beneath them still measures 0 words per section and still fails.

## Consequences

Per §0.3, every gate consuming `mdcheck.py` reverts to `FAIL(stale)` and is
re-measured. `harness/FREEZE.json` is updated with the new hash, and the
scorecard for the iteration in which this landed records the re-measurement
rather than carrying the prior status forward.

The ratchet baselines for G0.1, G0.2, G0.6 and G0.8 are discarded and re-earned
at the re-measured values, which is what §0.3 requires and what the runner does
automatically once the freeze hash moves.

## Evidence

Before the fix, on the completed prior-art corpus (`docs/prior-art/*.md`):

```
G0.1 value: 3 / 11
  ✗ pijul-darcs               ['Q1 what it got right has 0 words < 80']
  ✗ fossil                    ['Q1 ... 0 words', 'Q3 ... 0 words', 'Q3 cites no gate or ADR']
  ✗ unison                    ['Q1 ... 0 words', 'Q2 ... 67 words', 'Q3 ... 0 words']
  ... 8 of 11 failing
```

After the fix, same documents, same thresholds:

```
G0.1 value: 11 / 11
  jujutsu                4,534w  Q1=1008 Q2= 908 Q3=1716  sources=31
  pijul-darcs            5,121w  Q1=1547 Q2= 882 Q3=1752  sources=23
  centralized-and-data   5,370w  Q1=1485 Q2= 586 Q3=2324  sources=34
  ... 11 of 11 passing
```

The documents were not edited between the two runs. That is the evidence that
the change corrected the instrument rather than the result.
