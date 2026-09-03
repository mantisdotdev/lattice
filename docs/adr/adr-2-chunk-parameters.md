# ADR-2 — Chunk parameters and pack strategy

**Status:** Accepted (initial; §7 mandates revisiting after G1.9) · **Stage:** G0.6
**Constrains:** G1.8, G1.9, §5.1

## Context

§5.1 requires that **all** file content pass through content-defined chunking,
and asks for chunk min/avg/max and pack/compression strategy to be tuned "by
benchmark against G1". Two gates set the bar, and they pull against each other:

- **G1.8** — a 1-byte edit at a random offset in a 50 MB file must persist
  **< 256 KiB** of new data (p95). This wants *small* chunks: the new data is
  bounded by the size of the chunks the edit invalidates.
- **G1.9** — the store must be ≤ 1.25× **restic** on a binary corpus and
  ≤ 1.5× a **`git gc --aggressive`** pack on the reference repo's text. The text
  half of this is the hard one, and `docs/DISAGREEMENTS.md` Challenge 9 bet, in
  advance, that it was reachable while keeping §5.1's all-content-through-CDC
  constraint.

The naive expectation is that these trade directly: smaller chunks give better
edit locality and worse storage overhead, because per-chunk index and framing
cost is amortised over less payload. The measurements below show that
expectation is right about *chunking* and wrong about *the store*, and the
difference between those two things is what this ADR is about.

## Options

**Option A — Large chunks (restic-like, ~1 MiB average), per-chunk
compression.** Minimises chunk count and index size. Fails G1.8 immediately: a
1-byte edit invalidating a 1 MiB chunk persists ~1 MiB, four times the budget.

**Option B — Small chunks, per-chunk compression.** Satisfies G1.8 comfortably.
Measured against `git gc --aggressive`, it loses catastrophically — best case
**2.83×**, worst **6.75×** — because each chunk is compressed in isolation and
therefore cannot exploit the redundancy between one version of a file and the
next. That redundancy is exactly where git's 26.9× compression comes from.

**Option C — Small chunks, whole-pack compression with a long window.**
Measured at **0.51×** — the store is *half* the size of an aggressive git pack.
But a solid pack must be decompressed from its start to reach any chunk, so a
random chunk read becomes O(pack size). Unusable for pattern B of ADR-3.

**Option D — Small chunks, pack compressed in bounded independent segments.**
The compromise, and the point of measuring rather than arguing: a segment is
independently decompressible, so a random read costs at most one segment, while
long-range matching *within* a segment still captures cross-version redundancy.

## Decision

**We will build Option D, with initial parameters:**

| Parameter | Value |
|---|---|
| chunker | FastCDC (`fastcdc` crate, v2020 variant) |
| min chunk | **2 KiB** |
| average chunk | **8 KiB** |
| max chunk | **32 KiB** |
| pack compression | zstd, long-distance matching, `window_log = 27` |
| pack segment | **4 MiB** of chunk payload per independently-decodable frame |
| chunk ordering within a pack | grouped by source path, then size |

Three choices deserve their reasoning stated, because each was made against a
number rather than a preference:

**Max chunk = 32 KiB is set by G1.8, not by storage.** A 1-byte edit invalidates
the chunk containing it plus, at worst, the chunks whose boundaries shift — in
practice ≤ 3 chunks. With a 32 KiB ceiling the worst case is ~96 KiB, inside
G1.8's 256 KiB budget with nearly 3× margin. A 256 KiB max chunk (the largest
parameter set measured) would put the worst case at ~786 KiB and fail the gate
outright, regardless of how good its compression ratio looked.

**Chunk ordering is load-bearing and was nearly missed.** The first
implementation of this benchmark extracted blobs into directories named by OID
prefix, which scatters similar content randomly through the pack. Under that
layout the solid pack measured 0.87×. Grouping blobs by their source path — the
same locality heuristic git's pack writer uses — moved it to **0.51×**. The
compressor's window is only useful if related content is inside it, so pack
ordering is part of the format, not an implementation detail.

**4 MiB segments.** At 4 MiB the store is 0.63–0.75× git across every parameter
set, versus 0.51–0.52× for a solid pack — so bounding random-access cost costs
roughly 20-25% of the compression win, and still leaves better than 2× margin
against G1.9(b)'s 1.5× target. At 1 MiB the ratio degrades to 0.71–1.20% and, at
the largest chunk sizes, would approach the gate.

## Consequences

**Positive.** Challenge 9's bet is won on the text half: measured **0.63×** at
the chosen parameters against a target of ≤ 1.5×. The store is smaller than an
aggressively packed git repository holding identical content, while satisfying
G1.8's edit-locality requirement that git makes no attempt to satisfy. §5.1's
"all content through CDC" constraint survives intact — no small-file exemption
was needed.

