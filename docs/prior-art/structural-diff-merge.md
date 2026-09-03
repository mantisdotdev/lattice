# SemanticMerge, difftastic and GumTree

> Three attempts to make tooling see code as structure rather than lines, at three points on the research-to-product spectrum. **GumTree** (Falleri, Morandat, Blanc, Martinez, Monperrus; ASE 2014, Bordeaux/INRIA) is the academic AST-matching algorithm nearly every later structural tool builds on; GumTree 3.0 landed at ICSE 2024. **SemanticMerge** (Codice Software, Spain) was the only serious *commercial* structural merge product: a language-parsing three-way merge tool sold standalone and bundled with Plastic SCM. Unity acquired Codice on 17 August 2020 (~$20M); standalone SemanticMerge availability was subsequently ended, and `forum.plasticscm.com` topics now 302-redirect into Unity Version Control discussions — verified live. **difftastic** (Wilfred Hughes, 2020–present) is the tree-sitter structural *diff* that shipped at scale — 30+ languages, widely used as a `git difftool` — and which explicitly refuses to merge.

## 1. What did it get right?

### GumTree: two cheap phases beat one optimal algorithm

GumTree's contribution is a *matching* algorithm, not an edit-script algorithm. Phase one is top-down: two height-indexed priority queues are popped in lockstep, and identical subtrees taller than `minHeight` are mapped wholesale — this is what makes an unchanged 200-line method a single mapping rather than 200 line matches. Phase two is bottom-up: for each still-unmatched internal node, a candidate is accepted if `dice(t1,t2,M) > minDice` over already-mapped descendants, and if both subtrees are smaller than `maxSize` an optimal (expensive) recovery matcher runs *inside* that bounded window.

The design insight worth stealing is the containment of cost. RTED — the optimal tree-edit-distance baseline — hit out-of-memory on 82 modifications (~5%) and exceeded 10 seconds on 206 (~12% of the remainder), which the authors total as ~17% unusable. Median runtime ratios against text diff: RTED 298× (Jenkins) and 2,654× (JQuery); GumTree 18× and 30×, against 10× for *parsing alone* — the matching is nearly free once you have paid for the parse. Mean GumTree runtimes were 20 ms and 74 ms. GumTree 3.0 replaced the bottom-up optimal recovery with a cheaper heuristic and reports 50×–281× matching speedup with median edit scripts 50% smaller.

Two caveats the paper carries and secondary write-ups drop. The parameters are **not one configuration**: the accuracy experiment used `minHeight=2, minDice=0.5, maxSize=100`; the performance experiment used `minDice=0.2`. And the accuracy comparison discarded any revision pair whose trees exceeded 3,000 nodes.

### difftastic: bounded work, and fallbacks as typed values

Difftastic models diffing as a shortest-path problem over a DAG whose vertices are *pairs* of positions in the two syntax trees, solved with Dijkstra (Hughes tried A\* and "didn't see much improvement"). He is explicit about the blowup: the graph is O(L×R) in the two item counts, and correctly handling nested delimiters requires carrying parent-exit state — vertices are really `(lhs position, rhs position, list_of_parents_to_exit_together)` — making it O(2^N) in nesting depth. Vertex construction, not search, is the bottleneck, on graphs of "several million vertices"; the graph is therefore built lazily.

The escape hatches are calibrated numbers, not gestures — verified in `src/options.rs`:

- `DEFAULT_GRAPH_LIMIT = 3_000_000` vertices traversed — exceed it and fall back to line diff.
- `DEFAULT_BYTE_LIMIT = 1_000_000` — either input larger, use line diff.
- `DEFAULT_PARSE_ERROR_LIMIT = 0` — *any* parse error forces line diff.

Two details matter more than the constants. First, in `src/main.rs` the limit is applied **per changed section**, and one section exceeding it aborts the whole file. Second, the fallback is not a printed string but a typed value — `FileFormat::TextFallback { reason }`, carrying `"exceeded DFT_GRAPH_LIMIT"`, `"N parse errors, exceeded DFT_PARSE_ERROR_LIMIT"`, or `"… exceeded DFT_BYTE_LIMIT"` into the display layer. That is a reason enum attached to a result, which is exactly the shape an auditable system needs.

