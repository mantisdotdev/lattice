# ADR-1 — Merge model: snapshot DAG with structural three-way merge

**Status:** Accepted · **Stage:** G0.6 · **Blocks:** G4 entry, §5.4

## Context

§7 poses the question directly: patch algebra (Pijul-style) versus a snapshot DAG
with structural three-way merge. The choice is load-bearing because it determines
what "merge" *means* in the data model, and therefore what G4.3 and G4.4 can even
measure.

Patch algebra treats a repository as a set of patches with a commutation
relation. Its promised payoff is real: merges become order-independent by
construction rather than by testing, cherry-picking is not a distinct operation,
and conflicts are values in the algebra rather than exceptional states. Pijul's
categorical reformulation (pushouts over a free category of patches) fixed the
exponential-merge pathology that defined Darcs's reputation, so the theory is no
longer the objection it was in 2008.

Against that: the Git bridge is a CONSTRAINT (§5.6, "bidirectional,
daily-driveable, ships before the semantic and agent layers"), and Git is a
snapshot model. Every patch-algebra repository that must round-trip to Git pays
an impedance cost at the boundary — the mapping from patch sets to commit trees
is lossy in one direction and expensive in the other. G3.1 demands **100%**
tree/commit/authorship fidelity across ≥1,000 round-trips. A model that must
reconstruct snapshots from patches to answer "what tree is this?" makes the
hardest gate in the project harder for a benefit the gates do not measure.

## Options

**Option A — Patch algebra (Pijul-style).** Order-independence and clean
cherry-pick for free. Costs: bridge impedance against a HARD gate; a much smaller
pool of engineers who can reason about the model (§7 explicitly raises
"contributor hireability"); and the practical evidence that no patch-algebra
system has achieved adoption, which §3 asks us to take seriously as data rather
than dismiss as ecosystem bad luck.

**Option B — Snapshot DAG + three-way merge, structural where the semantic layer
covers the file.** Matches Git's substrate, so the bridge is a translation rather
than a reconstruction. Order-independence becomes a property to *test* rather
than a theorem, which is weaker — but testable properties that hold are worth
more than theorems about a system nobody adopts.

**Option C — Hybrid: snapshot storage, patch-algebraic merge resolution over a
derived patch view.** Attractive on paper. Rejected on complexity: it requires
both models to be correct and the mapping between them to be correct, and §8's
novelty budget forbids inventing where proven designs exist.

## Decision

**We will build Option B: a snapshot DAG with three-way merge, structural when
the semantic layer covers the file and line-based otherwise.**

We steal from Pijul and Darcs the two ideas that do not require the algebra:

1. **Conflicts are first-class values, not states.** A merge always produces a
   checkpoint; conflicted regions are stored as objects carrying both sides and
   the base, and resolution is a later checkpoint (§5.4). This is what makes
   "merges never block" implementable, and it is orthogonal to patch algebra —
   jj demonstrates the same idea on a snapshot backend.
2. **Order-independence is asserted as a property test from the first
   implementation**, not claimed. Where Lattice claims a merge is
   order-independent, a generated property test proves it on that
   implementation. We claim it only for the cases we test: idempotence of
   re-merge, and commutativity of independent structural edits.

## Consequences

**Positive.** The bridge becomes tractable: a Lattice checkpoint maps to a Git
commit tree without reconstruction, which is what G3.1's 100% fidelity target
needs. Contributors can reason about the model on day one. The structural merge
layer (§5.2) becomes an *enhancement* over a working line-based merge rather than
the foundation, so G4.5's honest-degradation requirement is satisfied by
construction: if the semantic layer is absent or unsure, the snapshot three-way
merge is still there and still correct.

**Negative, and accepted.** Cherry-pick and rebase-equivalents are separate
operations rather than algebra, so each needs its own correctness argument.
Order-independence holds only where tested; a case we did not generate is a case
we did not prove. We accept this because the alternative trades a testable
weakness for an adoption risk the prior art has already demonstrated twice.

**Consequence for measurement.** G4.4's denominator is now known: the measured
line-based conflict rate on the G0.3 corpus is **5.66%** (CI95 5.30–6.04%, n =
14,981). "Auto-resolve ≥20% of line-level conflicts" therefore means resolving
roughly 1.1% of all merges — a modest absolute number, which is worth stating
plainly so nobody mistakes G4.4 for a claim that structural merge transforms
everyday merging.

## Evidence

**Measured, this repository.** `corpus/data/merge-baselines.json` — 128,544
two-parent merges mined from 14 repositories across 8 languages; 14,990 replayed
under the frozen oracle (`corpus/manifests/g0-3-oracle.md`, sha256
`61f114e9…`), seed 20260903:

| Bucket | Count | Share |
|---|---:|---:|
| `CLEAN_MATCH` | 14,056 | 93.77% |
| `REPLAY_CONFLICT` | 848 | 5.66% |
| `EXCLUDED_EVIL` | 55 | 0.37% |
| `CLEAN_DIVERGE` | 21 | 0.14% |
| `CLEAN_DIVERGE_NORMALIZED` | 1 | 0.01% |
| `ERROR` | 9 | 0.06% |

Two facts from this table drove the decision. First, **93.77% of real merges
replay to a byte-identical tree under plain line-based three-way merge.** The
population where any merge algebra could differ is small, which caps the value of
a more powerful merge model and argues against paying a structural cost for it.
Second, the conflict rate varies by language from 0.93% (Go) to 25.18% (Rust,
n=139) — so a merge model must degrade well across languages rather than be tuned
for one.

**Literature.** Pijul's design rationale and its pushout formulation:
<https://pijul.org/manual/theory.html>. Darcs's exponential-merge history and the
"conflictors" work: <https://darcs.net/Theory>. Jujutsu's conflict representation
on a snapshot backend — the direct precedent for taking idea (1) without the
algebra: <https://jj-vcs.github.io/jj/latest/conflicts/>. Git's ORT merge, the
replay engine used for the baseline above:
<https://git-scm.com/docs/merge-strategies>.

**Prior-art analyses.** `docs/prior-art/pijul-darcs.md` (why theoretical
superiority did not convert to adoption) and `docs/prior-art/jujutsu.md`
(snapshot-backend conflict objects in production).
