# GAUNTLET.md — the auditable log

This file is the delivery. Per §0.6, *"a delivery not accompanied by the
complete `GAUNTLET.md` history and final scorecard is not a delivery."*

## How to read this file

Every scorecard below was **generated**, never written. `scripts/gauntlet
measure` executes each gate's harness, reads the number the harness printed,
compares it to the target frozen in `harness/gates.toml`, and appends the
result. There is no code path in this repository by which a gate can be
declared PASS without harness output. If a gate shows `N/A-yet`, its harness
does not exist yet and the gate therefore does not exist yet (§0.1).

Reproduce any scorecard:

```bash
scripts/gauntlet measure G0     # or G1, or a single gate id
scripts/gauntlet scorecard
```

## Status vocabulary

| Status | Meaning |
|---|---|
| `PASS` | harness ran, measured value satisfies the target |
| `FAIL` | harness ran, target not met |
| `FAIL(stale)` | harness file changed since its freeze (§0.3) — must be re-measured |
| `FAIL(harness-error)` | harness could not produce a measurement |
| `WAIVED` | SOFT gate, failing, carrying a §0.5 waiver that passed mechanical validation |
| `PASS-PENDING-HUMAN` | autonomous floor passed and the [S] kit is committed; awaiting real humans (§0.7) |
| `N/A-yet` | no harness yet — the gate is not measurable and is not claimed |

`PASS`, `WAIVED`, and `PASS-PENDING-HUMAN` are the three statuses that let a
stage reach CLEAR.

## Reference environment

Recorded in [`bench/ENVIRONMENT.md`](bench/ENVIRONMENT.md). Timing gates apply
unscaled at or above the §0.3 hardware class.

---

## Iteration log

