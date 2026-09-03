#!/usr/bin/env bash
# Clone the mining corpus (G0.3 / G0.4). Bare + full history: merge replay needs
# every object, but never needs a working tree.
set -uo pipefail

MANIFEST="${1:-corpus/manifests/mining-repos.tsv}"
DEST="${2:-corpus/data/repos}"
JOBS="${JOBS:-5}"

mkdir -p "$DEST" corpus/data/logs

clone_one() {
  local slug="$1"
  local name="${slug//\//__}"
  local dir="$DEST/$name.git"
  if [ -d "$dir" ] && git -C "$dir" rev-parse --git-dir >/dev/null 2>&1; then
    echo "skip   $slug (present)"; return 0
  fi
  rm -rf "$dir"
  if git clone --bare --quiet "https://github.com/$slug.git" "$dir" \
        >"corpus/data/logs/$name.log" 2>&1; then
    echo "ok     $slug  $(du -sh "$dir" | cut -f1)"
  else
    echo "FAIL   $slug (see corpus/data/logs/$name.log)"
  fi
}
export -f clone_one
export DEST

grep -v '^#' "$MANIFEST" | cut -f1 | grep -v '^$' \
  | xargs -P "$JOBS" -I{} bash -c 'clone_one "$@"' _ {}
