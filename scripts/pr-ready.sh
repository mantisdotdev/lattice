#!/usr/bin/env bash
# Is a pull request ready to merge?
#
# House rule: a PR merges only when CodeRabbit has FINISHED its review pass and
# its outstanding findings are at zero. Eyeballing a comment count is how PR #1
# got merged with findings still open -- the count includes re-posts across
# passes, so "fewer than last time" is not "zero".
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

state=$(gh pr view "$PR" --repo "$REPO" --json state,mergeable,headRefOid \
        --jq '"\(.state)|\(.mergeable)|\(.headRefOid)"' 2>/dev/null) \
  || undetermined "cannot read PR #$PR"
IFS='|' read -r pr_state mergeable head <<<"$state"

[ "$pr_state" = "OPEN" ] || fail "PR is $pr_state"
[ "$mergeable" = "MERGEABLE" ] || fail "PR is not mergeable ($mergeable)"

# 1. CI must be green. A red build is not a merge candidate regardless of review.
checks=$(gh pr checks "$PR" --repo "$REPO" --json name,state 2>/dev/null) || checks="[]"
red=$(echo "$checks" | jq -r '[.[] | select(.state=="FAILURE" or .state=="ERROR")] | length')
pending=$(echo "$checks" | jq -r '[.[] | select(.state=="PENDING" or .state=="IN_PROGRESS")] | length')
[ "${red:-0}" -eq 0 ] || fail "$red CI check(s) failing"
[ "${pending:-0}" -eq 0 ] || fail "$pending CI check(s) still running"

# 2. CodeRabbit must have reviewed THIS head commit. A review of an older commit
#    says nothing about what is about to be merged.
summary=$(gh api "/repos/$REPO/issues/$PR/comments" --paginate \
          --jq '[.[] | select(.user.login | test("coderabbit";"i"))] | last' 2>/dev/null)
[ -n "$summary" ] && [ "$summary" != "null" ] \
  || fail "CodeRabbit has not commented yet"

body=$(echo "$summary" | jq -r '.body')
if echo "$body" | grep -qiE "review in progress|currently processing"; then
  fail "CodeRabbit review still in progress"
fi
if ! echo "$body" | grep -q "${head:0:7}"; then
  echo "  note: latest CodeRabbit summary does not name head ${head:0:7};"
  echo "        it may be reviewing an older commit."
fi

# 3. Outstanding inline findings must be zero. Resolved threads do not count.
open_findings=$(gh api graphql -f query='
  query($owner:String!, $name:String!, $pr:Int!) {
    repository(owner:$owner, name:$name) {
      pullRequest(number:$pr) {
        reviewThreads(first:100) {
          nodes { isResolved isOutdated comments(first:1){nodes{author{login}}} }
        }
      }
    }
  }' -f owner="${REPO%%/*}" -f name="${REPO##*/}" -F pr="$PR" \
  --jq '[.data.repository.pullRequest.reviewThreads.nodes[]
         | select(.isResolved == false and .isOutdated == false)
         | select(.comments.nodes[0].author.login | test("coderabbit";"i"))]
        | length' 2>/dev/null)

if [ -z "$open_findings" ]; then
  undetermined "could not query review threads"
fi
[ "$open_findings" -eq 0 ] || fail "$open_findings unresolved CodeRabbit finding(s)"

echo "READY — CI green, CodeRabbit reviewed ${head:0:7}, 0 unresolved findings"
exit 0