**Negative, and accepted.** Reading one chunk decompresses up to 4 MiB. For
sequential checkout this is free (the segment is consumed anyway); for scattered
single-chunk reads it is a real cost, and it lands on `ltx diff` and `ltx trace`.
Mitigation: a decompressed-segment cache in the daemon, and pack ordering that
keeps a file's chunks in one segment. This is called out for G6.3's performance
audit rather than assumed away.

**Negative.** Pack ordering must be preserved when packs are rewritten during
maintenance, or the compression ratio silently degrades over time. This is a
regression the §0.4 ratchet will catch on G1.9, which is the argument for G1.9
being ratcheted rather than measured once.

**Open, and honest about it.** G1.9(a) — the ≤ 1.25× restic comparison on the
binary corpus — is **not yet measured**. The binary corpus was built after this
prototype (5.04 GiB, 1,997 real seed files, four mutation kinds) and restic is
not installed on the reference machine. Small chunks should favour Lattice on
localized-patch mutations and be neutral on re-encodes, but that is a prediction,
not a result, and it is recorded here as a prediction so that the eventual number
can contradict it. §7 mandates revisiting this ADR after G1.9 regardless.

## Evidence

**Measured, this repository.** `bench/results/raw/adr2-git-baseline-django.json`,
produced by `scripts/corpus/git_baseline.py` over the last 400 commits of
`django/django` — 146,344 unique blobs, 2,924 MiB of raw content — with
`git gc --aggressive` (git 2.50.1) as the baseline at **108.5 MiB (26.9×
compression)**:

| min / avg / max | per-chunk zstd | solid pack | 1 MiB seg | 4 MiB seg | 16 MiB seg |
|---|---:|---:|---:|---:|---:|
| 1024 / 2048 / 8192 | 2.83× | 0.55× | 0.71× | 0.63× | 0.59× |
| 2048 / 4096 / 16384 | 3.42× | 0.53× | 0.75× | 0.63× | 0.58× |
| **2048 / 8192 / 32768** | 4.11× | 0.52× | 0.82× | **0.64×** | 0.58× |
| 4096 / 8192 / 32768 | 4.21× | 0.52× | 0.82× | 0.64× | 0.58× |
| 4096 / 16384 / 65536 | 5.08× | 0.51× | 0.93× | 0.67× | 0.59× |
| 8192 / 16384 / 65536 | 5.20× | 0.51× | 0.94× | 0.67× | 0.58× |
| 8192 / 32768 / 131072 | 5.97× | 0.52× | 1.05× | 0.70× | 0.59× |
| 16384 / 65536 / 262144 | 6.75× | 0.52× | 1.20× | 0.75× | 0.61× |

The single most important column comparison is the first two. Per-chunk
compression fails G1.9(b) at **every** parameter set, by between 1.9× and 4.5×.
Pack-level compression passes at **every** parameter set, by between 2.9× and
2.0×. The parameters barely matter; the *compression boundary* is the entire
decision. A project that tuned chunk sizes against per-chunk compression would
have concluded that content-defined chunking cannot compete with git on source
text — which is what Challenge 9 predicted the naive measurement would show, and
what it did show.

**Measured, edit locality** (`bench/results/raw/adr2-source-sweep.json`, 37,026
real source files): p95 new bytes persisted for a 1-byte edit, by max chunk size
— 8 KiB → 5,215 B; 16 KiB → 14,994 B; 32 KiB → 28,705 B; 64 KiB → 47,780 B;
128 KiB → 80,681 B; 256 KiB → 179,109 B. The trend is linear in max chunk size,
which is why max chunk size is the parameter G1.8 actually constrains.

**Measured, the small-file crossover** (same file). Against whole-file zstd with
no chunking, chunked storage costs 1.04–1.14× in the 1 KiB–64 KiB buckets and
1.80× above 1 MiB — the per-chunk overhead §5.1 and §7 warned about. This is real
and is the reason Option B fails; it is also entirely absorbed once compression
moves to the pack. The crossover is therefore not a reason to exempt small files
from chunking, which is what ADR-2 might otherwise have concluded.

**Literature.** FastCDC's parameter analysis and normalized chunking:
<https://www.usenix.org/conference/atc16/technical-sessions/presentation/xia>.
restic's repository format and its ~1 MiB chunk regime:
<https://restic.readthedocs.io/en/stable/100_references.html#repository-format>.
zstd long-distance matching:
<https://github.com/facebook/zstd/blob/dev/doc/zstd_manual.html>.
`docs/prior-art/cdc-storage.md` for how borg, casync and Xet resolve the same
small-file regime.
