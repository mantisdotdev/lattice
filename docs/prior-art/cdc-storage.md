# restic, borg, casync and Xet

> Four production systems that put content-defined chunking (CDC) into the field. **restic**
> (Alexander Neumann; commits from December 2014, v0.1.0 in 2015) and **borg** (a 2015 fork of
> Attic, 1.0.0 in 2016) are encrypted deduplicating backup tools — mature, ubiquitously
> packaged, dominant in self-hosted backup. **casync** (Lennart Poettering, announced
> 2017-06-20, under `systemd/`) applied the idea to OS-image distribution; its last release is
> `v2` from July 2017 and its default branch has taken two commits since the end of 2022, while
> the Go reimplementation `desync` carries the format. **Xet** (XetHub, acquired by Hugging Face
> 2024-08-08) is the one that put CDC underneath a *Git-shaped* workflow; on 2025-10-27 Hugging
> Face reported that "all **77PB+** over **6,000,000 repositories** have been migrated to the Xet
> backend". These four are the closest prior art to Lattice's storage layer, and the only place
> where ADR-2's parameters have been chosen under real load and then publicly regretted.

## 1. What did it get right?

The four systems disagree about almost everything except the shape of the answer. All of them
clamp chunk size rather than trusting the rolling hash. All of them refuse to let a
content-addressed store hold one entry per chunk. All of them discovered that the chunk index,
not the chunk data, is the resource that eventually breaks — and two of them changed their
defaults in public when it did. Two randomise their chunker per repository so boundaries do not
leak content, and one writes down a read-latency policy expressed as a write-time deduplication
constraint. Every one of these is a decision ADR-2 has to make, already priced by somebody
operating at a scale Lattice will not reach for years.

### 1.1 The clamp is the design, not the rolling hash

A bare rolling-hash cut criterion yields a geometric chunk-size distribution. The Xet paper
states it exactly: "assuming uniformly random hash values, the chunk size is described by the
geometric distribution", so naive rolling-hash chunking "will result in a large number of tiny
chunks which increase the overhead required to represent deduped data" (Low et al., CIDR '23,
§6.2.1). Every shipped system answers with a `(min, target, max)` clamp.

