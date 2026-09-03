# Unison

Unison is a statically-typed, purely functional language whose defining bet is that **source code should not be stored as text at all**. The `unisonweb/unison` repository was created 2015-05-06 by Paul Chiusano; development is carried by Unison Computing, a Delaware public benefit corporation headquartered in Somerville, MA, founded 2016 and cofounded by Chiusano, Rúnar Bjarnason and Arya Irani. Unison stores every definition as a typed syntax tree keyed by a 512-bit SHA3 hash of its *structure*, with human-readable names held in a separate, mutable namespace. Version 1.0.0 shipped 2025-11-25, ten and a half years after the first commit; releases continue through 1.4.0 (2026-08-19). It is the most complete existing implementation of content-addressed code, and therefore the most complete existing experiment in making the AST authoritative — precisely the design Lattice has already ruled out.

*Caveat on sourcing:* `docs/codebase-editor-design.markdown` is a historical design document. The `patch` and root-namespace model it describes has since been replaced by projects/branches and the 2024 merge algorithm — `patch` no longer appears anywhere in UCM's `help` output. Claims below are marked design-doc vs. shipped where the distinction matters.

## 1. What did it get right?

### The hash is the identity, computed over structure, not text

The language reference defines a hash as "a 512-bit SHA3 digest of a term or a type's internal structure, excluding all names," and states that "the hash of a term or type is its true name." Bound variables become positional references and every dependency is replaced by *its* hash, so a definition's hash transitively pins its dependency closure. Cycles are hashed as a unit — the cycle gets hash `#x`, members are `#x.n`, and "a cycle has a canonical order determined by sorting all the members of the cycle by their individual hashes (with the cycle removed)." Constructors are `#x#c`. The design document draws the conclusion Lattice cares about: "there's no such thing as changing a definition, there's only introducing new definitions."

### Names are a separate, mutable index — and that is what buys the good behaviour

`Namespace` denotes literally `Map Name Code`. Names never participate in identity, and three wins fall out mechanically: renames "even bulk renames of whole packages of definitions, are 100% accurate and fast" because no dependent referenced the name; aliasing is free, so competing conventions coexist without forking code; and "dependency conflicts are, fundamentally, due to different definitions 'competing' for the same names," so with names demoted, "the diamond dependency problem [is] just not a thing." Because "dependencies are tracked at the level of individual definitions, not whole packages" (churn post, 2020-04-10), a downstream user upgrades only what they actually depend on.

### A typed database with a Merkle-linked history object

`Branch` denotes `Causal (Map Code (Conflicted CodeEdit, Conflicted NameEdits))`. `Causal e` is a three-constructor Merkle structure — `One`, `Cons {currentHash, head, tail}`, `Merge {currentHash, head, tail1, tail2}` — with `before` (a partial order), `head`, `one`, `cons`, a **commutative** `merge`, and `sequence`. Commutativity is load-bearing: Alice's and Bob's edits compose in either order. This is the shape of Lattice's checkpoint DAG, arrived at independently and years earlier.

Storage moved from a Git-friendly file-per-hash tree (`v1`: `terms/<hash>/compiled.ub`, `dependents/<hash>/<dependent-hash>`, with the explicit note "git merge of `dependents` does the right thing") to a SQLite schema (`v2`) with `hash`, `text`, `object`, `causal` and `causal_parent` tables. One v2 detail is worth stealing outright: serialized objects carry a `LocalIds` lookup array mapping local indices to global `TextId`/`ObjectId` values, so that "when a definition is transferred between codebases, only the lookup array is rewritten, not the ABT itself." That is how you make a content-addressed blob portable across stores without touching its body.

### No build step, and exactly-correct test caching

"Unison does not have builds and therefore doesn't generate build artifacts." Test results are cached on the hash of the test expression, and the tour claims "the dependency tracking for determining whether a test needs rerunning is 100% accurate and is tracked at the level of individual definitions." A rename cannot invalidate a cached test result, because the rename did not change the hash. This is the strongest single argument for entity-level identity in a VCS: it converts incremental work from a heuristic into a lookup.

### Refactoring as a measurable frontier — with a shipped caveat

