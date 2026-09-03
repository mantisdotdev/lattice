# Disagreements and Corrections

**Gate G0.2 · HARD · produced before any product code exists.**

The brief states the rule plainly: *"You are required to disagree… an agent that
silently implements a flawed spec has failed."* This memo is that disagreement.

Ten challenges follow. They are ordered by how much damage each would do if it
went unaddressed, not by how comfortable they are. Four of them change what gets
built. Three change how something gets *measured* — which matters more than it
sounds, because a gate that measures the wrong population is worse than no gate:
it manufactures confidence. Three are accepted as bets, each with a stated
condition under which I will have been wrong.

A note on tone. The spec is unusually good, and most of it survives contact with
scrutiny. The challenges below concentrate on its seams: places where two
separately-sound requirements collide, and places where a measurement's
denominator quietly excludes the cases the measurement exists to catch.

---

## Challenge 1 — G4.3 measures silent mis-merges in the one population where they almost cannot occur

**Claim challenged:** §6, gate G4.3, defines a silent mis-merge as a case where
"structural merge produces a clean result semantically non-equivalent,
post-normalization, to the human result **in cases where line-based merge was
also clean and matched the human result**." The rate must be ≤ 0.1% and ≤ the
G0.3 line-based baseline.

**Evidence:** The restriction on the denominator is the problem. Consider what
the qualifying population actually is. A case enters it only when line-based
three-way merge (a) produced no conflict and (b) produced exactly what the human
committed. That is the definition of an *easy* merge: the two sides touched
disjoint regions far enough apart that naive text merging was already correct.

Structural merge, running on the same input, has almost nothing to do in that
population. Its opportunity to differ from the line result arises when it moves,
reorders, or reconciles code that the line algorithm would have treated as
independent text — and by construction, in this population, treating the text as
independent was *already the right answer*. So the numerator tends to zero for
reasons that have nothing to do with whether the matcher is any good.

Meanwhile the dangerous population — the one every practitioner fears — is
excluded by the same clause. The real risk of structural merge is that it
*resolves something line merge would have conflicted on*, confidently and
wrongly. That is precisely the population gate G4.4 rewards Lattice for growing
(≥ 20% of line-level conflicts auto-resolved). G4.3 as written therefore creates
an incentive gradient pointing the wrong way: every conflict the matcher
aggressively auto-resolves increases the G4.4 score and is invisible to the G4.3
safety gate, because those cases never had a clean-and-matching line merge to
qualify them.

The two gates share a pinned operating point, which the spec is right to demand
— but sharing an operating point does not help if they measure disjoint
populations.

**Resolution:** ACCEPTED — G4.3's harness is split into two measurements, and
both must pass. The stated metric and target are not weakened; a second, stricter
measurement is added alongside, which §0.4 permits (a re-scope downward is
forbidden; this is a re-scope *upward*).

- **G4.3-a**, exactly as specified: rate over the clean-and-matching population,
  ≤ 0.1% and ≤ the line baseline. Retained unchanged so the spec's own criterion
  is honoured verbatim.
- **G4.3-b**, the population the spec's wording omits: of the cases where
  structural merge auto-resolves a conflict that line merge reported (the G4.4
  numerator), the fraction whose result is semantically non-equivalent to the
  human result under the same G0.3 normalization. This is measured, reported in
  every scorecard next to G4.4's rate, and is the number a reviewer should look
  at first.

G4.3-b has no target inherited from the spec, so imposing one would be my
invention rather than the stakeholder's requirement. It is therefore reported
without a pass/fail threshold in this delivery, and the resolution-vs-mis-merge
curve required by G4.4 is what carries the judgment. **The stakeholder should
consider setting a numeric target on G4.3-b**; my recommendation is that it
become the primary safety gate and G4.3-a be retained as a regression check.
This is flagged in `STAKEHOLDER/001`.

---

## Challenge 2 — The line-based silent-mis-merge baseline that G4.3 compares against is approximately zero by construction

**Claim challenged:** §6, gate G0.3, requires that "line-based conflict AND
line-based silent-mis-merge baselines" be measured on the mined corpus, and G4.3
requires the structural rate be "≤ the G0.3 line-based baseline."

**Evidence:** For a merge commit in a real repository, what is the "human
result"? It is the tree recorded in the merge commit. But for the large majority
of merge commits, that tree *was produced by the version control system's own
automatic three-way merge* — the developer typed `git merge`, it succeeded
without conflict, and they committed the result unedited. The recorded human
result and the line-based merge result are then the same object, not merely
equivalent.

So on the qualifying population, the line-based silent mis-merge rate is not
"low"; it is identically zero, for a tautological reason. The only merges where
the recorded tree differs from the naive three-way result are (a) conflicts the
human resolved by hand and (b) evil merges — and §6 explicitly instructs that
evil merges be excluded by the oracle's exclusion rules.

This makes the constraint "structural rate ≤ line baseline" collapse to
"structural rate = 0 exactly," which is strictly stronger than the 0.1% absolute
target sitting next to it, and stronger than the spec appears to intend given
that it wrote both conditions as if the binding one might be either.

I want to be careful not to overstate this. There remains a genuinely non-empty
measurable population: merges where the human resolved a conflict by hand, where
one can compare a *re-run* line merge (conflict) against the human tree, and
merges from repositories whose merge tooling differed from the replay tool
(different rename-detection settings, ORT versus recursive, `.gitattributes`
merge drivers). But these are not what "line-based silent mis-merge baseline"
naturally denotes, and their population is small.

