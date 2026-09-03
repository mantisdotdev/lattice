#!/usr/bin/env bash
# G0.7 probe repos. The probe reads commit metadata only, so treeless clones
# (--filter=tree:0) are sufficient and are a fraction of the download.
set -uo pipefail
DEST=corpus/data/probe-repos
mkdir -p "$DEST" corpus/data/logs
one() {
  local slug="$1" name="${1//\//__}"
  local dir="$DEST/$name.git"
  git -C "$dir" rev-parse --git-dir >/dev/null 2>&1 && { echo "skip $slug"; return 0; }
  rm -rf "$dir"
  if git clone --bare --filter=tree:0 --quiet "https://github.com/$slug.git" "$dir" \
       >"corpus/data/logs/probe-$name.log" 2>&1; then
    echo "ok   $slug $(du -sh "$dir" | cut -f1)"
  else
    echo "FAIL $slug"
  fi
}
export -f one; export DEST
printf '%s\n' home-assistant/core microsoft/vscode nodejs/node kubernetes/kubernetes \
  | xargs -P 4 -I{} bash -c 'one "$@"' _ {}