The design document formalises progress: a branch *covers* a codebase when every dependent of a type-changing edit also has an edit and none conflict; remaining work is the transitive-dependent count of *escaped dependents*, and "this number will decrease monotonically as the Branch is developed." `todo` ships this as an ordered worklist — it prints "I recommend working on them in the following order." But Unison's own post lists, among the old workflow's defects, that "the suggested order of edits sometimes leads you in a circle." A monotone metric on paper did not yield an acyclic worklist in practice. Steal the framing; do not assume the ordering is free.

## 2. Why didn't it win (or why is it niche)?

Unison is alive, shipping, and runs Unison Cloud's orchestration layer on itself. It is also niche: 6,720 stars, 308 forks, 1,219 open issues plus 49 open PRs (GitHub's `open_issues_count` of 1,268 conflates the two), and 123 listings covering 118 unique projects across 14 categories in Unison Share's *curated* catalog — a curated subset, so a floor rather than a census. After eleven years.

### Owning the language is the entry fee, and the identity function still isn't total

Content addressing demands a parser, typechecker, hashing spec, runtime, storage engine, package host and code-review tool that all agree on one canonical representation. Unison built all of them. As LWN put it: "Since Unison code is not stored as text files, but rather as a database, the community can't really reuse existing tooling. Tools like Unison Share must be written from scratch." Share consequently carries "organizations, tickets, code contributions (pull requests), code review, and more" as first-party features. That is the true cost of leaving text behind.

Worse, structural hashing is unsolved even when you own the language. Issue #2787 (opened 2022-01-06, **still open**) reports that the canonical cycle ordering ignores self-referential variables, so two structurally identical members tie and the tiebreak falls through to the *alphabetical order of term names* — the very thing the hash excludes. The shipped mitigation, PR #6007 (merged 2025-12-04, nine days after 1.0.0, released in 1.0.1), does not fix it: it makes hashing **crash** via `error` when ordering is ambiguous. The author writes: "This also is the first introduction of the idea that hashing a component could *fail*; which is undesirable." The suggested workaround is to insert an inert dummy binding to perturb the hash. Identity is undetermined *by* content, and leaks a name.

### Structural storage cannot round-trip text, so it discards it

"Comments in Unison aren't actually committed to the Unison codebase" — persistent prose must be re-expressed as separately-hashed `Doc` values. Formatting is regenerated by the pretty-printer. And incompleteness is forbidden: "A Unison codebase contains no ill-typed terms." Broken code lives outside the database, in a `.u` file.

The tell is that Unison's *own* version control had to reintroduce names into a hash. PR #4910 (merged 2024-05-16) diffs namespaces by **syntactic hashes**, "in which a pretty-print environment renders references before hashing. This is the mechanism by which we distinguish 'user changes' from auto-propagated changes." PR #5716 (2025-05-30) does rename detection the same way: "determined by comparing syntactic hashes, not Unison hashes." Nine years in, pure content addressing was insufficient for version control.

### No path from existing code

There is no importer, and FFI is, per the project's FAQ, "still in its infancy" — 1.0.1 added 16- and 32-bit FFI types, 1.1.0 added more types and pointer operations, 1.1.1 added null pointers. Every library, editor integration, grep, review tool and CI system must be rebuilt or abandoned. The team is candid: the June 2024 roadmap concedes that "actually getting to a real usable technology took years," with "quality of life improvements and bugfixes that until now we've been putting off to work on major features." The October 2023 update-process post opens by calling this area "the roughest part of the experience right now."

## 3. What will Lattice do differently, concretely?

### Steal entity identity and names-as-metadata. Reject AST-as-truth.

Lattice's binding decision — bytes are the source of truth, the semantic layer is derived — is correct, and Unison is the evidence. Five failures follow directly from an authoritative AST: comments vanish; formatting vanishes, so a whitespace-only checkpoint is unrepresentable; syntax errors cannot be stored at all, which makes the ephemeral autosnapshot tier *impossible* rather than awkward, since continuous capture means capturing unparseable text by definition; unsupported content has no address (the tree-sitter org hosts 28 non-archived `tree-sitter-*` repos, and `tree-sitter-toml` and `tree-sitter-swift` are **archived** — Cargo.toml is in every Rust repo); and the parse is not deterministic across edit paths.

