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

# Enumerate the buckets we understand and REJECT anything else. Selecting only
# known-bad values silently excluded an unrecognised or missing bucket from both
# counters, so a check in an unknown state read as passing.
unknown=$(echo "$checks" | jq -r '
  [.[] | select((.bucket // "missing") as $b
                | ["pass","fail","pending","skipping","cancel"] | index($b) | not)]
  | length') || undetermined "cannot evaluate CI buckets"
[ "$unknown" -eq 0 ] || undetermined "$unknown CI check(s) report an unrecognised bucket"

bad=$(echo "$checks" | jq -r '[.[] | select(.bucket=="fail" or .bucket=="cancel")] | length') \
  || undetermined "cannot evaluate CI buckets"
waiting=$(echo "$checks" | jq -r '[.[] | select(.bucket=="pending")] | length') \
  || undetermined "cannot evaluate CI buckets"
[ "$bad" -eq 0 ] || fail "$bad CI check(s) failed or cancelled"
[ "$waiting" -eq 0 ] || fail "$waiting CI check(s) still running"

# ------------------------------------------------- 2. review is for THIS head
# Each field is extracted by its own --jq. Round-tripping the whole comment
# object through a shell variable and re-parsing it broke on control characters
# in CodeRabbit's body (it embeds ASCII art), leaving `body` silently EMPTY --
# so every content check below matched nothing and the gate blocked a PR whose
# review had actually finished.
# EXACT bot login. A substring match on "coderabbit" would let any account
# whose name merely contains it -- coderabbit-helper, my-coderabbit -- post a
# comment that satisfies this gate. The reviewer's identity is the thing being
# trusted, so it is compared exactly.
# `gh api --jq` does not accept --arg, so the login is inlined literally.
body=$(gh api "/repos/$REPO/issues/$PR/comments" --paginate \
       --jq '[.[] | select(.user.login == "coderabbitai[bot]")] | last | .body' \
       2>/dev/null) || undetermined "cannot read PR comments"
comment_iso=$(gh api "/repos/$REPO/issues/$PR/comments" --paginate \
       --jq '[.[] | select(.user.login == "coderabbitai[bot]")] | last | .updated_at' \
       2>/dev/null) || undetermined "cannot read PR comment timestamps"
[ -n "$body" ] && [ "$body" != "null" ] \
  || fail "CodeRabbit has not commented yet"
if echo "$body" | grep -qiE "review in progress|currently processing"; then
  fail "CodeRabbit review still in progress"
fi
# Has CodeRabbit reviewed THIS head? Two acceptable proofs, because CodeRabbit
# emits two shapes of completion:
#   a) a walkthrough summary naming the commit range it reviewed, or
#   b) a bare "Review finished" acknowledgment, which carries NO sha.
# Requiring (a) alone produced a false negative that blocked a PR whose review
# had in fact completed -- so (b) is accepted when the comment is NEWER than the
# head commit, which is the same guarantee by a different route.
# When did GITHUB first see this head? The committer date is set by the author
# (GIT_COMMITTER_DATE), so a backdated commit would make a stale review look
# newer than the code it did not review -- defeating the SHA-free path entirely.
# Check-run start times are stamped server-side and cannot be back-dated by a
# pusher, so the earliest one for this SHA is used instead.
head_epoch=$(gh api "/repos/$REPO/commits/$head/check-runs" \
             --jq '[.check_runs[].started_at] | min // empty' 2>/dev/null \
             | head -1)
if [ -n "$head_epoch" ]; then
  head_epoch=$(TZ=UTC date -j -f "%Y-%m-%dT%H:%M:%SZ" "$head_epoch" "+%s" 2>/dev/null \
               || date -u -d "$head_epoch" "+%s" 2>/dev/null || echo 0)
else
  head_epoch=0
fi
# TZ=UTC is load-bearing: macOS `date -j -f` IGNORES the trailing Z and parses
# the timestamp in local time, so on a machine at UTC+5:30 a comment posted five
# minutes ago read as five hours old -- older than the commit, and the gate
# blocked a PR whose review had finished.
comment_epoch=$(TZ=UTC date -j -f "%Y-%m-%dT%H:%M:%SZ" "$comment_iso" "+%s" 2>/dev/null \
                || date -u -d "$comment_iso" "+%s" 2>/dev/null || echo 0)

# CodeRabbit signals completion in THREE shapes, and a gate that knows only one
# of them blocks PRs whose review has finished. Each was discovered the same
# way: by the gate refusing a PR that was demonstrably ready.
#   a) a walkthrough summary naming the reviewed commit range
#   b) a bare "Review finished" acknowledgment, carrying no SHA
#   c) "Already reviewed the last commit" -- the incremental reviewer declining
#      to repeat itself, which is a POSITIVE statement that head is covered
# (b) and (c) carry no SHA, so they are accepted only when the comment is newer
# than the head commit -- the same guarantee by a different route.
covers_head=false
echo "$body" | grep -qE "${head:0:7}|${head}" && covers_head=true
if [ "$covers_head" != "true" ] \
   && [ "$comment_epoch" -gt "$head_epoch" ] && [ "$head_epoch" -gt 0 ]; then
  if echo "$body" | grep -qiE "review finished|actionable comments posted|already reviewed the last commit"; then
    covers_head=true
  fi
fi
# The SHA-free path depends entirely on a trustworthy head timestamp. Without
# one there is no evidence the review covers this code, so refuse rather than
# guess.
if [ "$covers_head" = "true" ] && [ "$head_epoch" -eq 0 ] \
   && ! echo "$body" | grep -qE "${head:0:7}|${head}"; then
  undetermined "no server-side timestamp for head ${head:0:7} (no check-runs), "\
               "so a review that does not name the SHA cannot be shown to cover it"
fi
[ "$covers_head" = "true" ] \
  || fail "newest CodeRabbit review does not cover head ${head:0:7}; it reviewed an earlier commit"

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
            nodes { isResolved isOutdated comments(first:1){nodes{author{__typename login}}} }
          }
        }
      }
    }' -f owner="${REPO%%/*}" -f name="${REPO##*/}" -F pr="$PR" \
       -F after="$cursor" 2>/dev/null) || undetermined "cannot query review threads"

  # Identify CodeRabbit by its GitHub App bot identity, NOT a login substring
  # (CWE-287). Two subtleties, both learned the hard way:
  #   * GraphQL returns a bot's login WITHOUT the `[bot]` suffix REST uses, so
  #     this must match "coderabbitai", not "coderabbitai[bot]" — the earlier
  #     mismatch made this whole count silently zero, a false READY.
  #   * Requiring __typename == "Bot" means a User account named "coderabbitai"
  #     cannot impersonate the app: only the real GitHub App is a Bot with that
  #     login slug.
  n=$(echo "$page" | jq -r '
        [.data.repository.pullRequest.reviewThreads.nodes[]
         | select(.isResolved == false and .isOutdated == false)
         | select(.comments.nodes[0].author.__typename == "Bot")
         | select(.comments.nodes[0].author.login == "coderabbitai")]
        | length') || undetermined "cannot evaluate review threads"
  open_findings=$((open_findings + n))

  has_next=$(echo "$page" | jq -r '.data.repository.pullRequest.reviewThreads.pageInfo.hasNextPage')
  [ "$has_next" = "true" ] || { has_next="false"; break; }
  cursor=$(echo "$page" | jq -r '.data.repository.pullRequest.reviewThreads.pageInfo.endCursor')
done
# Exhausting the page budget with more pages outstanding means findings were
# never looked at. Silently exiting the loop would report zero from a partial
# count -- the same class of error as truncating at first:100.
[ "${has_next:-false}" != "true" ] \
  || undetermined "more than 2,000 review threads; pagination budget exhausted "\
                  "before all findings were counted"

[ "$open_findings" -eq 0 ] || fail "$open_findings unresolved CodeRabbit finding(s)"

echo "READY — CI green, CodeRabbit reviewed ${head:0:7}, 0 unresolved findings"
exit 0