**Resolution:** ACCEPTED — the G0.3 oracle definition is amended before any G4
work, and the amendment is recorded in the corpus manifest with its hash. The
baseline is measured three ways and all three are reported:

1. **Naive baseline** (the spec's literal reading): identically 0 by
   construction. Reported as such, with this reasoning attached, so that the
   number is never mistaken for evidence of line merge's safety.
2. **Divergence baseline**: over all mined merges, the fraction where re-running
   line-based three-way merge cleanly produces a tree *differing* from the
   recorded tree. This is the honest measure of "line merge silently did
   something the human did not do," and it is non-trivial because replay tooling
   and repository settings differ from the original merge.
3. **Resolved-conflict baseline**: over merges where replayed line merge
   conflicts, the human resolution is available as ground truth; this population
   is the correct comparator for G4.3-b from Challenge 1.

G4.3's "≤ line baseline" clause is evaluated against baseline (2), which is the
only one of the three that is both non-degenerate and a like-for-like comparison.
This is documented as a deviation in the spec-compliance audit input (G6.4).

---

## Challenge 3 — The seven-noun constraint is violated by the spec's own CLI table

**Claim challenged:** §4.2 is marked CONSTRAINT: "at most seven user-facing
nouns," enumerated as working state, change, checkpoint, line, lens, workspace,
remote. §4.2 continues: "Everything else… is plumbing behind `ltx internals` —
never required, never surfaced in normal-path output or errors." Gate G2.3 makes
this HARD and machine-checked.

**Evidence:** §4.4's own CLI table and §5.4's working model introduce at least
two further user-facing nouns:

- **Changeset.** §4.4 lists `ltx assign <changeset>` and §5.4 specifies `ltx save
  <changeset>`. A noun that appears as a command argument, that the user must
  name and select, is user-facing by any reasonable definition. It is also listed
  in ADR-11 as needing a final name — which only makes sense for a user-facing
  term.
- **Conflict.** §5.4 makes conflicts "first-class objects carrying both sides +
  base," which the user must find, inspect, and resolve. §4.3 promises lines stay
  usable while conflicts exist. A first-class object the user acts on is a noun.

Two further candidates are arguable rather than clear-cut: **provenance** and
**actor**, both surfaced by `ltx trace` (§4.4, §5.5), and **attestation** (§5.5).
If those count, the model has eleven or twelve nouns, not seven.

This is not pedantry, because G2.3 is HARD and unwaivable. Implemented
literally, the vocabulary lint would fail on the spec's own command surface, and
the only ways out are to weaken the lint (forbidden — it is a HARD gate's
harness) or to change the model.

**Resolution:** ACCEPTED — the model is corrected, not the lint. Two structural
changes, both of which I think are improvements independent of the gate:

1. **A changeset is not a new noun; it is a *change* that has not been
   checkpointed yet.** This is already true in the spec's own data model —
   §4.2's "change" is "a logical unit of work with a stable ID," which is exactly
   what a changeset is. So `ltx assign` assigns hunks to a *change*, and `ltx
   save <change>` checkpoints one. The partial-save capability §2 demands is
   fully preserved; one noun disappears; and the user learns one concept
   ("change") that behaves the same before and after it is checkpointed. This is
   also closer to jj's model, where a change exists before it is anything else.
2. **Conflict is a *state*, not an object, in user-facing language.** The
   underlying first-class object is unchanged — §5.4's design is right and is
   kept exactly. But the surface says a *checkpoint* (or a *change*) "has
   conflicts," the way a file has a size. Users act on the change; the conflict
   is a property they resolve. `ltx internals conflict …` exposes the object for
   tooling.

Provenance, actor, and attestation are resolved as **adjectival metadata on a
checkpoint**, never as standalone nouns in normal-path output: `ltx trace` answers
"who wrote this, and was it reviewed," and its output labels are attributes, not
entities. The vocabulary lint's noun list is therefore exactly the seven, and the
lint is enforced against help text, docs, and every error string — including the
ones this resolution creates.

---

## Challenge 4 — `ltx verify` is undefined on a partial clone, which is the v1 default

**Claim challenged:** §5.1 states "`ltx verify` walks the full structure" and
"everything reachable from a signed root." §5.6 states "**Initial clone defaults
to partial in v1** — history metadata + working set up front, content fetched
lazily." Gate G1.1 requires "full reference-repo verify once per stage."

**Evidence:** These cannot both be true on the same repository. On a partial
clone the content chunks are, by design, absent; walking "the full structure"
would either fail on every unfetched chunk or silently fetch the entire store,
which defeats partial clone and would make G5.8's ≤ 25% initial-transfer target
meaningless the first time anyone verified.

The gap has teeth beyond definitional tidiness. "No data loss, ever" is the
spec's fourth rule of engagement, and `ltx verify` is the instrument that
substantiates it. If the default installation state is one in which verify cannot
run to completion, then the guarantee most users can actually check is weaker
than the guarantee advertised — and worse, nobody would notice, because verify
would report success on the subset it happened to hold.

**Resolution:** ACCEPTED — verify is specified as two operations with distinct,
honestly-named guarantees, and the weaker one may never report the stronger one's
result.

- **`ltx verify`** (default, works on any clone): verifies the complete Merkle
  *spine* — the op-log's hash chain, every checkpoint, every directory tree, and
  every chunk-tree node — plus the content of every chunk actually present
  locally. It reports coverage explicitly: *"verified 100% of history structure;
  content verified for 12,418 of 96,331 chunks (12.9%); 83,913 chunks not present
  locally."* Structure is fully verifiable on a partial clone because the spine is
  exactly what partial clone retains.
- **`ltx verify --complete`**: fetches whatever is missing and verifies
  everything. Required for G1.1's once-per-stage full verification, and the only
  form permitted to print an unqualified "verified."

The distinction is enforced by the JSON contract (G2.5): the verify result object
carries `structure_verified`, `chunks_verified`, `chunks_absent`, and
`complete: bool`. A caller cannot mistake partial verification for complete
verification without ignoring a field named `complete`.

---

## Challenge 5 — Every latency gate assumes the daemon, and no gate measures life without it

**Claim challenged:** Gate G1.5 specifies "p95 < 100 ms (**watcher resident**)."
G1.6, G1.7, G4.6 and G5.4 state targets without qualifying the daemon's presence.
§4.3 promises "honest degradation." ADR-4 is explicitly left open: "Daemon:
mandatory for perf targets or optional accelerator — **what degrades without
it?**"

**Evidence:** ADR-4 asks the right question and no gate answers it. As written,
Lattice could ship a product that meets every timing target with a resident
daemon and is unusable without one, and the scorecard would be entirely green.
That is a real risk rather than a hypothetical: continuous auto-snapshotting
(§5.4) is watcher-driven, and a cold `ltx status` on a 100k-file repository
without a warm index means a full filesystem walk — hundreds of milliseconds to
seconds, an order of magnitude past the gate.

It also interacts badly with the adoption story. The Git bridge exists so that
"one developer, inside a Git team, full benefit, zero coordination" (§5.6). A
developer who must run a background daemon has coordination costs: corporate
endpoint software, container and CI environments where no daemon persists, remote
development over SSH, and Windows service policy. The wedge is blunted precisely
in the environments where adoption is hardest.

**Resolution:** ACCEPTED — a daemonless measurement is added to every timing
gate's harness, reported in the scorecard as a second column, and ADR-4 is
decided against the data rather than in advance. Concretely: G1.5, G1.6, G1.7,
G4.6 and G5.4 each emit both `p95_daemon_resident` and `p95_daemonless`. The
gate's pass/fail remains governed by the spec's stated target, applied to the
daemon-resident number, because that is what the spec says and I may not
re-scope a gate upward on my own authority either.

But the daemonless number is published in every scorecard, and ADR-4 commits to
a hard product constraint derived from it: **no command may become
*unavailable* without the daemon, and any command more than 5× slower
daemonless must say so in its own output**, naming the daemon as the remedy —
which is exactly what G2.4's machine-checked "recovery action" requirement is
for. If the daemonless path proves catastrophically slow, that is a finding the
stakeholder sees, not one the scorecard hides.

---

## Challenge 6 — "Audit-grade provenance" is a stronger claim than the threat model can support

**Claim challenged:** §1 names user group (3) as "teams needing **audit-grade**
provenance of AI-authored code." §5.5's threat model states the opposite bound:
"the goal is reliable attribution among cooperating actors and after-the-fact
tamper evidence — not DRM; **a malicious local actor can claim to be human**."

**Evidence:** Both statements are individually defensible and they do not fit
together in a user's head. "Audit-grade" is a term of art that, to the compliance
and assurance audience it names, connotes evidence admissible against an
adversarial party — the exact case the threat model excludes. An auditor asking
"can this repository prove no AI wrote this code?" gets the answer "no, it can
prove that whoever held this key *said* a human wrote it, and that the claim has
not been altered since."

That weaker property is genuinely valuable, and I want to be precise that this
challenge is about the claim rather than the design: signed, tamper-evident,
queryable attribution is a real improvement over a `Co-Authored-By` trailer
anybody can type. The failure mode is a team adopting Lattice for a compliance
obligation it cannot discharge, discovering this during an audit, and concluding
the tool misled them. That is reputational damage to the differentiating feature.

**Resolution:** ACCEPTED — the claim is restated wherever it appears, and the
constraint is pushed into the product surface rather than left in a design
document nobody reads. The user-facing formulation is **"verifiable attribution
and tamper-evident review history"**, never "audit-grade" unqualified.

Concretely, three product commitments follow, all gated: (a) §5.5's "metadata
honesty" requirement is implemented as a hard display rule — a signed fact and a
self-reported claim never render with the same visual weight, and the JSON
contract types them as distinct fields (`verified` versus `asserted`), so no
downstream tool can flatten them by accident; (b) `ltx trace` output always
carries the signature status of each record, and an unsigned record is labelled
`unverified`, not left blank; (c) the threat model ships as user-facing
documentation, not an internal note, and the G6.2 security-review kit is
instructed to attack the gap between what the UI implies and what the signatures
prove. Gate G5.2 already measures the mechanism — "forged actor claims: 100%
detected or **correctly surfaced as unverified**" — and that second clause is
where this resolution lives.

---

## Challenge 7 — Redaction under sync is a censorship and denial-of-service vector, and no gate tests it adversarially

**Claim challenged:** §2(b) specifies redaction as "content replaced by signed
cryptographic tombstones preserving Merkle structure and audit trail." ADR-5 asks
whether "advisory between honest peers" is acceptable. Gate G5.7 measures that
"redaction propagates per ADR-5 semantics; post-redaction verify passes on all
peers; audit trail complete."

**Evidence:** G5.7 measures that redaction *works*. Nothing measures what happens
when redaction is used as a weapon. The attack is direct: a peer with write
access issues redaction tombstones for content that has no reason to be redacted
— a rival's commits, an inconvenient audit trail, or, at volume, a large fraction
of the store. Because tombstones preserve Merkle structure and carry valid
signatures, `ltx verify` passes on every peer afterwards. The repository is
verifiably intact and the content is gone.

This is materially worse than the force-push it replaces, in one specific
respect. A force-push is loud, local, and recoverable from any peer that has not
yet fetched. A tombstone is *designed* to propagate and to survive verification,
and §2(b)'s framing ("loud and logged") describes the audit trail, not a
mechanism that stops propagation or restores content.

There is also a subtler variant worth naming: because a tombstone is the only
legitimate reason for content to be absent while structure verifies, a peer that
simply *lacks* content (partial clone, per Challenge 4) and a peer whose content
was *destroyed* are distinguishable only by the presence of a valid tombstone. An
attacker who can strip tombstones makes destruction look like laziness.

**Resolution:** ACCEPTED — three changes, and one deliberate limitation stated
plainly.

1. **Redaction is never silently applied on receipt.** A tombstone arriving over
   sync for content a peer holds is *quarantined*: recorded in the op-log,
   surfaced by `ltx status`, and applied only on explicit local acceptance or a
   pre-configured policy naming the authorised redactor keys. This is the
   difference between an advisory protocol and an executable one, and ADR-5 now
   decides for advisory-with-explicit-acceptance.
2. **Redaction requires an authorisation policy, committed and signed**, listing
   the keys permitted to redact. A tombstone signed by a key outside the policy
   verifies as a *signature* but is rejected as an *authorisation*, and says so.
3. **Gate G5.7's harness is extended with an adversarial population**: spurious
   tombstones from unauthorised keys, tombstone-stripping, replayed tombstones
   from an older op-log segment, and mass-redaction floods. The measured value
   counts failures across both the honest and adversarial populations, so the
   gate cannot pass by handling only the cooperative case. This is a strictly
   stricter harness under §0.3 and is recorded as such.

The stated limitation: none of this defends against a peer that is *authorised*
to redact and does so maliciously. That is a key-management and governance
problem, and Lattice's answer is the audit trail — every redaction names its
redactor and its time, permanently and unforgeably. §8 forbids novel
cryptography, and inventing a threshold-authorisation scheme here would violate
it. This limitation is documented in the threat model and handed to G6.2.

---

## Challenge 8 — "Universal undo" cannot mean what a user will assume it means for `sync`

**Claim challenged:** §4.3, marked CONSTRAINT: "**Universal undo:** every
state-changing command undoable via `ltx undo`, including merges, **syncs**, lens
edits, and undo itself." Gate G1.3 makes this HARD, with an equality domain of
"user-visible state — working-tree bytes, checkpoint graph, lines, changesets."

**Evidence:** The equality domain is local, and correctly so — G1.3 can only test
the local repository. But `sync` is the one operation in the list whose effects
are *not* local. After `ltx sync` pushes checkpoints to a peer, `ltx undo`
restores local state and the peer keeps everything it received. The gate passes,
legitimately, and the user's mental model — "undo puts things back" — is wrong in
the one case where being wrong is most expensive, because the content has left
the machine.

The spec is not confused about this; §5.6's *retraction* operation is precisely
the honest mechanism, and it is well designed ("lens hides it; record keeps it").
The gap is that nothing connects the two: `ltx undo` after a sync is specified as
undoable, and a user who runs it has no reason to know that retraction is a
separate, additional thing they must do, or that it is not equivalent.

This matters most for the accidental-secret case, which §2(b) already recognises
as real enough to justify an entire redaction protocol. A user who commits a
secret, syncs, and then reaches for `ltx undo` is in exactly the situation the
redaction protocol exists for, and the command they will reach for first is the
one that quietly does not help.

**Resolution:** ACCEPTED — undo of a sync remains fully undoable locally, as the
gate requires, and is required to state its own boundary. `ltx undo` of a sync
operation prints, and includes in its JSON result, what was *not* undone: the
peer that received the data, the checkpoints it holds, and the two commands that
actually address remote state (`ltx retract`, and `ltx redact` when the content
must be destroyed rather than hidden).

This is enforced mechanically rather than by good intentions: G2.4 already
requires every error path to carry "a recovery action, a causal category, and the
§4.2 concept involved," machine-checked. The same check is extended to the undo
result of any operation with remote effects, so a sync-undo that fails to name
its remote residue fails G2.4 — a HARD gate. The `--json` schema gets a
`remote_effects_not_undone` array, present and empty for purely local undos.

---

## Challenge 9 — G1.9(b)'s 1.5× bar against `git gc --aggressive` on source text is the likeliest gate to fail, and CDC is the reason

**Claim challenged:** Gate G1.9(b): "Text portion of reference repo: store ≤ 1.5×
a `git gc --aggressive`-packed clone (pinned git version)." §5.1 requires that
"**All** file content passes through content-defined chunking."

**Evidence:** These two requirements pull against each other, and the prior art
says so. Git's aggressive repack performs delta compression across *all* objects
in a window sorted by path and size, finding cross-file similarity between
related source files and long delta chains across a file's history. Content-
defined chunking is a fundamentally different strategy: it wins decisively on
large files with localised edits, and it is structurally disadvantaged on corpora
of many small text files, where per-chunk metadata (hash, offset, length, index
entry — on the order of 40-60 bytes) is amortised over chunk payloads that, at a
typical 8-16 KiB average chunk size, most source files never reach. A 2 KiB
source file is one chunk; the CDC store gains nothing over whole-file compression
and pays index overhead, while git may delta it against a sibling.

This is the "small-file crossover point" that G1.9 itself asks Lattice to report
and that ADR-2 must resolve — so the spec already knows the hazard exists. My
disagreement is narrower: I think 1.5× is optimistic rather than unreachable, and
I would rather say so now, in writing, than discover it at waiver time and
appear to be retrofitting an excuse.

**Resolution:** ACCEPTED-AS-BET. I proceed with the target as stated and do not
seek to re-scope it. The bet: a hybrid store meets ≤ 1.5× on source text without
abandoning §5.1's "all content through CDC" constraint, by making the *chunking
parameters* content-adaptive (small files land in a single chunk and are then
delta-compressed against similar chunks within a pack, recovering git's
cross-file delta advantage) rather than by exempting small files from chunking.

**Falsification condition, pre-registered:** if, after ADR-2's parameter sweep
and a pack-level delta-compression implementation, the measured ratio on the
reference repo's text portion exceeds **1.5×** and no untried credible strategy
remains, this bet is lost. In that event G1.9 enters §0.5 plateau review as a
SOFT perf gate, and — per the §0.5 cap — an achieved value worse than **2×
target (i.e. 3.0× git)** makes the gate unwaivable and forces redesign. I record
now, before measuring, that I expect the final value to land between **1.15× and
1.45×**, and that a value above 1.6× should be read as evidence that the
all-content-through-CDC constraint itself deserves re-examination rather than
that the parameters were merely mistuned.

---

## Challenge 10 — §0.9's "independent instances" are not independent in the way the gate needs

**Claim challenged:** §0.9 requires that personas (G2.1) and adversarial
reviewers (G6) be "instantiated as a separately spawned agent instance or fresh
session whose *entire input* is: the committed materials that gate specifies…
The instance must have no access to your working context or reasoning."

**Evidence:** The mechanical requirement is satisfiable and I satisfy it: fresh
instances, frozen prompt templates committed before the consuming stage, no
shared working context, full transcripts committed. What cannot be satisfied is
the *purpose* behind it. Context isolation defends against one instance
inheriting another's conclusions. It does not defend against shared priors: a
reviewer instance of the same model as the builder brings the same training
distribution, the same architectural intuitions, and — critically — the same
blind spots. If I am systematically wrong about, say, the failure modes of an
fsync ordering discipline, a fresh instance of me is systematically wrong about
it in the same direction, and will report clean.

For G2.1's personas this is the more damaging case, because the gate is a
*usability* study. A model instance simulating "a developer with no Lattice
experience" does not have the thing being measured — genuine unfamiliarity,
genuine impatience, genuine misreading of a help string. It can only produce a
plausible narrative of what such a person might do. The spec is clearly aware of
this, which is why G2.1 is the autonomous *floor* and G2.2 requires real humans.
The negative control G2.1 mandates (personas against a degraded doc set must
complete ≥ 30 points lower) is a genuinely clever guard against the study having
no discriminative power at all, and I am adopting it exactly as written.

**Resolution:** ACCEPTED-AS-BET, with the limitation documented rather than
engineered away. I run the §0.9 instances as specified, commit every transcript,
and additionally record in each transcript's header that the instance shares a
model family with the builder — so that a later reader weighting the evidence
knows what kind of evidence it is. The G6 reviewer briefs are written to
counteract shared priors where a prompt can: each lens is given a *concrete
attack inventory* drawn from the prior-art failures (§3) and from the canary
pool, rather than an open invitation to "find problems," because an inventory
constrains the search to regions a shared prior would skip.

**Falsification condition, pre-registered:** the canary mechanism §0.9 already
mandates is the test. If, across G6 rounds, reviewer instances find ≥ 4 of 5
planted canary defects (the round-validity bar) but the external human security
review commissioned under G6.2 subsequently reports **any** CONFIRMED
critical/high finding in a class the canary pool represented, then instance-based
review demonstrably failed to substitute for independent review in that class,
this bet is lost, and the delivery's G6 evidence must be labelled accordingly in
the final report.

---

---

## Challenges 11–16 — from an independent spec review

The six challenges below were **not found by me**. The stakeholder commissioned
an independent review of the original brief and supplied its findings; these are
the ones that survived checking, restated in this memo's format so they carry the
same obligations as the rest. Provenance is recorded because a memo that quietly
absorbs someone else's findings misrepresents who reviewed what — and because
§0.9's whole concern is knowing what kind of evidence a finding is.

The same review raised claims I checked and rejected; they are recorded at the
end of this section with the evidence against them, because a disagreements memo
that only records agreements is not doing its job either.

---

## Challenge 11 — §0.9 lets a HARD gate be downgraded on the agent's own say-so

**Claim challenged:** §0.9 concludes: "If your environment cannot spawn instances
whose context you do not share, those gates convert to [S]: deliver the kit."
§0.5 states "**HARD gates can never be waived.**"

**Evidence:** The conversion clause is an unguarded escape hatch through the
strongest rule in the protocol. The gates §0.9 governs are G2.1 and **all of
G6** — including G6.1 (data-loss hunter), G6.2 (security) and G6.4 (the
spec-compliance audit that is supposed to catch exactly this kind of evasion).
Every G6 gate except G6.2 is HARD [A].

The trigger is a claim about the environment, made by the party the gates
exist to audit, requiring no evidence. On that unverified claim, a HARD [A] gate
becomes [S], and an [S] gate reaches PASS-PENDING-HUMAN on delivery of a kit —
which §0.1 makes "sufficient for stage CLEAR and for both delivery states." So a
run could reach CHAMPION with every adversarial review unperformed, having
asserted once that it could not spawn instances. G6.4, the gate that would notice,
is itself convertible by the same clause.

I want to be clear that this did not arise here: 22 independent instances were
spawned for G0.1 alone, so the clause never fired. The hole is in the protocol,
not in this run, and it is worth closing before a later run under different
conditions finds it.

**Resolution:** ACCEPTED — conversion becomes waiver-equivalent, not assertion-
equivalent. Three requirements, all mechanically checkable:

1. **A committed incapability reproducer.** A script that demonstrates the
   spawn failure on the reference environment, committed alongside its output,
   exactly as §0.1 requires of any other claim ("a claim without harness output
   attached is a lie").
2. **An ADR** classifying the conversion, and a **G6.4 ruling** of VALID or
   INVALID on it — the same adjudication §0.5 waivers receive.
3. **G6.4 is non-convertible.** The auditor may not convert itself. If G6.4
   cannot run as [A], the delivery is EXHAUSTED at best, never CHAMPION.

Additionally, a converted HARD gate counts toward **EXHAUSTED only**. This
repository already implements part of the guard by construction: gate class is
frozen in `harness/gates.toml`, so converting one requires editing the registry,
which is a re-scope under §0.4 and needs an ADR. That was accidental rather than
designed; it is now deliberate, and `harness/lib/validate_registry.py` enforces
the non-convertibility of G6.4.

---

## Challenge 12 — "every state-changing command is undoable" is false for redaction and thinning, and must be

**Claim challenged:** §4.3, marked CONSTRAINT: "**Universal undo:** every
state-changing command undoable via `ltx undo`." §2(b) specifies redaction for
"secrets get committed; GDPR exists." §2(c) specifies policy-driven thinning.

**Evidence:** Challenge 8 addressed `sync`. Two harder cases were missed, and
they run the opposite way — there the promise was too weak, here it is too
strong.

**Redaction must not be undoable.** Its entire purpose is destroying content
that must not exist: a leaked credential, or personal data under a GDPR erasure
request. A working `ltx undo` immediately after `ltx redact` would resurrect it.
Worse, it would make the erasure claim false in a legally consequential way — a
controller who says data was erased, from a system that can un-erase it on one
command, has not erased it.

**Thinning is not undoable either.** §2(c) defines thinning as deleting ephemeral
data under policy; the data is gone. The op-log records that it happened, which
is audit, not reversal.

So the CONSTRAINT as written is not merely hard to implement — it is a promise
that must be broken for two operations, and G1.3's equality domain quietly
sidesteps this by excluding ephemeral autosnapshots without ever saying that
redaction is outside the guarantee.

**Resolution:** ACCEPTED — undo is scoped precisely, and the exclusions are
enumerated in the gate rather than left implicit.

- `ltx undo` is **local causal undo**: it reverses the local effect of an
  operation on user-visible state.
- **Redaction and thinning are explicitly non-undoable**, and both say so at the
  point of use — `ltx redact` requires confirmation naming the irreversibility,
  and its own output states that undo will not restore the content.
- **Undo of a sync** is a compensating retraction, per Challenge 8.
- **G1.3's harness enumerates the non-undoable set** and asserts that (a) every
  command outside it is undoable, and (b) every command inside it *refuses*
  undo with an explanatory error rather than silently doing nothing. A
  silently-ineffective undo is worse than a refused one.

The list of non-undoable operations is frozen in the harness, so adding a third
one later is a visible change to a HARD gate rather than a quiet exemption.

---

## Challenge 13 — the operation log grows without bound and nothing in the spec stops it

**Claim challenged:** §5.3 defines the op-log as "append-only, Merkle-linked
record of every repo-level operation." §2(c) identifies unbounded growth as a
real problem and answers it with three durability tiers and policy-driven
thinning.

**Evidence:** §2(c)'s answer covers **snapshots**. It does not cover the op-log.
Every `ltx` invocation appends; §5.4's continuous auto-snapshotting appends on a
watcher debounce; and undo appends rather than truncating, since undo is itself
an operation. On an active repository this is a monotonically growing metadata
structure with no specified bound, and it is on the read path of `ltx undo` and
`ltx trace`.

The spec appears to assume compaction exists without specifying it: §6's coverage
contract requires fault injectors to "demonstrate hits in every declared critical
section (store write, **compaction**, thinning, merge, sync)." Compaction is
named as a critical section that must be fault-injected — but no ADR defines it,
no gate measures the log's size, and §7's ADR list has no entry for it. A
subsystem exists in the test plan and nowhere else.

**Resolution:** ACCEPTED — a new ADR is required before G1 harness freeze
(**ADR-13, op-log compaction and retention**), specifying epoch snapshots plus
archive packs: the live log holds operations since the last epoch; older segments
compact into an archive pack that preserves the Merkle chain and remains
verifiable; **redaction tombstones are never compacted**, since their audit value
is precisely their permanence.

A corresponding gate is added: **live op-log size must be bounded** under the
G2.8 year-long retention traces, measured rather than asserted. G2.8 already
simulates a year of usage and already checks that no checkpointed data is
collected; extending it to assert a bound on log growth costs one more assertion
over a trace that is already being generated.

---

## Challenge 14 — §0.5's plateau formula grants a waiver to a gate that is actively getting worse

**Claim challenged:** §0.5, plateau condition 3: "improvement over the last 3
iterations — the fraction of the remaining gap closed,
max(0, (|v_{i−3} − target| − |v_i − target|) / |v_{i−3} − target|) — is < **2%**."

**Evidence:** Work the arithmetic for a gate that is regressing. If `v_i` is
further from target than `v_{i−3}`, the numerator is negative, `max(0, ·)`
clamps it to 0, and 0 < 2% — so the plateau condition is **satisfied**. A gate
moving steadily in the wrong direction qualifies for a waiver on exactly the same
footing as one that has genuinely converged.

The `max(0, ·)` was presumably meant to stop a negative improvement from reading
as an improvement. Its actual effect is to make regression and convergence
indistinguishable at the point where the protocol decides whether to stop trying.
This is the one place in an otherwise well-designed protocol where the honest
outcome and the convenient outcome are scored identically, which is precisely
where a gaming pressure would concentrate.

**Resolution:** ACCEPTED — an anti-stall rule, enforced in
`harness/lib/gauntlet.py`'s `validate_waiver`:

- The waiver is **invalid if `v_i` is worse than `v_{i−3}`** by more than
  measurement noise. A regressing gate has not plateaued; it has broken, and the
  response is diagnosis, not a waiver.
- Each of the ≥5 targeted iterations must **link a committed change** (a code or
  configuration commit) and name the gate it targeted. Iterations that changed
  nothing do not count toward the threshold, which closes the variant where five
  re-measurements of an unchanged system satisfy the protocol.
- The **direction of travel is reported in the waiver** alongside the achieved
  value, so a reader sees whether the gate was converging or drifting.

---

## Challenge 15 — "deleting every lens loses zero information" is not true as stated

**Claim challenged:** §5.3, marked CONSTRAINT: "lenses are read-path only;
deleting every lens loses zero information." §5.3 also describes lenses as
"named, versioned, shareable **data** — declarative rules."

**Evidence:** The two sentences contradict each other. If a lens is data that a
user authors, names, versions and shares, then deleting it destroys that data.
What survives is the *history*; what is lost is the user's curation of it — which
may represent real effort and, for a shared `releases` lens, a team-wide
convention.

This matters more than a wording quibble because G2.7 is a HARD gate that
property-tests the claim over ≥10,000 generated cases. A harness written against
the literal sentence would have to assert that deleting a lens loses nothing,
which is false, so the harness would either be wrong or would quietly test
something narrower than the sentence says.

**Resolution:** ACCEPTED — the constraint is restated to say what it actually
guarantees, which is the property that matters: **deleting every lens loses zero
information about the recorded history; the raw graph is unchanged and every
lensed view is reconstructible from it.** Lens definitions are themselves user
data, are stored durably, participate in sync, and are recoverable by `ltx undo`
like any other user-authored object.

G2.7's harness tests the restated property in two parts: that the raw graph is
byte-identical before and after lens deletion, and that any lensed view
reconstructs bit-identically from the raw graph plus the lens definition. That is
strictly more than the original sentence asked for, since it also pins the
reconstruction.

---

## Challenge 16 — no gate checks that Lattice may legally ship

**Claim challenged:** §8: "Apache-2.0 (check tree-sitter grammar licenses)."
§5.1 names RocksDB as a store-backend candidate; §5.2 requires 5–8 tree-sitter
grammars; §8 permits gitoxide as a component.

**Evidence:** §8 recognises the risk — it explicitly flags grammar licences — and
then leaves it as a parenthetical with no gate behind it. The exposure is
concrete: tree-sitter grammars are individually licensed and several widely-used
ones are not permissive; RocksDB is dual-licensed GPLv2/Apache-2.0; libgit2 is
GPLv2-with-linking-exception, which is why `docs/prior-art/rust-git-implementations.md`
was briefed to address the licence question as a hard constraint.

Every other project-level obligation in this brief has a gate. This one has a
parenthesis. A licence violation discovered after launch is not a bug that can be
fixed by a patch release — it can require removing a language, or a backend, from
a shipped product.

**Resolution:** ACCEPTED — a **licence-compatibility gate** is added to the
registry as HARD [A], measured by `cargo-deny` plus an SBOM over the full
dependency tree including tree-sitter grammars, with an allowlist of licences
compatible with Apache-2.0 distribution. Value = count of dependencies outside
the allowlist; target 0.

This is a gate the brief does not contain, so adding it is a scope *addition*
rather than a re-scope, which §0.4 does not forbid — it forbids re-scoping
downward. It is recorded in `STAKEHOLDER/001` as an addition for the
stakeholder's awareness, since adding gates on my own authority is a liberty and
should be visible as one.

---

## Rejected claims from the same review, with evidence

Three findings from the independent review did not survive checking. They are
recorded here rather than dropped, because a review's misses are as much part of
the audit trail as its hits.

**"G1.9's `git gc --aggressive` baseline is a strawman."** Rejected, with
measurement. On 400 commits of `django/django` (146,344 unique blobs, 2,924 MiB
of raw content) an aggressive repack produced **108.5 MiB — a 26.9× compression
ratio** — and beat naive content-defined chunking by **2.83×**. It is the hardest
baseline in the brief, not a soft one; see `docs/adr/adr-2-chunk-parameters.md`
and `bench/results/raw/adr2-git-baseline-django.json`. The claim is the opposite
of what the data shows.

**"The *perf* markers are never placed (§0.5 → §6)."** Rejected. §6 places them
explicitly: "Perf gates … G1.5–G1.9, G4.6, G5.4, G5.8; of these, G1.5–G1.7, G4.6,
and G5.4 are timing gates for §0.4's tolerances."
`harness/lib/validate_registry.py` asserts the registry's perf and timing sets
match those lists exactly, and CI fails if they diverge.

**"G4.1's ≥99% entity-match precision is unpassable and mandates failure
analysis."** Rejected as stated. §5.2 mandates the escape that makes it
reachable: "below-threshold confidence ⇒ record 'new entity', never guess."
Precision is therefore tunable against recall by construction — a matcher can
always raise precision by declining more matches — and recall is the SOFT gate at
75%. The real risk is that G4.2's recall target becomes unreachable while G4.1
holds, which is a different finding with a different remedy, and it is why G4.2
requires the full precision/recall operating curve in the scorecard so
threshold-cranking is visible.


## Summary

| # | Challenge | Resolution | Changes |
|---|---|---|---|
| 1 | G4.3 measures the safe population, not the dangerous one | ACCEPTED | measurement (adds G4.3-b) |
| 2 | The line mis-merge baseline is ~0 by construction | ACCEPTED | measurement (G0.3 oracle) |
| 3 | Seven-noun constraint violated by the spec's own CLI | ACCEPTED | product (changeset → change; conflict → state) |
| 4 | `ltx verify` undefined on the default partial clone | ACCEPTED | product (`verify` vs `verify --complete`) |
| 5 | No gate measures the daemonless path | ACCEPTED | measurement + ADR-4 constraint |
| 6 | "Audit-grade" overclaims vs. the threat model | ACCEPTED | product (claim + display rules) |
| 7 | Redaction is a censorship vector; G5.7 tests only honest peers | ACCEPTED | product (quarantine + authorisation policy) + stricter harness |
| 8 | Undo of `sync` does not mean what users will assume | ACCEPTED | product (`remote_effects_not_undone`) |
| 9 | G1.9(b)'s 1.5× text bar fights the all-CDC constraint | ACCEPTED-AS-BET | falsifiable at 1.5×; expect 1.15–1.45× |
| 10 | §0.9 instances share the builder's blind spots | ACCEPTED-AS-BET | falsifiable via G6.2 external review |
| 11 | §0.9 lets a HARD gate be downgraded on bare assertion | ACCEPTED | protocol (conversion becomes waiver-equivalent; G6.4 non-convertible) |
| 12 | Undo cannot cover redaction or thinning, and must not | ACCEPTED | product (undo scoped; non-undoable set frozen in G1.3) |
| 13 | The op-log grows without bound; compaction is untested and unspecified | ACCEPTED | new ADR-13 + a log-size bound measured by G2.8 |
| 14 | §0.5's plateau formula grants waivers to regressing gates | ACCEPTED | protocol (anti-stall rule enforced in the runner) |
| 15 | "Deleting every lens loses zero information" is false as stated | ACCEPTED | restated; G2.7 tests strictly more |
| 16 | No gate checks that Lattice may legally ship | ACCEPTED | new HARD licence-compatibility gate |

Thirteen challenges change what is built, how it is measured, or how the protocol
governs itself. The measurement changes make gates **stricter** than the spec's
literal text (Challenges 1, 5, 7, 11, 12, 14, 15); none makes any gate looser,
which §0.4 would forbid for HARD gates in any case. Two new gates are added
(Challenges 13 and 16), which is a scope addition rather than a re-scope.

Two are bets I may lose, and both say in advance what losing looks like.

Challenges 1–10 are mine. Challenges 11–16 came from an independent review the
stakeholder commissioned; three further claims from that review were checked and
rejected, with the evidence recorded above. **Of the ten findings that review
raised, five had already been found independently in Challenges 1–8** — the noun
cap (3), undo-vs-sync (8), the G4.3 dual-AND (1, 2), redaction under sync (4, 6,
7), and the daemon question (5) — which is a reasonable calibration signal for
both reviews.