### The derived-index design Lattice should commit to

- **Hash independence (gate G2.1).** Checkpoint Merkle roots come from BLAKE3 chunk trees over bytes only. The semantic index lives under `.lattice/index/`, reachable only via `ltx internals`, and is never an input to any hash. G2.1: over ≥1,000 repositories, delete and rebuild the index; assert every checkpoint root is byte-identical. One mismatch fails the gate.
- **Entity IDs in a sidecar binding table.** `(checkpoint_id, path, byte_range) → entity_id`, with `confidence: f32` and `origin ∈ {heuristic, confirmed, inherited}`, stored outside the Merkle root so a rebuild may revise a heuristic binding without rewriting history. This is exactly what Unison lacks: its hashes *are* the history, so a hashing mistake is permanent.
- **Loud degradation (gate G2.4).** No grammar, or `ERROR` nodes over the changed range, ⇒ file-level granularity, `origin=heuristic, confidence=0`. `ltx trace` must emit a `coverage:` line in every result set (`semantic coverage: 71% of touched bytes; 4 files line-only`). G2.4 asserts the line is present and numerically correct on a fixture set containing at least one uncovered grammar and one `ERROR`-parse file, since `ltx trace --agent --unreviewed --touching src/auth/` is an audit surface and a silent under-report is worse than no feature.
- **Confirmations are op-log entries, and they are hard constraints.** A confirmed rename or cross-file move is an `entity.bind` operation in the append-only Merkle-linked op-log, Ed25519-signed with the same provenance record as a checkpoint. The matcher may fill gaps but may never contradict or overwrite a confirmation on rebuild. `ltx undo` covers it for free.
- **No second hash.** Unison invented the syntactic hash to tell a human edit from a propagated one. Lattice's provenance layer records actor class on every checkpoint, so this is primary data. ADR-0xx must record the trade explicitly, because the temptation recurs at every merge.
- **Borrow `LocalIds` in the chunk store.** Serialized objects carry a local→global id table so `ltx sync` rewrites only the table, never the blob body.
- **Frontier worklist (from §1), with an acyclicity requirement.** Changesets and conflict objects expose remaining work as a dependency-ordered worklist over a monotone metric. Because Unison shipped an ordering that "sometimes leads you in a circle," the worklist must be produced by a topological sort over a graph with cycles explicitly condensed (SCC-collapsed and surfaced as a unit), and a property test must assert no ordering ever revisits a node.

### Residual risk for Lattice

**Heuristic identity is worse than the best published tool, and the gap is a tooling gap Lattice cannot close.** RefactoringMiner's live accuracy page reports 0.999 precision / 0.984 recall on its 547-commit Java oracle, plus Python (0.996/0.997, 202 commits), Kotlin (0.998/1.000, 63 commits) and JavaScript (0.978/1.000, 25 commits). Those numbers rest on compiler frontends: Eclipse JDT for Java, `kotlin-compiler-embeddable` for Kotlin, Eclipse CDT for C++, SWC via swc4j for TS/JS. Its tree-sitter path is a GumTree `gen.treesitter-ng` **beta**, and its own roadmap leaves "Validate precision/recall" unchecked for Kotlin and JavaScript. Git's rename detection defaults to a 50% similarity index and stops past `diff.renameLimit` (1000 files). *Gate G2.2:* publish measured recall and precision of entity continuation on a hand-labelled corpus of ≥500 real rename/move commits across ≥6 languages, surfaced in `ltx internals index stats`, with a stated floor below which `ltx trace` refuses to report entity-level results.

**Rebuild determinism is not free, and the bug is in code Lattice does not own.** tree-sitter #4001 documents an incremental parse producing an `ERROR` node where a fresh parse does not; maintainers closed it "not planned," and the reporter himself could not tell whether the fault was in tree-sitter core or in `tree-sitter-haskell` — whose issue #129 has been open since 2024-09-22 with **zero comments**. *Gate G2.3:* incremental-vs-fresh index equivalence over a generated edit-sequence corpus.

