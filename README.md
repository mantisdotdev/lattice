# Lattice

**Version control for the era in which humans and agents write code together.**

Engine: `LTX` · CLI: `ltx` · repo directory: `.lattice/` · Rust · Apache-2.0

---

## What this is

Git won on speed, distribution, and content addressing. It now fails on four
fronts, and Lattice is a direct attack on all four at once:

| Git's failure | Lattice's answer |
|---|---|
| A punishing conceptual model exposed raw | **Seven nouns, total.** No index, no HEAD, no detached anything. Machine-linted, not aspirational (gate G2.3). |
| "Clean history" only by destroying the real record | **Lenses** — named, shareable projections over immutable history. Read-path only: delete every lens and you lose exactly zero information (gate G2.7). |
| Large files and monorepos are bolt-ons | **Content-defined chunking for everything**, from byte one. Measured against `restic` and `git gc --aggressive`, not against a strawman (gate G1.9). |
| Total blindness to *who or what* wrote a change | **Signed provenance as a first-class, indexed, queryable record** — actor class, model ID, task linkage, review status (gates G5.1, G5.2, G5.4). |

And one thing Git does that Lattice refuses to inherit: **losing your work.**
"No data loss" here is not a slogan; it is four HARD gates with fault-injection
harnesses attached (G1.1–G1.4).

## The seven concepts

There are seven user-facing nouns. There is no eighth.

1. **Working state** — what is on disk right now, continuously auto-snapshotted.
2. **Change** — a logical unit of work with a stable ID that survives amendment.
3. **Checkpoint** — an immutable, durable snapshot. The unit of history.
4. **Line** — a named line of work.
5. **Lens** — a named projection that presents history in a chosen shape.
6. **Workspace** — an independent checkout with its own working state.
7. **Remote** — a peer to sync with, never an authority.

Chunks, entities, Merkle trees, and the operation log exist, and you can inspect
them under `ltx internals`. You will never be required to know they are there.

## Everyday use

```
ltx save "Added wallet connect"   # one step. no staging area, no ceremony.
ltx undo                          # undoes the last operation. any operation.
ltx start auth                    # a new line of work
ltx switch main                   # state is preserved per line; no stash ritual
ltx diff                          # structural where possible, --raw always available
ltx log                           # through the active lens; --forensic shows everything
ltx sync                          # there is no force. divergence converges by merge.
ltx trace src/auth/session.rs     # who and what wrote this, and whether it was reviewed
ltx adopt                         # attach to an existing Git repo and keep your team
```

Every command takes `--json`. Exit codes are a contract. Every error names a way
back to safety — that one is machine-checked (gate G2.4).

## Status

**Under construction, and honest about it.** This project is built under a
protocol that forbids claiming anything a harness has not measured. The live
scorecard is [`GAUNTLET.md`](GAUNTLET.md); it is generated, never written.

```bash
scripts/gauntlet status      # current delivery state
scripts/gauntlet measure G0  # re-measure a stage yourself
```

If a gate in that file says `N/A-yet`, the harness for it does not exist, and so
neither does the claim. Nothing in this README is exempt from that rule: the
table above describes what Lattice is being built to do, and `GAUNTLET.md`
records how much of it currently survives contact with a measurement.

## Repository map

| Path | What lives there |
|---|---|
| `crates/ltx-core` | all logic — storage, op-log, merge, provenance |
| `crates/ltx` | CLI shell (contains no logic the API lacks) |
| `crates/ltx-daemon` | API shell |
| `harness/` | the Gauntlet harnesses — one per gate, frozen before its stage |
| `harness/gates.toml` | the authoritative gate registry: metric, target, type |
| `docs/adr/` | architecture decision records |
| `docs/prior-art/` | the §3 studies that constrain the design |
| `eval/` | frozen persona and adversarial-reviewer prompt templates |
| `corpus/` | hash-pinned corpus manifests and build scripts |
| `bench/` | environment record, calibration, raw results, ratchet baselines |
| `STAKEHOLDER/` | every communication to the party who commissioned this |

## Building

```bash
cargo build --workspace
cargo test  --workspace
```

## License

Apache-2.0. See [LICENSE](LICENSE).
