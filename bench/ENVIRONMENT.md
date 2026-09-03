# Reference environment (§0.3)

The protocol names a minimum hardware class: ≥ 8 physical x86-64 cores (or
Apple Silicon equivalent), ≥ 16 GB RAM, NVMe-class storage. Timing targets are
p95 over ≥ 100 warm runs and apply **unscaled** at or above this class.

## Machine actually used for all measurements in GAUNTLET.md

| Property | Value |
|---|---|
| Architecture | Apple Silicon (arm64), Darwin 25.4.0 |
| Physical cores | 8 |
| Logical cores | 8 |
| RAM | 24 GiB (25769803776 bytes) |
| Storage | NVMe-class internal SSD |
| Rust | 1.96.0 (ac68faa20 2026-05-25) |
| Python | 3.14.6 |
| git | 2.50.1 (Apple Git-155) |

**Verdict: at or above the reference class.** 8 physical cores meets the floor,
24 GiB exceeds the 16 GiB floor, storage is NVMe-class. Therefore **no
calibration factor is applied** and all timing targets are enforced at their
stated values.

## Calibration procedure (frozen before the G1 harness freeze, §0.3)

If measurement ever moves to weaker or different hardware, the normalization
factor is computed by `harness/lib/calibrate.py`, which runs a fixed
microbenchmark triple — single-core integer/branch mix, memory-bandwidth
streaming, and 4 KiB O_DSYNC write latency — and reports the geometric mean of
the three ratios against the values recorded on this machine. The factor is
**capped at 2×** per §0.3; a machine needing more than 2× is not eligible to
produce gate measurements at all. The procedure and its recorded reference
values are frozen; changing either is an ADR under §0.3.

Regenerate this machine's reference values:

```bash
python3 harness/lib/calibrate.py --record
```