The history is instructive. `--node-limit` (0.19, default 50,000; raised to 100,000 in v0.20) *estimated* graph size before building it. The v0.30 changelog states the replacement's reason directly: the old flag "applied a limit based on an estimate of how big the graph would be, leading to very slow diffs when the estimate was wrong."

Difftastic's second correct decision is a stated non-goal: it does not patch and does not merge, because AST diffing is lossy — it discards syntactically insignificant whitespace — so the tree cannot be printed back as a faithful artifact. It points users at Mergiraf.

### SemanticMerge: the parser boundary as a product surface

SemanticMerge shipped built-in parsers for exactly five languages — C#, Java, VB.NET, C, C++ — and made everything else somebody else's problem via a well-specified external parser protocol. A parser is a console app invoked as `parser.exe shell <flagfile>`; it writes `READY` into the flag file, then reads triplets from stdin (source path, encoding, output YAML path) until the sentinel `end`, answering `OK` or `KO`. The YAML is a *position tree*: a file node containing containers and terminals, each with `locationSpan`, terminals with `span`, containers with `headerSpan`/`footerSpan`, plus `parsingErrorsDetected` and an optional `parsingError` list. The protocol description is corroborated independently by `sageserpent-open/SemanticMergeScalaPlugin`, which implements it against the same contract.

The binding constraint is the good idea: spans must tile the file exhaustively. The docs put it bluntly — there can be no holes between characters or the parser will not work. Because spans tile, SemanticMerge could always reconstruct bytes it did not understand, and could merge a moved-and-edited method as *one* operation while treating the text inside it as text. Community parsers exist for Scala, C#, TypeScript, F#, Delphi and Go.

### Mergiraf: escalate, and reuse the cheap pass's work

Mergiraf (Rust, tree-sitter, GumTree-classic matching, PCS-triple merging) runs cheap-first: line merge, returned immediately if clean. The refinement worth copying is what it does on failure — rather than discarding the line-merge attempt, it reconstructs fictional revisions from the *conflicted* output and feeds the already-established matchings into the structural pass, "speeding it up significantly as many nodes are readily matched."

## 2. Why didn't it win (or why is it niche)?

**SemanticMerge died of commerce, not algorithms.** Nothing in the record suggests merge quality was the problem. It was a paid, closed, per-seat tool for an operation developers hit occasionally, sold alongside a DVCS that never displaced Git, by a company acquired by a *game engine* vendor whose interest was binary assets and large-team locking. The external-parser protocol is the tell: correct engineering that relocated per-language cost onto volunteers with no other reason to maintain a SemanticMerge parser. Tree-sitter later solved that properly, by making grammars useful for syntax highlighting first and diffing second.

**difftastic is winning, in its lane.** Its niche-ness is deliberate: it cannot merge, and it degrades to line diff under three documented conditions. The author's known-issues list is candid — poor scaling on files with many changes, high memory use, "regularly has releases that fix crashes" — and the tricky-cases docs concede that the correct handling of nested delimiters differs by language ("Most languages want to prefer the inner delimiter, whereas Lisps and JSON prefer the outer"), that some behaviours are cost-model-sensitive and have flipped between versions, and that unordered tree diffing is NP-hard, so all reorderings are reported as changes.

**GumTree won academically and hit an accuracy ceiling — but not the one the draft assumed.** Two structural facts about the 2014 paper: it contains *no precision or recall numbers at all* (verified by exhaustive search of the text), and its human evaluation is 144 file pairs rated by three raters who were authors, a bias they acknowledge, drawn only from revisions with a *single* source-code change. Full agreement that GumTree "does a good job" was 122/144 (84.7%); full agreement that it beat plain diff, only 28/144 (19.4%). Its hyperparameters are not universal either: DAT (TSE 2023) finds a tuned configuration improves edit scripts in 21.8% of evaluated cases.

## 3. What will Lattice do differently, concretely?

### 3.1 Re-tier the G4 entity-match gate, and buy precision with signature anchoring