| System | Hash | Window | min | target | max |
|---|---|---|---|---|---|
| restic (`restic/chunker`) | Rabin, per-repo random irreducible polynomial | 64 B | 512 KiB | 1 MiB (mask `(1<<20)-1`) | 8 MiB |
| borg 1.x (`buzhash,19,23,21,4095`) | buzhash, per-repo XOR seed | 4095 B | 512 KiB | 2 MiB | 8 MiB |
| borg 2 (`fastcdc,19,23,21,2`) | FastCDC, normalisation level 2 | — | 512 KiB | 2 MiB | 8 MiB |
| casync | buzhash | 48 B | 16 KiB (`avg/4`) | 64 KiB | 256 KiB (`avg*4`) |
| XetHub (CIDR '23 paper) | 4 Gear hashes, "Low Variance Chunking", each targeting 4 KiB | — | — | 16 KiB | — |
| Xet (shipped, HF spec) | GearHash `h = (h<<1) + T[b]`, mask `0xFFFF000000000000` | 64 B | 8 KiB | 64 KiB | 128 KiB |

Two things to steal. The shipped Xet spec **abandoned** the paper's Low-Variance Chunking for a
single Gear hash with a hard 8 KiB/128 KiB clamp — the simpler mechanism survived contact with
Hub scale. And FastCDC's own evaluation prices the clamp: enlarging the minimum and skipping
cut-points raises speed "by the ratio of Predefined min chunk size / Expected avg chunk size"
but "decreases the deduplication ratio (about 15 percent decline in the worst case)", which
normalised chunking recovers. Their comprehensive result is explicit — "NC-2 with MinSize of
8 KB maximizes the chunking speed without sacrificing the deduplication ratio", with NC-2 at a
6 KB minimum giving the highest dedup ratio at comparable average chunk size — and so is their
sizing rule: the average generated chunk size "is approximately equal to the summation of the
expected chunk size and the applied minimum chunk size". On speed, the ATC '16 abstract claims
FastCDC is "about 10x faster than the best of open-source Rabin-based CDC … while achieving
nearly the same deduplication ratio"; the TPDS extension's own microbenchmark is more modest —
Gear at about 3× Rabin, plus 40–50% from the optimised hash judgment. Cite the range, not the
headline.

### 1.2 Never let CAS entries scale 1:1 with chunks

Everyone packs. restic writes `EncryptedBlob₁ ‖ … ‖ EncryptedBlobₙ ‖ EncryptedHeader ‖
Header_Length`, with blobs "authenticated and encrypted independently", which "enables
repository reorganisation without having to touch the encrypted Blobs"; pack size is tunable
via `--pack-size`, default 16 MiB, range 4–128 MiB. borg 1.x uses append-only segment logs
capped at `max_segment_size = 524288000` (~500 MB) holding records of `(CRC32, entry size,
tag ∈ {PUT(0), DELETE(1), COMMIT(2)}, 32-byte key, data)`; borg 2 moved to **pack files** —
"many of them are batched into a pack file and that pack file is stored as a single store
object." Xet states the rule outright — "Avoid communication or storage strategies that scale
1:1 with the number of chunks" — because the Hub's ~45 PB at 64 KiB is roughly 690 billion
chunks; it aggregates into **xorbs** of ≤64 MB and ≤8,192 chunks.

### 1.3 The index is the cost centre, and its price per chunk is known

borg's `docs/misc/create_chunker-params.txt` is the best public measurement — one 37 GB VM
directory chunked three ways:

| chunker-params | target | unique chunks | index bytes | compressed | deduplicated |
|---|---|---|---|---|---|
| `10,23,16` (min 1 KiB) | ~64 KiB | 378,374 | 20,971,538 | 14.81 GB | **12.18 GB** |
| `16,23,20` (min 64 KiB) | ~1 MiB | 25,889 | 1,310,738 | 14.60 GB | 13.38 GB |
| `16,23,31` (min 64 KiB) | 8 MiB | 4,319 | 327,698 | **14.59 GB** | 14.59 GB |

Three readings. (a) The marginal index cost is ~55 and ~51 bytes per chunk on the two finer
rows; borg 2 documents the entry precisely — "a chunks index entry is 32 + 48 == 80 bytes, and
that is also exactly what it needs on disk"; Xet's paper independently puts the MerkleDAG at
"about 0.5% of the size of the repository", which at 16 KiB chunks is also ~80 bytes/chunk.
Three systems, one number: **budget 50–100 bytes of index per chunk and size RAM from it** —
borg adds that "the total RAM needs are about 2.1x the repo index size". (b) Deduplication
improves monotonically as chunks shrink (14.59 → 12.18 GB), and index cost rises 64× to buy it.
(c) Compression gets *better* with larger chunks (14.81 → 14.59 GB) because per-chunk
compression destroys cross-chunk context. Hold on to (c); it is the whole story for G1-9.

### 1.4 Xet: CDC under Git, and the parts that are genuinely new

Xet is the only prior art with Lattice's problem shape. Its Content-Defined Merkle Tree gives
interior nodes a *variable* fan-out by applying CDC to the hash stream itself — "a hash is on a
chunk boundary if the hash meets criterion `hash mod 4 == 0`", with nodes constrained to
"between 2 and 8 hashes" — so an insertion perturbs a path rather than the tree. File
reconstruction intersects the file's CDMT with the CAS objects' CDMTs to yield a xorb hash plus
"start and end indices within the xorb (start inclusive, end exclusive)". Deduplication is
three-tier: in-session hash table → local shard cache → a global API queried only for **key
chunks** (the first chunk of every file, plus chunks whose "last 8 bytes of the hash interpreted
as a little-endian 64 bit integer % 1024 == 0"), with hashes HMAC-protected so that "raw chunk
hashes are never transmitted from servers to clients". Measured: 50 versions of CORD-19,
2.45 TB raw → 545 GB with Git LFS, **287 GB with Xet** (including a 2.4 GB MerkleTree), 87 GB
with a CSV-aware chunker — though the paper notes "data compression … is not used in this
benchmark", so those are uncompressed figures and not comparable to a zstd store. Separately,
`bartowski/gemma-2-9b-it-GGUF`'s 29 quantisations, 191 GB → ~97 GB in 1,515 blocks, upload
509 → 258 minutes at 50 MB/s.

### 1.5 Two policies nobody else writes down

**Per-repository chunker randomisation.** restic selects "an irreducible polynomial … at random
and saved in the file config when a repository is initialized, so that watermark attacks are
much harder"; borg's "buzhash table is altered by XORing it with a seed randomly generated once
for the repository, and stored encrypted in the keyfile. This is to prevent chunk size based
fingerprinting attacks on your encrypted repo contents." Chunk boundaries leak content.

**Fragmentation prevention.** The Xet deduplication spec devotes a section to it: over-eager
dedup scatters a file across xorbs and slows reads, so implementations "SHOULD keep long,
continuous runs together", suggesting "a minimum run of chunks (e.g. at least 8 chunks) or
targeting an average contiguous run of chunks totalling length >= 1MB". It also throttles the
global query itself: "Consider only issuing a request to an eligible chunk every ~4MB of data."
A read-latency policy expressed as a write-time deduplication constraint.

### 1.6 casync's one great idea

casync chunks the serialised stream, not files: "We remove file boundaries before chunking
things up. This means that small files are lumped together with their siblings and large files
are chopped into pieces." Ten thousand 4 KB files do not become ten thousand CAS entries. Its
index (`.caibx`/`.caidx`) is deliberately trivial — "chunk hash values plus their respective
chunk sizes in a simple linear array."

## 2. Why didn't it win (or why is it niche)?

**restic and borg are not niche — they won backup and never attempted VCS.** Their irrelevance
to source control is a *parameter* choice: a 512 KiB minimum chunk is right for nightly TB
snapshots and useless for a one-line edit. The scar is index memory, and borg's 1.0.0 changelog
is unusually candid: "one of the biggest issues with borg < 1.0 (and also attic) was that it had
a default target chunk size of 64kiB, thus it created a lot of chunks and thus also a huge chunk
management overhead (high RAM and disk usage)." The default moved from `10,23,16,4095` to
`19,23,21,4095` — 64 KiB to 2 MiB — accepting that "the new big chunks do not deduplicate with
the old small chunks, so expect your repo to grow at least by the size of every changed file".
Every existing repository silently lost its dedup. restic hit the same wall from the other
side: maintainer Michael Eischer's memory PR #3773 states it "is not intended as a complete
solution to the memory usage problems, as that would require a repository format change as
discussed in #2532." And restic shipped no compression at all until v0.14.0 (2022-08-25,
repository format v2), which says something about how far pure CDC gets without entropy coding.
The consequences are visible from outside: a published experiment prepending two bytes to a
100 MB random file grew a borg repository by ~6 MB and a restic repository by ~2 MB — roughly
24× and 8× over G1-8's 262,144-byte budget.

**casync went niche for ecosystem reasons, not technical ones.** A tool with no platform: no
hosting service, no default consumer. Its last release is `v2` (2017-07-26) and its repository
has taken two commits since the end of 2022. The format outlived the implementation — `desync`
"implements the casync format and interoperates with it — same index files, archives and chunk
stores." Its most visible downstream, RAUC, added casync bundle streaming in 0.4 (2018), then
shipped its own HTTP(S) streaming installation in 1.7 (2022-06-03) and desync support in 1.8
(2022-09-30). casync support has not been removed, but it is now one option among three. A
format can survive its implementation; an implementation cannot survive having no consumer that
depends on it — which is the argument for Lattice's Git bridge shipping before the semantic and
agent layers, not after.

**Xet is currently winning, and *how* it wins is the uncomfortable part.** Rollout began
January 2025; by May 2025 it "became the default on the Hub for new users and organizations";
500,000 repositories holding 20 PB had joined within six months, and by October 2025 Hugging
Face reported the whole Hub migrated with no user intervention. But on the Hub only files
matched by `.gitattributes` and represented by "pointer files" go to Xet — the mechanism exists
precisely so "the repository stays small and typical Git workflows remain efficient." The
largest CDC-under-Git deployment on earth deliberately does not route ordinary small source
files through its chunker. That is 77 PB of evidence against Lattice's binding decision that
*all* content goes through CDC, and it deserves an answer rather than a footnote.

## 3. What will Lattice do differently, concretely?

ADR-2 does not exist yet in `docs/adr/` (ADR-1, ADR-3 and ADR-4 do), and neither does
`harness/g1/g1_8_chunking.py` — `harness/g1/` is empty, so by GAUNTLET.md's own rule the gates
constraining ADR-2 "do not exist yet". That makes this the moment to fix what ADR-2 will claim.
Two gates bind it, both registered in `harness/gates.toml`. **G1-8** (SOFT, perf): "p95 new
bytes persisted for a 1-byte edit in a 50MB file (100 trials × 5 file types)", target
`< 262144`. **G1-9** (SOFT, perf) is a *dual* baseline: `max(store/restic ÷ 1.25,
store/git-gc ÷ 1.5) < 1.0`. Every measurement Lattice has taken addresses only the git-gc leg;
the restic leg — the leg this document's own subject matter defines — has never been measured.

### 3.1 ADR-2 must adopt a parameter set Lattice actually measured

"min 8 KiB / target 64 KiB / max 128 KiB" is Xet's *shipped spec*, but the number quoted in its
support (`edit_locality_p95_bytes` = 80,681 B) comes from the sweep row `(8192, 32768, 131072)`
— target 32 KiB, not 64 KiB. `bench/results/raw/adr2-source-sweep.json` contains eight
candidates and 8/64/128 is not among them. ADR-2 must name a row that was measured, or measure
the row it names. On the rows that were measured, the source sweep (37,026 files, 219.9 MB,
mean file 5,940 B) says something a Xet-shaped recommendation misses:

| params | chunks/file | mean chunk | duplicate bytes | index share of store | p95 edit bytes |
|---|---|---|---|---|---|
| (1K, 2K, 8K) | 2.82 | 2,133 B | 2.42% | 6.29% | 5,215 |
| (2K, 8K, 32K) | 1.41 | 4,330 B | 1.81% | 3.73% | 28,705 |
| (8K, 32K, 128K) | 1.07 | 5,769 B | 1.39% | 3.01% | 80,681 |
| (16K, 64K, 256K) | 1.02 | 6,028 B | 1.32% | 2.92% | 179,109 |

**At Xet-class parameters, chunking a source tree is nearly a no-op**: 37,026 files produce
39,489 chunks, 1.07 per file, because almost every file is below the 8 KiB minimum. The target
size never binds; Lattice inherits none of Xet's benefit and all of its per-chunk index cost.
ADR-2 should take **(min 2 KiB, target 8 KiB, max 32 KiB)**: it is measured, it holds G1-8's p95
at 28,705 B with a 9× margin and a worst case of 2 × 32 KiB = 65,536 B — where a 128 KiB max has
a worst case of exactly 262,144 B, the gate boundary — and §3.3 shows it is also the best row
under bounded packs. Rolling hash: FastCDC. But note that `prototypes/chunkbench` calls
`fastcdc::v2020::FastCDC::new`, whose documented default is `Normalization::Level1`, so **every
number in the ADR-2 sweep was produced at NC-1, not the NC-2 that borg 2 ships.** ADR-2 must
either re-run the sweep through `FastCDC::with_level(…, Normalization::Level2)` or record NC-1
as the decision. It must not cite borg 2's default as evidence for a level it did not measure.

### 3.2 The single-snapshot crossover has two causes, and they trade places

It is tempting to blame the crossover entirely on lost compression context and dismiss per-chunk
metadata. Lattice's own artifact refutes that. Decompose the excess of the chunked store over
whole-file zstd on the same corpus:

- **(1K, 2K, 8K)**: store 76,833,953 B vs whole-file 57,548,505 B. Excess 19,285,448 B, of
  which index is 4,829,664 B — **25% metadata, 75% lost compression context.**
- **(8K, 32K, 128K)**: store 59,928,312 B. Excess 2,379,807 B, of which index is 1,804,272 B —
  **76% metadata, 24% lost compression context.**

Both mechanisms are real; which dominates flips across the sweep, so a design that fixes only
one of them fixes the wrong half at one end of the range. The `<1KiB` bucket of
`small_file_crossover` is the pure case: every file there is below the 2 KiB minimum, so it is
exactly one chunk, chunked and whole-file compression are byte-identical, and the entire 1.14×
penalty is 48 bytes of index per file minus what file-level dedup returns. All seven buckets:
1.14× (<1 KiB), 1.04× (1–4 KiB), 1.06× (4–16 KiB), 1.13× (16–64 KiB), 1.15× (64–256 KiB), 1.26×
(256 KiB–1 MiB), 1.80× (≥1 MiB). There is no bucket where naive CDC pays for itself on a single
snapshot — which is the measurement behind Hugging Face's routing decision, arrived at
independently.

### 3.3 The pack format is ADR-2's real decision, and bounded frames are affordable

restic, borg and casync all compress **per chunk or per blob**, deliberately, to preserve random
access. That single decision costs them git's advantage, and it is the decision ADR-3 left open:
ADR-3 fixes "custom append-only packs with an immutable sorted index per pack" and says nothing
about the compression unit — its own benchmark artifact stores 40,000 × 8,192 B chunks in
329,440,000 bytes, i.e. uncompressed. Running `scripts/corpus/git_baseline.py` against django
(146,344 unique blobs, 3.066 GB raw; `git gc --aggressive` = 113.80 MB) with the prototype at
HEAD measures per-chunk zstd, an unbounded long-window pack, and — contrary to the claim that
this was unmeasured — the bounded, independently decompressible frames a real store needs:

| params | per-chunk zstd | unbounded pack | 16 MiB frames | 4 MiB frames | 1 MiB frames | unique chunks |
|---|---|---|---|---|---|---|
| (1K, 2K, 8K) | 2.83× | 0.55× | 0.59× | 0.63× | **0.71×** | 311,241 |
| (2K, 8K, 32K) | 4.11× | 0.52× | 0.58× | **0.64×** | 0.82× | 198,240 |
| (8K, 32K, 128K) | 5.97× | 0.52× | 0.59× | 0.70× | 1.05× | 155,952 |
| (16K, 64K, 256K) | 6.75× | 0.52× | 0.61× | 0.75× | 1.20× | 149,555 |

Three results, all new. First, **the long-window pack is nearly independent of chunk size**
(0.514–0.547 across the whole sweep) because zstd's matcher recovers the cross-version
redundancy the chunker failed to deduplicate; the chunker is not the lever, the pack is.
Second, **bounding the frames is affordable**: relative to the unbounded pack, 16 MiB frames
cost 1.09–1.18×, 4 MiB frames 1.15–1.44×, and 1 MiB frames 1.30–2.30×; at 1 MiB every row but
the two coarsest still lands under 1.0× git, and the four finest under 0.83×. Third, **chunk
size only matters once frames are small** — at 1 MiB the sweep spreads 0.71× to 1.20×, so if
G1-7 forces small frames, finer chunks buy the size back. netty at `--commits 300` reproduces
the shape at lower absolute values (per-chunk 3.46×, unbounded 0.36×, 16 MiB 0.42×, 4 MiB
0.45×, 1 MiB 0.54× at (2K, 8K, 32K)).

ADR-2 therefore specifies: (i) one chunker for everything, small files remaining one chunk, so
the binding decision stands; (ii) the CAS interface unchanged at `get(chunk_hash) -> bytes`, so
cross-chunk compression is invisible above the CAS and adds no eighth noun; (iii) packs ordered
by locality so successive versions of one path land adjacent, which is also what satisfies Xet's
fragmentation rule; and (iv) **4 MiB seekable frames as the default, with the frame size a
declared parameter of the pack format**, since it is the knob that trades store size against
read amplification. ADR-3's store benchmark must therefore report read amplification — bytes
decompressed per chunk read — alongside store size: at a 4,330 B mean chunk, a 4 MiB frame is
~970× amplification and a 1 MiB frame ~240×, and that number lands directly on G1-7's
`max(switch p95 / 500ms, log p95 / 100ms) < 1.0`.

### 3.4 What ADR-2 must state that no benchmark can settle

**Chunker seed.** restic randomises its polynomial and borg seeds its buzhash table, both
explicitly against watermarking and fingerprinting. Lattice wants the opposite — stable
cross-repository chunk identity, so `ltx adopt` and native sync dedup globally — which is
exactly the configuration those two designed against, and why Xet's global dedup API is
HMAC-protected. `fastcdc::v2020::FastCDC::with_level_and_seed` exposes the same gear-table seed
borg uses, so the mechanism is one argument away; the decision is which way to point it. ADR-2
must state the tradeoff and record it as accepted, and Lattice must not ship any
cross-repository dedup query without Xet-style HMAC protection of chunk hashes.

**Fragmentation.** Xet caps dedup aggressiveness (runs of ≥8 chunks or ≥1 MB contiguous) to
protect reads. Locality-ordered packing makes this load-bearing rather than optional, and it
lands on G1-7. It belongs in ADR-2, not in a later bug.

**Redaction under dedup.** Xet's authors name the problem first: the MerkleDAG "is append only
and grows monotonically over time, which makes common industry use cases of data deletion and
eviction (e.g., garbage collection, removed branches, legal deletion requirements) a challenge."
Lattice's redaction protocol has that shape and adds a worse case: deduplication means a chunk
holding a secret may be **co-owned** by an innocent file, so redacting it either corrupts that
file or forces re-materialisation of every co-owner. ADR-2 must define chunk-level reference
counting and a co-ownership resolution — re-materialise co-owners, or refuse — and a gate must
construct the case where a secret-bearing chunk is deduplicated into an innocent file.

### Residual risk for Lattice

**The restic leg of G1-9 has never been measured, in a document about restic.** G1-9 is
`max(store/restic ÷ 1.25, store/git-gc ÷ 1.5)`; every artifact in `bench/results/raw/` measures
only the git-gc leg, and searching the repository for `restic` returns the gate definition, two
ADR-3 sentences and a README line — no baseline. The two legs pull opposite ways: a long-window
pack beats git precisely by *not* doing what restic does, while a restic repository of source
history would be far larger than a git pack, so the restic leg is probably slack — a gate that
cannot fail is not a gate. ADR-2 should measure it or say plainly that it is decorative.

**The load-bearing pack number is not reproducible across prototype edits.** The 0.59× figure
for netty at (8K, 32K, 128K) came from a build of `prototypes/chunkbench` predating commit
`1bb6cfc`; re-running the same committed script against the prototype at HEAD gives 0.36× at the
same parameters with the git baseline unchanged to four digits, and two consecutive re-runs
agree to four digits, so this is a build difference, not noise. The committed django artifact
moved the same way — 1.42× to 0.52× at (8K, 32K, 128K) — which is a 2.7× swing in the number
that decides G1-9. `prototypes/` sits outside the freeze discipline GAUNTLET.md applies from G1
onward, so nothing caught it. Any ADR-2 number must be regenerated from the committed prototype
and re-committed in the same change.

**`--commits N` does not do what its name says.** `scripts/corpus/git_baseline.py` derives
`tip` and `oldest` from `rev-list -nN`, then fetches `tip:refs/heads/m` with no `--depth` and
enumerates objects with `rev-list --objects --all`. The comment "Graft the range: keep only the
N commits by shallowing at the oldest" describes code that is not there. Both the 300- and
400-commit django artifacts therefore measure django's entire 34,907-commit history and report
identical blob counts — which is why a 300-commit run "reproduces the 400-commit artifact to
three digits". Either implement the shallowing or delete the parameter.

**Edit locality is measured on the wrong file.** `chunkbench` runs `edit_locality` on "the
largest file available", which in a corpus with two files ≥1 MiB is a ~1–2 MB text file. G1-8
specifies a 50 MB file across five file types. Incompressible binary is where the max clamp
binds most often and p95 approaches 2× max; at a 128 KiB max that is exactly 262,144 B, the gate
target. G1-8's harness must chunk real 50 MB files of each declared type before ADR-2 fixes the
max clamp — and the per-chunk store figures are themselves extrapolations, since `sweep_one`
compresses a 20,000-chunk sample and scales the ratio.

**Continuous autosnapshots have no analogue in any of the four systems.** restic and borg
snapshot daily; Lattice snapshots continuously, and each autosnapshot of a modified 2 KB file
writes a whole new chunk — one changed byte, new hash, no dedup. Thinning reclaims it later, but
the ephemeral tier's write amplification and index churn are unmeasured and land on exactly the
index borg retreated from in 2016. At (2K, 8K, 32K) django yields 198,240 unique chunks for
3.07 GB, about 64,650 per GB, so a 100 GB monorepo history is ~6.5 M chunks and, at borg's
documented 80 B/entry, ~520 MB of index — near 1.1 GB resident under borg's 2.1× rule, and
double that at (1K, 2K, 8K). Xet reports the corresponding ceiling directly: the MerkleDAG "gets
too large to download when the repository approaches the 10-100TB range". Lattice has no
subgraph-on-demand or partial-index story, and the sweep's own 48 B/chunk index constant is
optimistic against borg's measured 80 B.

**The strongest prior art does not use one chunker for everything.** borg 2 ships two:
`fastcdc,19,23,21,2` for file content and `fastcdc,15,19,17,2` — a 32 KiB minimum against
512 KiB — for the item metadata stream, because the two streams have different size
distributions. Hugging Face routes only `.gitattributes`-matched files through Xet at all.
Lattice's binding decision that all content goes through one chunker is therefore a bet against
both of the systems that have run this at scale. §3.3 shows the bet is affordable *because the
pack, not the chunker, does the work* — but ADR-2 should record it as a bet with that stated
reason, not as an obvious design.

**Native P2P sync makes redaction a statement, not an erasure.** Once a chunk has propagated, a
signed tombstone in the op-log records intent; it does not delete bytes on a peer. The GDPR
justification for the redaction protocol is weaker than the spec assumes, and the weakness is
structural rather than an implementation gap.

## Sources

- restic design document, `doc/design.rst` — pack layout, independent blob encryption, random polynomial and watermark attacks — https://github.com/restic/restic/blob/master/doc/design.rst [primary]
- `restic/chunker` constants (`windowSize = 64`, `MinSize = 512 * kiB`, `MaxSize = 8 * miB`, mask `(1<<20)-1` in `NewBase`) — https://github.com/restic/chunker/blob/master/chunker.go [primary]
- restic tuning parameters — `--pack-size`, default 16 MiB, range 4–128 MiB — https://restic.readthedocs.io/en/stable/047_tuning_parameters.html [primary]
- restic v0.14.0 release, 2022-08-25 (zstd compression, repository format v2) — https://github.com/restic/restic/releases/tag/v0.14.0 [primary]
- restic PR #3773, Michael Eischer — "would require a repository format change as discussed in #2532" — https://github.com/restic/restic/pull/3773 [primary]
- borg internals / data structures, stable (buzhash `19,23,21,4095`; per-repo XOR seed; segment records; `max_segment_size = 524288000`) — https://borgbackup.readthedocs.io/en/stable/internals/data-structures.html [primary]
- borg internals / data structures, latest (borg 2 default `fastcdc,19,23,21,2`, metadata stream `fastcdc,15,19,17,2`; "a chunks index entry is 32 + 48 == 80 bytes"; pack files) — https://borgbackup.readthedocs.io/en/latest/internals/data-structures.html [primary]
- borg `docs/misc/create_chunker-params.txt` — the 37 GB three-way measurement and "the total RAM needs are about 2.1x the repo index size" — https://github.com/borgbackup/borg/blob/master/docs/misc/create_chunker-params.txt [primary]
- borg 1.0.0 changelog — 64 KiB → 2 MiB default, its rationale, and the lost dedup — https://borgbackup.readthedocs.io/en/1.0.0/changes.html [primary]
- Poettering, "casync — A tool for distributing file system images", 2017-06-20 — https://0pointer.net/blog/casync-a-tool-for-distributing-file-system-images.html [primary]
- casync `src/cachunker.h` — `CA_CHUNK_SIZE_AVG_DEFAULT` 64 KiB, min `avg/4`, max `avg*4`, 48-byte window — https://github.com/systemd/casync/blob/main/src/cachunker.h [primary]
- desync — Go implementation interoperating with the casync format — https://github.com/folbricht/desync [primary]
- RAUC changelog — casync in 0.4 (2018), HTTP(S) streaming in 1.7 (2022-06-03), desync in 1.8 (2022-09-30) — https://rauc.readthedocs.io/en/latest/changes.html [primary]
- Low et al., "Git is for Data", CIDR 2023 — geometric distribution, Low Variance Chunking, CDMT `hash mod 4 == 0` with 2–8 children, 16 MB CAS objects, CORD-19 results (uncompressed), MerkleDAG 0.5% / 10-100TB / append-only deletion limitation — https://www.cidrdb.org/cidr2023/papers/p43-low.pdf [primary]
- Xet chunking specification — GearHash, 8 KiB / 64 KiB / 128 KiB, mask `0xFFFF000000000000` — https://huggingface.co/docs/xet/chunking [primary]
- Xet deduplication specification — xorbs ≤64 MB / ≤8192 chunks, key chunks, HMAC, fragmentation prevention, ~4 MB query spacing — https://huggingface.co/docs/xet/deduplication [primary]
- Hugging Face, "From Chunks to Blocks" — the 1:1 rule, ~690 billion chunks, Gemma GGUF numbers — https://github.com/huggingface/blog/blob/main/from-chunks-to-blocks.md [primary]
- Hugging Face, "Migrating the Hub from Git LFS to Xet" — timeline, May 2025 default — https://huggingface.co/blog/migrating-the-hub-to-xet [primary]
- Hugging Face, "huggingface_hub v1.0", 2025-10-27 — "all 77PB+ over 6,000,000 repositories have been migrated to the Xet backend" — https://huggingface.co/blog/huggingface-hub-v1 [primary]
- Hugging Face, "XetHub joins Hugging Face", 2024-08-08 — https://huggingface.co/blog/xethub-joins-hf [primary]
- Hugging Face Hub storage backends — `.gitattributes`, pointer files, "the repository stays small" — https://huggingface.co/docs/hub/en/storage-backends [primary]
- Xia et al., "FastCDC", USENIX ATC '16 abstract (10× over the best open-source Rabin CDC) and the TPDS 2020 extension (15% cut-point-skipping loss; NC-2 with a 6–8 KB minimum; avg ≈ expected + min; Gear ~3× Rabin) — https://www.usenix.org/conference/atc16/technical-sessions/presentation/xia · https://csyhua.github.io/csyhua/hua-tpds2020-dedup.pdf [primary]
- `git gc` documentation — `--aggressive` passes `-f` → `--no-reuse-delta`; `gc.aggressiveWindow` 250 against a default window of 10; `gc.aggressiveDepth` 50, the same as the default depth — https://git-scm.com/docs/git-gc [primary]
- scy, borg vs restic insertion experiment (100 MB random file, 2-byte prepend: borg +6 MB, restic +2 MB) — https://gist.github.com/scy/de5176aef9209cb07e5f8c7b365cfbf1 [secondary]
- `fastcdc` crate 3.2.1, `src/v2020/mod.rs` — `FastCDC::new` defaults to `Normalization::Level1`; `with_level_and_seed` exposes a per-repository gear-table seed — https://docs.rs/fastcdc/3.2.1/fastcdc/v2020/index.html [primary]
- In-repo: `harness/gates.toml` (G1-7, G1-8, G1-9); `bench/results/raw/adr2-source-sweep.json`; `bench/results/raw/adr2-git-baseline-django.json`; `bench/results/raw/adr3-store-bench.json`; `prototypes/chunkbench/src/main.rs`; `scripts/corpus/git_baseline.py`; plus two independent runs of `scripts/corpus/git_baseline.py` against `corpus/data/repos/netty__netty.git` at `--commits 300` performed for this document [primary]
