# Competitive landscape — who else is building this

**Status:** live document · **Retrieved:** 2026-09-03 · **Not a §3 entry**

The eleven §3 analyses cover *technical* prior art: systems whose designs Lattice
should steal from or learn from. They deliberately do not ask a commercial
question, and Stage G0 never required one.

That was a gap. §3's third question is "what will Lattice do differently,
concretely?" — and the honest answer depends on who else is currently doing it.
This document exists because a check for direct competitors turned up more than
expected, including one announced **two days before this was written**.

---

## The direct thesis competitor: East River Source Control

| | |
|---|---|
| **Positioning** | "the building blocks for human ↔ machine collaboration"; "Built for humans and machines" |
| **Backing** | Amplify Partners, who have publicly argued that agents need better version control than Git |
| **Team** | **Martin von Zweigbergk — creator of Jujutsu — became CTO on 2026-09-01.** `jj`'s own `docs/paid_contributors.md` now lists ERSC ahead of Alphabet, with eight named jj contributors |
| **Status** | Pre-product. The site is a mailing-list signup; latest availability update dated May 2026 |
| **Apparent angle** | Server-side scale — jj improves the laptop, but "the remote server is still Git, which has a ceiling that comes fast for products at scale" |

**Assessment.** This is the most serious competitive fact in this document, and it
would be dishonest to soften it. ERSC has the single best-qualified person alive
for this problem, institutional funding, and a stated mission that is Lattice's
thesis sentence.

Two things are genuinely different, for now:

1. **Their stated problem is scale; ours is provenance.** ERSC's public framing is
   about the server-side ceiling for large organisations. Nothing on their site
   mentions provenance, attribution, or authorship. That may change.
2. **They are pre-product.** So is Lattice. Neither of us can claim a lead.

**What it should change here.** Nothing in the architecture; everything in the
positioning. Lattice cannot credibly market itself as "rethinking version control
for the AI era" against the person who already rethought version control. The
defensible claim is narrower and is stated at the end of this document.

**Falsification watch:** if ERSC ships signed authorship provenance in its data
model, Lattice's remaining differentiator is local-first operation alone, and the
project should be re-scoped or stopped. This is a stakeholder decision, recorded
here so the trigger is visible in advance rather than argued about later.

---

## The shipping competitor: Diversion

| | |
|---|---|
| **What** | Cloud-native version control; Y Combinator S22 |
| **Original market** | Game studios and creative teams — a Perforce alternative |
| **Strengths** | Large binaries and exclusive file locking by design; Unreal and Unity integrations; asset review; ~70% claimed cost reduction vs Perforce |
| **Architecture** | **Centralized and cloud-hosted.** Every repository operation is an API call against serverless infrastructure; the desktop client syncs work-in-progress to the cloud in real time |
| **Agentic move** | Recently repositioned as "version control for an agentic world", with a Claude Code plugin capturing "prompt provenance, decisions, reasoning traces, implementation context" |
| **Status** | Shipping, with paying customers |

**Assessment.** Diversion is ahead of Lattice on two of its four pillars — large
binaries and agent context — and is a real business today. It also solves the
exclusive-locking problem for binary assets that `docs/prior-art/centralized-and-data.md`
identifies as an open gap in Lattice's design and that §8 does not list as a
non-goal.

**The architectural fork is genuine, not spin.** Diversion is centralized,
cloud-hosted and online-first. §2 makes "offline-everything distribution" and "no
mandatory server" CONSTRAINTs Lattice must preserve. These are not two
implementations of one idea; they are opposite answers to "where does the truth
live." Each is correct for a different customer, and neither can easily become
the other.

**Where Lattice differs concretely:** provenance in Diversion is a *plugin
capturing prompt context into their cloud*. In Lattice it is a signed record in
the checkpoint's own data model, verifiable offline, with no third party holding
the source (§5.5, gates G5.1/G5.2/G5.4). A team that cannot send source code to a
vendor — regulated finance, health, defence — can use one of these and not the
other.

---

## The adjacent category: build and artifact provenance