The Alikhanifard & Tsantalis benchmark — 800 Defects4J bug-fix commits (from a 17-project dataset) plus 188 refactoring commits (from a 187-project oracle) — gives per-entity precision/recall (Table 7, "program element mappings"):

| Entity | n | GT3.0 greedy P/R | IJM P/R | RefactoringMiner 3.0 P/R |
|---|---|---|---|---|
| Type | 1432 | 99.9 / 98.7 | 99.9 / 98.7 | 100 / 100 |
| Method | 2289 | 93.5 / 75.1 | **99.4 / 82.7** | 100 / 100 |
| Field | 245 | 98.5 / 52.2 | 98.2 / 44.5 | 100 / 98.8 |
| Overall | 3983 | 96.4 / 82.3 | **99.6 / 86.2** | 100 / 99.9 |

**Correction to the received wisdom: ≥99% precision is *not* RefactoringMiner-only.** IJM — a GumTree-family matcher, not a refactoring-aware one — reaches 99.4% on methods and 99.6% overall. The paper attributes this to a cheap, portable mechanism: *partial matching*, which first matches method declarations by signature alone, then matches the remainder by signature and body. Extracting a declaration's name and parameter list is exactly what Mergiraf already does per language for its signature configuration, so this is buildable on a tree-sitter CST. **ADR-SM-01: LTX's matcher performs signature-scoped anchoring before general tree matching.**

Two caveats that bound all of this. Every tool here parses with Eclipse JDT — Java only, a resolved AST, not a tree-sitter CST. And precision/recall are computed *only over elements whose AST subtree changed*; identical subtrees were excluded because all tools match them at 100%. Only 4.8% of method declarations and 1.5% of field declarations in the benchmark changed at all.

Concrete change to G4 — three separately gated tiers:
- **G4-A — file-scope named declarations** (type/class/module/enum): ≥99% precision, ≥95% recall.
- **G4-B — functions/methods**: ≥95% precision, ≥75% recall, **conditional on ADR-SM-01 shipping**; the un-anchored baseline is reported alongside. 99% is not claimed until measured on a tree-sitter CST.
- **G4-C — fields, variables, statements**: measured and reported in the G4 artifact, **not gated in v1**. Best published field recall across all six tools is 59.2%; IJM's is 44.5%.

### 3.2 The silent mis-merge gate as written is falsified by the published record

Cavalcanti/Borba/Accioly reproduced 34,030 merges across 50 Java projects: semistructured merge produced **more** additional false negatives than unstructured (3,260 vs 2,714), and 4.42% ± 5.53% of scenarios had at least one semistructured aFN versus 0.88% ± 1.08% unstructured. MergirafSemi (21,615 scenarios / 513 repos / 5 languages) measures validated wrong auto-merges against diff3: Java 110 vs 1 (110/1,479 ≈ 7.4%); Go 292 vs 9 (of 7,181); Rust 244 vs 15 (of 4,253); Python 172 vs 44; JS 44 vs 1.

So "silent mis-merge rate ≤0.1% **and** ≤ the line-based baseline" is roughly **15–70× below** the best published structural result, and the second clause runs *against* the observed direction.

**The one published mechanism that inverted this — described precisely.** Cavalcanti's improved tool (S3M) reached zero additional false positives and 2,489 aFNs against unstructured's 2,714 (~8.3% fewer). It did so *per handler*, not globally: the renaming handler "only reports conflicts if unstructured merge reports a similar one," and a second handler uses unstructured merge as an oracle to reduce false negatives. Cost: ~24 minutes versus 45 seconds for unstructured on 1,731 scenarios.

**ADR-SM-02 (structural-merge acceptance rule)** — an adaptation, not a copy: a structural resolution is written as merged bytes only if (a) line merge also produces a clean result and the two byte sequences agree, or (b) the disagreement is confined to a declared commutative parent for that language in `languages.toml`. Otherwise LTX emits a conflict object. Note honestly what this costs — see Residual risk.

### 3.3 Conflicts as objects, and an op-log oracle — with its limits stated

Lattice's binding decision that merges never block and conflicts are first-class objects carrying both sides plus base means structural merge in LTX can be a **proposal recorded as an attestation on the conflict object** rather than a rewrite of bytes. Acceptance is a later checkpoint; `ltx undo` reverses acceptance as an op-log pointer move.

