#!/usr/bin/env bash
#
# Build the G1.2 adversarial corpus on a CASE-SENSITIVE filesystem and measure
# the gate there.
#
# WHY THIS EXISTS
#
# ADR-14 concluded that "G1.2 can only reach PASS on a case-sensitive
# filesystem", because the corpus case the gate mandates — two names differing
# only by case — cannot exist as two files anywhere else. The reference machine
# (bench/ENVIRONMENT.md) runs case-insensitive APFS, so measured there the gate
# correctly reports 0 mismatches AND a coverage failure: it never tested what
# it was asked to test.
#
# macOS can create a case-sensitive APFS volume inside a sparse disk image,
# which closes the gap on the reference machine rather than deferring the gate
# to a different one.
#
# BOTH HALVES MUST BE CASE-SENSITIVE, and this is the part that is easy to get
# wrong: the corpus, AND the temporary directory the harness copies it into and
# checks out into. `harness/g1/g1_2_byte_fidelity.py:146` uses
# `tempfile.mkdtemp`, which honours TMPDIR. A case-sensitive corpus copied into
# a folding temporary directory folds on the way in, and the gate would report
# a phantom mismatch that looks exactly like an engine bug.
#
# The harness itself is frozen (§0.3) and is not touched. The corpus manifest
# is a generated artifact, not a frozen one, so regenerating it is legitimate —
# and regenerating it here is what the gate's own note asks for.
#
# USAGE
#   scripts/corpus/measure_g1_2_case_sensitive.sh          # build + measure
#   LTX_CASE_KEEP=1 scripts/corpus/measure_g1_2_case_sensitive.sh   # keep the volume
#
# The volume is detached on exit unless LTX_CASE_KEEP is set. The sparse image
# is left in place so a re-run does not rebuild it from scratch.

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
IMAGE="${LTX_CASE_IMAGE:-${TMPDIR:-/tmp}/ltx-case-sensitive.sparseimage}"
VOLNAME="ltxcase"
MOUNT="/Volumes/${VOLNAME}"
# 1.1 GB corpus, copied once into the repo and once into the checkout, plus
# pack storage for the save. 16 GB is comfortable; a sparse image only occupies
# what it actually uses.
SIZE="${LTX_CASE_SIZE:-16g}"
CORPUS_LINK="${REPO}/corpus/data/adversarial"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This script is macOS-specific (hdiutil). On Linux the root filesystem" >&2
  echo "is already case-sensitive: just run the corpus builder and the gate." >&2
  exit 2
fi

cleanup() {
  if [[ -L "${CORPUS_LINK}" ]]; then rm -f "${CORPUS_LINK}"; fi
  if [[ -z "${LTX_CASE_KEEP:-}" ]] && mount | grep -q " ${MOUNT} "; then
    hdiutil detach "${MOUNT}" -quiet || true
  fi
}
trap cleanup EXIT

if [[ ! -f "${IMAGE}" ]]; then
  echo "==> creating case-sensitive sparse image at ${IMAGE}"
  hdiutil create -size "${SIZE}" -fs "Case-sensitive APFS" \
    -volname "${VOLNAME}" -type SPARSE -quiet "${IMAGE%.sparseimage}"
fi

if ! mount | grep -q " ${MOUNT} "; then
  echo "==> mounting ${MOUNT}"
  hdiutil attach "${IMAGE}" -mountpoint "${MOUNT}" -quiet -nobrowse
fi

# Prove the volume actually folds nothing before trusting a measurement taken
# on it. A silent failure here would produce a confident, wrong PASS.
probe="${MOUNT}/.case-probe"
rm -rf "${probe}"; mkdir -p "${probe}"
printf 'upper\n' > "${probe}/A.txt"
printf 'lower\n' > "${probe}/a.txt"
if [[ "$(ls "${probe}" | wc -l | tr -d ' ')" != "2" ]]; then
  echo "FATAL: ${MOUNT} folded A.txt and a.txt — it is not case-sensitive." >&2
  exit 1
fi
rm -rf "${probe}"
echo "==> verified: ${MOUNT} distinguishes A.txt from a.txt"

# The corpus builder writes to a fixed path under the repo, so point that path
# at the case-sensitive volume rather than teaching it a new flag.
mkdir -p "${MOUNT}/adversarial" "${MOUNT}/tmp"
rm -rf "${CORPUS_LINK}"
mkdir -p "$(dirname "${CORPUS_LINK}")"
ln -s "${MOUNT}/adversarial" "${CORPUS_LINK}"

echo "==> building the adversarial corpus on the case-sensitive volume"
TMPDIR="${MOUNT}/tmp" python3 "${REPO}/scripts/corpus/build_adversarial_corpus.py" --force

echo "==> corpus case-sensitivity as recorded in the manifest:"
python3 - "${REPO}" <<'PY'
import json, sys, pathlib
m = json.load(open(pathlib.Path(sys.argv[1]) / "corpus/manifests/g1-2-adversarial.json"))
print("    filesystem_case_sensitive:", m.get("filesystem_case_sensitive"))
print("    folded_names:", m.get("folded_names"))
print("    total_files:", m.get("total_files"))
PY

echo "==> measuring G1.2 with TMPDIR on the case-sensitive volume"
TMPDIR="${MOUNT}/tmp" python3 "${REPO}/harness/g1/g1_2_byte_fidelity.py"