**SLSA**, **Sigstore**, **in-toto**, and tools such as `agent-sign` establish
cryptographic provenance for *how an artifact was built* — builder identity, build
instructions, environment, dependency digests — with transparency-log backing.

**This is a different layer and the distinction must be explained rather than
assumed.** SLSA answers "this binary came from this source via this builder."
Lattice answers "this function was written by this actor and reviewed by that
one." A buyer will not necessarily see the difference unprompted, and any pitch
that implies Lattice competes with Sigstore is both wrong and easy to puncture.

**Design consequence:** §5.5 already requires that attestation be designed so a
sigstore-style flow can layer on later. This landscape confirms that as the right
call — the correct relationship is complementary, not competitive, and Lattice
should be able to *emit* in-toto-shaped attestations rather than replace them.

---

## The market is validated, and that is not purely good news

Widely-quoted figures at the time of writing: **41% of code is AI-generated or
AI-assisted**, and **82% of developers use AI tools weekly**. A published census
detects AI coding agents across **180 million repositories**.

This confirms G0.7's GO verdict from an independent direction, which is
reassuring. It also means:

- **G0.7's measurement is no longer novel.** Our 14.4% median / 67.4% recent
  figures are real, reproducible and ours, but a 180-million-repository census
  makes them internal validation rather than a publishable finding. Any external
  communication treating them as new is overclaiming.
- **The problem is visible to everyone.** A market this legible does not stay
  uncontested, and the entrants above are evidence that it already has not.

---

## What is actually unoccupied

Stated as narrowly as the evidence supports:

> **Signed authorship provenance built into the version-control data model,
> verifiable offline, on hardware the user controls.**

Everything adjacent is taken. The local UX is jj's and Sapling's, and both are
better than anything Lattice will have for months. Large binaries are Diversion's
and Perforce's. Build provenance is SLSA's and Sigstore's. The "VCS for the AI
era" banner is ERSC's, with better credentials.

What remains is the intersection: provenance as *data model* rather than plugin,
*authorship* rather than build, and *local-first* rather than cloud. The natural
audience is teams for whom a cloud VCS is disqualifying — regulated industries,
defence, and anyone contractually barred from sending source to a vendor. That is
a real constituency, structurally unavailable to Diversion, and probably not
ERSC's first priority.

It is also a much smaller claim than "rethinking version control," and the
project's public language should reflect that.

---

## Consequences for the record

1. **`README.md` and `STAKEHOLDER/001` overstate the differentiator** as written
   and should be narrowed to the claim above.
2. **Exclusive locking for binary assets** is now a competitive gap as well as a
   design gap: Diversion ships it, Perforce ships it, Lattice has no answer and
   §8 does not list it as a non-goal. Already raised under the deferred findings
   in `docs/DISAGREEMENTS.md`; this raises its priority.
3. **This document is a live watch item**, not a one-off. The falsification
   condition on ERSC above should be re-checked before G5 (the agent layer) is
   built, since that is the stage whose value depends on the differentiator still
   being unoccupied.

## Sources

- [ERSC — Martin von Zweigbergk named CTO](https://ersc.io/blog/martin-joins-ersc) [primary]
- [East River Source Control](https://ersc.io/) [primary]
- [Diversion — Version Control for an Agentic World](https://www.diversion.dev/blog/diversion-version-control-for-an-agentic-world) [primary]
- [Diversion — Launch HN (YC S22)](https://news.ycombinator.com/item?id=39088551) [primary]
- [Diversion vs Git LFS](https://www.diversion.dev/compare-diversion-to-git-lfs) [primary]
- [Code provenance is the missing control for AI-generated commits](https://nhimg.org/articles/code-provenance-is-the-missing-control-for-ai-generated-commits/) [secondary]
- [agent-sign — agent attestations from source to runtime](https://github.com/always-further/agent-sign) [primary]
- [Detecting AI Coding Agents in Open Source: a census of 180M repositories](https://arxiv.org/pdf/2606.24429) [primary]
- [Beyond Identity — why code provenance is non-negotiable](https://www.beyondidentity.com/resource/why-is-code-provenance-non-negotiable-in-the-age-of-ai) [secondary]