OOPSLA'17 states that ground truth for integration conflicts "is not computable in this context," and that the merged-code oracle "is only available after the merge result has been committed. So a handler could not rely on that." Lattice's op-log *does* record the eventual resolution checkpoint. **G4's mis-merge metric is therefore defined as: of conflict objects the structural merger proposed to auto-resolve, the fraction whose proposed bytes differ from the resolution checkpoint that actually landed.** Two constraints follow and must be written into the gate: this is **retrospective telemetry, not a pre-release gate** (at v1 there is no user history), so G4 ships against a fixed corpus and the op-log metric is the continuous post-release monitor; and because the developer sees the proposal before resolving, agreement is inflated by anchoring — the measured rate is a **lower bound**.

### 3.4 Bound the semantic layer with named constants, and stop degrading silently

Adopt difftastic's *shape*, and re-derive its numbers. `LTX_SEMANTIC_BYTE_LIMIT` (start 1,000,000), `LTX_SEMANTIC_NODE_LIMIT` (start 3,000,000), `LTX_PARSE_ERROR_LIMIT` (start 0) are **provisional seeds requiring re-benchmarking** — `DFT_GRAPH_LIMIT` bounds a Dijkstra route search in an interactive difftool, whereas LTX's GumTree-family matcher has a different cost structure and writes to a signed checkpoint. Learn the process lesson regardless: **count work performed, never estimate it in advance**, and apply the limit per changed region as difftastic does.

Invert one binding decision. The spec says the semantic layer "degrades silently to line/byte behaviour." Difftastic is louder: `FileFormat::TextFallback { reason }`. **ADR-SM-03: every semantic-layer fallback is written to the op-log as a `semantic_fallback` operation carrying a reason enum (byte limit / node limit / parse error / no grammar), queryable via `ltx trace --semantic-fallback`.** Silent to the UI, loud to the record.

### 3.5 Never own a grammar; pin it and hash it

**ADR-SM-04: Lattice defines no grammar format and binds to upstream tree-sitter grammars; the derived semantic index is keyed on (grammar name, grammar content hash, matcher parameter set)**, so a grammar upgrade invalidates and regenerates rather than silently changing merge behaviour. **ADR-SM-05: a checkpoint recording a structural resolution carries the grammar content hash and parameter set in its Ed25519-signed provenance, and native sync has an explicit convergence-under-grammar-skew test.**

### 3.6 Budget for per-language configuration; it does not go away

Mergiraf — the most language-agnostic structural merge tool that exists, currently 31 languages plus 14 declarative formats — still requires hand-written per-language commutative-parent sets, signature definitions and isomorphism rules, and publishes **no support tiers**: every language is listed identically. "A tree-sitter grammar exists" does not imply "structural merge works." **Lattice ships a machine-readable `languages.toml` with a coverage tier per language, and G4 is evaluated per tier per language, never as a repo-wide average.** The tier manifest is a genuine addition to the state of the art, not a copy.

### 3.7 Escalate; never pretty-print

