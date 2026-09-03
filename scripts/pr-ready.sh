#!/usr/bin/env bash
# Is a pull request ready to merge?
#
# House rule: a PR merges only when CodeRabbit has FINISHED its review pass on
# THIS head commit and its outstanding findings are at zero.
#
# Every uncertainty here fails closed. The first version of this script did not,
# and carried the same defect that let PR #1 merge with findings open: when the
# newest CodeRabbit summary did not name the current head it printed a note and
# carried on, so a stale review could still produce READY. A merge gate that
# reports ready when it does not know is not a gate.
#
# Exit 0 = ready. Exit 1 = not ready. Exit 2 = could not determine.
#
#   scripts/pr-ready.sh 2
set -uo pipefail

PR="${1:?usage: pr-ready.sh <pr-number>}"
REPO="${REPO:-mantisdotdev/lattice}"

fail() { echo "NOT READY — $*"; exit 1; }
undetermined() { echo "UNDETERMINED — $*"; exit 2; }

command -v gh >/dev/null || undetermined "gh CLI unavailable"
command -v jq >/dev/null || undetermined "jq unavailable"

state=$(gh pr view "$PR" --repo "$REPO" --json state,mergeable,headRefOid \
        --jq '"\(.state)|\(.mergeable)|\(.headRefOid)"' 2>/dev/null) \
  || undetermined "cannot read PR #$PR"
IFS='|' read -r pr_state mergeable head <<<"$state"

[ "$pr_state" = "OPEN" ] || fail "PR is $pr_state"
[ "$mergeable" = "MERGEABLE" ] || fail "PR is not mergeable ($mergeable)"

# ---------------------------------------------------------------- 1. CI green
# `bucket` collapses the many check states into pass/fail/pending/skipping/
# cancel, so no state is silently uncovered. A failure to READ the checks is
# undetermined, never "no checks".
checks=$(gh pr checks "$PR" --repo "$REPO" --json name,state,bucket 2>/dev/null) \
  || undetermined "cannot read CI checks"
echo "$checks" | jq -e 'type == "array"' >/dev/null 2>&1 \
  || undetermined "unparseable CI check output"

bad=$(echo "$checks" | jq -r '[.[] | select(.bucket=="fail" or .bucket=="cancel")] | length') \
  || undetermined "cannot evaluate CI buckets"
waiting=$(echo "$checks" | jq -r '[.[] | select(.bucket=="pending")] | length') \
  || undetermined "cannot evaluate CI buckets"
[ "$bad" -eq 0 ] || fail "$bad CI check(s) failed or cancelled"
[ "$waiting" -eq 0 ] || fail "$waiting CI check(s) still running"

# ------------------------------------------------- 2. review is for THIS head
summary=$(gh api "/repos/$REPO/issues/$PR/comments" --paginate \
          --jq '[.[] | select(.user.login | test("coderabbit";"i"))] | last' 2>/dev/null) \
  || undetermined "cannot read PR comments"
[ -n "$summary" ] && [ "$summary" != "null" ] \
  || fail "CodeRabbit has not commented yet"

body=$(echo "$summary" | jq -r '.body')
if echo "$body" | grep -qiE "review in progress|currently processing"; then
  fail "CodeRabbit review still in progress"
fi
# CodeRabbit names the commit range it reviewed. If the current head is not in
# that summary, the review predates the code about to be merged -- which is the
# state that must NOT read as ready.
if ! echo "$body" | grep -qE "${head:0:7}|${head}"; then
  fail "newest CodeRabbit review does not cover head ${head:0:7}; it reviewed an earlier commit"
fi

# ------------------------------------ 3. zero unresolved findings, ALL of them
# reviewThreads(first:100) silently truncates, so an unresolved thread past the
# first hundred would be invisible to a gate whose entire job is finding them.
cursor="null"
open_findings=0
for _ in $(seq 1 20); do
  page=$(gh api graphql -f query='
    query($owner:String!, $name:String!, $pr:Int!, $after:String) {
      repository(owner:$owner, name:$name) {
        pullRequest(number:$pr) {
          reviewThreads(first:100, after:$after) {
            pageInfo { hasNextPage endCursor }
            nodes { isResolved isOutdated comments(first:1){nodes{author{login}}} }
          }
        }
      }
    }' -f owner="${REPO%%/*}" -f name="${REPO##*/}" -F pr="$PR" \
       -F after="$cursor" 2>/dev/null) || undetermined "cannot query review threads"

  n=$(echo "$page" | jq -r '
        [.data.repository.pullRequest.reviewThreads.nodes[]
         | select(.isResolved == false and .isOutdated == false)
         | select(.comments.nodes[0].author.login | test("coderabbit";"i"))]
        | length') || undetermined "cannot evaluate review threads"
  open_findings=$((open_findings + n))

  has_next=$(echo "$page" | jq -r '.data.repository.pullRequest.reviewThreads.pageInfo.hasNextPage')
  [ "$has_next" = "true" ] || break
  cursor=$(echo "$page" | jq -r '.data.repository.pullRequest.reviewThreads.pageInfo.endCursor')
done

[ "$open_findings" -eq 0 ] || fail "$open_findings unresolved CodeRabbit finding(s)"

echo "READY — CI green, CodeRabbit reviewed ${head:0:7}, 0 unresolved findings"
exit 0