**Provenance is a key-custody problem, not a schema problem — and nobody has solved it.** Unison ships an MCP server exposing 29 tools, 8 flagged `destructiveHint = true`, including `update`, `rename`, `move`, `delete`, `delete.namespace` and `CreateBranchTool` (added 1.1.1, explicitly "to allow AI to create branches"). Its operation log — `project_branch_reflog` — has columns `project_id, project_branch_id, time, from_root_causal_id, to_root_causal_id, reason` and **no actor column at all**; authorship exists only as `create.author`, an ordinary unsigned `Author` definition hand-attached as metadata. So the space is genuinely unoccupied, which validates Lattice's differentiator. But it also means nobody has answered the hard part: when an agent shells out to `ltx`, it holds whatever key is on the machine — usually the human's. An `entity.bind` or checkpoint signature then attests to key possession, not to actor class. *ADR required:* who may hold an agent-class key, whether an agent-signed `entity.bind` is a confirmation or only a high-confidence heuristic, and what `ltx trace --agent` does when it cannot distinguish the two.

**A queryable derived index becomes de facto authoritative, and redaction breaks it.** Unison's syntactic hash began as an internal merge helper and now *defines* what a rename is. Once `ltx trace --agent --unreviewed` is used for compliance sign-off, its output is the record for that purpose. A signed tombstone destroys the bytes the index was built from, so a post-redaction rebuild yields different bindings and two audits of the same history disagree. *ADR required:* either freeze index outputs into the op-log at redaction time, or state loudly that pre-redaction entity claims are unreproducible.

**"No force-push concept exists" is contradicted by the closest prior art.** Unison — append-only, immutable, content-addressed, Merkle-causal — ships `unsafe.force-push (or push.unsafe-force)`: "Like `push`, but forcibly overwrites the remote namespace." The most committed immutability design in existence still needed the escape hatch, and named it. Lattice's Git bridge makes this worse, not better: if Lattice→Git published history is lens-defined, changing a lens changes the generated commit hashes, and to a plain-Git teammate that is indistinguishable from a force-push. *Concrete requirement:* record a lens version per exported Git ref and refuse re-export under a changed lens without an explicit, logged operation; and an ADR stating what Lattice's own escape hatch is, since shipping without one has not worked for anybody.

**"Seven user-facing nouns, total" is not a complexity bound.** UCM's `help` lists 119 primary commands plus 67 aliases — 186 invocable names — over roughly six nouns, and `patch` was deleted outright rather than kept. Four separate reflog commands (`reflog`, `reflog.branch`, `reflog.global`, `project.reflog`, plus `deprecated.root-reflog`) accreted around one concept. Conceptual load lives in verbs, not nouns. *Replace the noun count with a verb budget gate:* a cap on `ltx` surface commands outside `ltx internals`, enforced in CI, since undo-of-undo and lens composition will each carry more load than any noun.

**Universal undo is a bigger claim than anything shipped.** UCM's `undo` "reverts the most recent change to the codebase" — single-step. Lattice promises undo of everything including merges, syncs and undo itself. Nothing in the prior art demonstrates that; treat it as unproven and gate it with an adversarial property test over generated operation sequences that include nested undos and syncs.

**Sequencing.** Unison took ten and a half years to 1.0 while *owning* the language. Lattice's semantic problem is strictly harder — N languages, no ownership, no typechecker, grammars that get archived — and is scheduled third, behind the Git bridge. Any v1 headline that depends on the semantic layer (structural merge, cross-file entity identity) should be labelled v2 in the roadmap now, not discovered as v2 later.

## Sources