**ADR-SM-06:** line merge first; on conflict, reconstruct fictional revisions from the conflicted output and seed the structural pass with the matchings already established (Mergiraf's mechanism); whole-file structural only if conflicts persist. The relevant measured datum is that MergirafSemi's *coarser* strategy — line merge inside method bodies — runs at 44.7 ms median against Mergiraf's 57.3 ms (22% faster) while auto-resolving 80.0% of scenarios against Mergiraf's 84.1%; that is the granularity/cost curve, not a free win. **ADR-SM-07: merged artifacts are always reassembled from byte spans, never printed from the tree**, and the semantic index must tile every character with no gaps — SemanticMerge's invariant, imported.

### Residual risk for Lattice

- **ADR-SM-02 is either safe-and-useless or leans entirely on per-language work.** Clause (a) writes structural bytes only when line merge is *already* clean and agrees — precisely the case where structural merge adds nothing. All the value therefore collapses into clause (b), the commutative-parent carve-out, which is exactly the hand-written per-language configuration Mergiraf shows does not generalise. The rule is honest, but it converts "structural merge" into "a per-language allowlist of commutative parents." Own that, or the ≥20% conflict-reduction target is unreachable *because of* ADR-SM-02.
- **First-class conflict objects relocate the trade-off; they do not remove it.** Something must still decide whether the working tree materialises the proposal. Materialise by default and silent mis-merge risk is identical to any other tool; do not, and conflict reduction cannot be claimed. Pareto-pairing the two G4 metrics names the trade-off but does not resolve it.
- **The op-log oracle is anchored by the proposal it scores.** If LTX shows a proposed resolution and the developer accepts it, "the resolution that landed" equals the proposal. The metric measures agreement with the tool's own suggestion, so it under-reports error. A control arm (a withheld-proposal cohort) is the only way to get an unbiased number.
- **Merge output is a function of a non-authoritative input.** Bytes are the source of truth, but if a structural resolution's *bytes* depend on grammar version, matcher hyperparameters and limit constants, two peers on different grammar versions can produce different merged bytes from identical inputs — and those bytes get checkpointed, content-addressed and signed. Git does not have this problem because diff3 is deterministic. ADR-SM-05 records the skew; it does not make sync converge.
- **`ltx undo` is not an involution across a grammar upgrade.** Undo restores an op-log pointer, but redo recomputes a proposal from the *current* grammar hash and parameters. Universal undo must therefore replay the recorded proposal bytes, not regenerate them.
- **Regenerating the derived index changes past answers.** A grammar upgrade can silently change what `ltx trace` returns about history that has not changed. Auditability and regenerability are in tension; someone must decide which wins.
- **Tier A's ≥95% recall is imported from a number Lattice's own architecture undercuts.** 98.7% on type declarations comes from Eclipse JDT ASTs on Java, where types are large subtrees and one-type-per-file is conventional. On a tree-sitter CST without name resolution, and for languages without that convention, Tier A is unproven — by the analysis's own CST/AST argument.
- **The headline agent query inherits the recall ceiling, but the exposure is smaller than it looks.** Published recall is measured only on entities whose subtree changed; unchanged entities match at 100%. Per changed method the miss rate is ~24.9%, per changed field ~47.8% — but only ~4.8% of methods and ~1.5% of fields changed per commit in the benchmark, so repo-wide exposure is roughly an order of magnitude lower, while compounding across a long history. Entity-scoped `ltx trace` must still be gated to Tier A or report its own recall.
- **Conflict reduction is trivially met and is the same knob as mis-merges.** Published reductions: Apel et al. 34% average, Cavalcanti's earlier replication 62% ± 24%, the OOPSLA'17 study's own measurement ~24%, S3M 51%. A tool passes ≥20% by being aggressive, at exactly the cost of the mis-merge gate. Gate them as a Pareto pair or the target is meaningless.
- **Structural merge may not clear its own bar.** RefMerge resolved or reduced conflicts in 25% of 2,001 scenarios but *increased* conflicting LOC in 11%; IntelliMerge helped 24% and made 30% worse. And S3M's safety cost ~32× unstructured runtime.
- **The seven-noun budget is under pressure.** Conflict objects, attestations, `semantic_fallback` records, coverage tiers and grammar hashes are introduced here. `ltx trace --semantic-fallback` is a user-facing surface. Either these hide behind existing nouns or the budget is already spent.
- **SemanticMerge's cause of death applies.** It was ecosystem and commercial, not technical. Structural merge is not a wedge — it is a feature of a VCS that must first be adopted for other reasons. This supports the spec's sequencing (Git bridge before semantic and agent layers) and argues against leading with merge quality.

## Sources

- [primary] Falleri, Morandat, Blanc, Martinez, Monperrus, *Fine-grained and Accurate Source Code Differencing*, ASE 2014 — https://www.labri.fr/perso/xblanc/data/papers/ASE14.pdf *(full text extracted and verified: RTED 82 OOM / 206 >10s; medians 10/18/30/298/2654; means 20 ms, 74 ms; 122/144 and 28/144; minDice 0.5 vs 0.2; no precision/recall anywhere in the paper)*
- [primary] Falleri & Martinez, *Fine-grained, accurate and scalable source differencing*, ICSE 2024 — https://dl.acm.org/doi/10.1145/3597503.3639148 *(ACM returned 403; 50×–281× speedup and 50% smaller median edit scripts confirmed via the ICSE 2024 programme page and Semantic Scholar)*
- [primary] Martinez, Falleri, Monperrus, *Hyperparameter Optimization for AST Differencing*, TSE 2023 — https://arxiv.org/abs/2011.10268 *(21.8% verified in abstract)*
- [primary] Alikhanifard & Tsantalis, *A Novel Refactoring and Semantic Aware AST Differencing Tool and a Benchmark* — https://arxiv.org/pdf/2403.05939 *(Table 7 and Table 3 extracted verbatim; all tools use Eclipse JDT; unchanged elements excluded from P/R)*
- [primary] Cavalcanti, Borba, Accioly, *Evaluating and Improving Semistructured Merge*, OOPSLA 2017 — https://pauloborba.cin.ufpe.br/publication/2017evaluating_and_improving_semistructured_merge/2017OOPSLASemiVsUnstructuredMerge.pdf *(Tables 1 and 2 extracted; 34,030/50; 3,260 vs 2,714; 0 aFP and 2,489 aFN for the improved tool; handler-level oracle; 24 min vs 45 s)*
- [primary] *MergirafSemi: A Language-Agnostic Semistructured Merge Tool* — https://arxiv.org/html/2608.11345 *(21,615/513/5; per-language aFN table; 44.7 vs 57.3 ms; 80.0% vs 84.1%)*
- [primary] Ellis, Nadi, Tsantalis, *Operation-based Refactoring-aware Merging* — https://arxiv.org/pdf/2112.10370 *(2,001 scenarios; RefMerge 25%/11%; IntelliMerge 24%/30%)*
- [primary] difftastic `src/options.rs` — https://raw.githubusercontent.com/Wilfred/difftastic/master/src/options.rs
- [primary] difftastic `src/main.rs` — `FileFormat::TextFallback { reason }` and per-section limit application — https://raw.githubusercontent.com/Wilfred/difftastic/master/src/main.rs
- [primary] difftastic CHANGELOG — `--node-limit` (v0.19, 50k → 100k in v0.20) replaced by `--graph-limit` in v0.30, with the stated estimation failure — https://raw.githubusercontent.com/Wilfred/difftastic/master/CHANGELOG.md
- [primary] difftastic README — non-goals (no patching, no merging), lossiness, known issues — https://github.com/Wilfred/difftastic
- [primary] difftastic manual — https://difftastic.wilfred.me.uk/diffing.html · https://difftastic.wilfred.me.uk/tricky_cases.html
- [primary] Wilfred Hughes, *Difftastic, the Fantastic Diff* — A\*, O(L×R), O(2^N), parent-exit tuple, "several million vertices" — https://www.wilfred.me.uk/blog/2022/09/06/difftastic-the-fantastic-diff/
- [primary] SemanticMerge external parsers guide — stdin protocol, YAML position tree, no-holes rule — https://www.semanticmerge.com/documentation/external-parsers/external-parsers-guide
- [primary] `sageserpent-open/SemanticMergeScalaPlugin` — independent implementation confirming `READY`, triplets, `OK`/`KO`, `end` — https://github.com/sageserpent-open/SemanticMergeScalaPlugin
- [primary] Mergiraf architecture (line-merge fast path, fictional revisions, GumTree classic, PCS triples, per-language commutative parents/signatures) and languages (31 + 14, no tiers) — https://mergiraf.org/architecture.html · https://mergiraf.org/languages.html
- [secondary] Unity acquires Codice Software, 17 Aug 2020, ~$20M — https://mcvuk.com/development-news/unity-acquires-plastic-scm-developer-codice-software/ · https://app.mergerlinks.com/transactions/2020-08-17-codice-software/dealmakers
- [secondary] "End of the availability of standalone Semantic Merge" (forum topic 23358) — now 302-redirects to Unity Version Control discussions, verified live — https://forum.plasticscm.com/topic/23358-end-of-the-availability-of-standalone-semantic-merge/