- [primary] Unison codebase editor design document (historical) — https://github.com/unisonweb/unison/blob/trunk/docs/codebase-editor-design.markdown
- [primary] The big idea — https://www.unison-lang.org/docs/the-big-idea/
- [primary] Language reference: Hashes — https://www.unison-lang.org/docs/language-reference/hashes/
- [primary] Language reference: Comments — https://www.unison-lang.org/docs/language-reference/comments/
- [primary] A tour of Unison — https://www.unison-lang.org/docs/tour/
- [primary] Repo format v2 (`LocalIds`) — https://github.com/unisonweb/unison/blob/trunk/docs/repoformats/v2.markdown
- [primary] Repo format v1 draft — https://github.com/unisonweb/unison/blob/trunk/docs/repoformats/v1-DRAFT.markdown
- [primary] Issue #2787, open, 2022-01-06 — https://github.com/unisonweb/unison/issues/2787
- [primary] PR #6007, merged 2025-12-04, shipped in 1.0.1 — https://github.com/unisonweb/unison/pull/6007
- [primary] PR #4910, merged 2024-05-16 (syntactic hashes) — https://github.com/unisonweb/unison/pull/4910
- [primary] PR #5716, merged 2025-05-30 (renames by syntactic hash) — https://github.com/unisonweb/unison/pull/5716
- [primary] `unison-cli/src/Unison/MCP/Tools.hs` — 29 MCP tools, 8 `destructiveHint = Just True`
- [primary] `docs/mcp.md` — https://github.com/unisonweb/unison/blob/trunk/docs/mcp.md
- [primary] `codebase2/codebase-sqlite/sql/013-add-project-branch-reflog-table.sql` — reflog schema, no actor column
- [primary] `unison-src/transcripts/idempotent/help.md` — 119 commands + 67 aliases; `unsafe.force-push`; `undo`; no `patch`
- [primary] `unison-src/transcripts/idempotent/create-author.md` — `create.author` / `metadata.authors`
- [primary] Release notes 1.0.0–1.4.0 (GitHub Releases API, fetched 2026-09-03) — FFI additions in 1.0.1/1.1.0/1.1.1; `CreateBranchTool` in 1.1.1 (PR #6155)
- [primary] A preview of Unison's improved update process, 2023-10-20 — https://www.unison-lang.org/blog/new-update-process/
- [primary] Resolving conflicts in a branch ("temporary stop-gap") — https://www.unison-lang.org/docs/usage-topics/workflow-how-tos/resolve-conflicts-projects/
- [primary] Announcing Unison 1.0, 2025-11-25 — https://www.unison-lang.org/unison-1-0/
- [primary] Where Unison is headed, 2024-06-03 — https://www.unison-lang.org/blog/where-unison-is-headed/
- [primary] How Unison reduces ecosystem churn, 2020-04-10 — https://www.unison-lang.org/blog/reducing-churn/
- [primary] General Unison FAQs (FFI "still in its infancy"; no builds) — https://www.unison-lang.org/docs/usage-topics/general-faqs/
- [primary] GitHub REST API, `unisonweb/unison` repo + releases, fetched 2026-09-03
- [primary] Unison Share catalog API — https://api.unison-lang.org/catalog, counted 2026-09-03 (123 listings, 118 unique projects, 14 categories)
- [primary] tree-sitter org repository listing, fetched 2026-09-03 (28 non-archived `tree-sitter-*` repos; `tree-sitter-toml`, `tree-sitter-swift` archived)
- [primary] tree-sitter issue #4001, closed "not planned" 2025-01-18 — https://github.com/tree-sitter/tree-sitter/issues/4001
- [primary] tree-sitter-haskell issue #129, open since 2024-09-22, 0 comments — https://github.com/tree-sitter/tree-sitter-haskell/issues/129
- [primary] RefactoringMiner accuracy page and `build.gradle` (JDT, kotlin-compiler-embeddable, Eclipse CDT, swc4j, `gen.treesitter-ng:4.0.0-beta8`) — https://github.com/tsantalis/RefactoringMiner
- [primary] git-diff documentation (50% default similarity; `diff.renameLimit` 1000) — https://git-scm.com/docs/git-diff
- [secondary] "Programming in Unison", LWN.net, 2024 — https://lwn.net/Articles/978955/
- [secondary] Tsantalis, Ketkar & Dig, "RefactoringMiner 2.0", IEEE TSE 48(3):930–950, March 2022 (early access 2020) — https://ieeexplore.ieee.org/document/9136878/
- [secondary] Unison Computing corporate details (Somerville MA, Delaware PBC, founded 2016) — https://www.unison-lang.org/unison-computing/ and https://www.unison-lang.org/blog/benefit-corp-report/
