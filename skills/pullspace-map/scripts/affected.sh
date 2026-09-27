#!/bin/sh
# Which map pages a branch's changes fall under.
#
#   affected.sh [base]      (default: the merge base with origin's default branch)
#
# Compares the working tree (committed and uncommitted) against <base>, and
# prints, tab-separated, for every existing page with a changed file under it:
#
#   <dir>   <nearest>   <under>   <state>
#
# <nearest> counts the changed files this is the closest page to: these pages
# are the ones to read and, if their text is no longer true, rewrite. A page
# with nearest 0 is only an ancestor; leave it alone unless its text is wrong.
# <state> is what plan.sh says about it now. "/" is the root.
# Then, one line each, changed directories no page is nearer to than the root:
#
#   nopage  <dir>   <changed files directly in it>
set -eu
here=$(cd "$(dirname "$0")" && pwd)
cd "$(git rev-parse --show-toplevel)"

base=${1:-}
if [ -z "$base" ]; then
  up=$(git symbolic-ref -q --short refs/remotes/origin/HEAD || echo origin/main)
  base=$(git merge-base HEAD "$up")
fi

changed=$(mktemp "${TMPDIR:-/tmp}/pullspace-changed.XXXXXX")
states=$(mktemp "${TMPDIR:-/tmp}/pullspace-states.XXXXXX")
trap 'rm -f "$changed" "$states"' EXIT
{
  git diff --name-only "$base" -- . ':(exclude).pullspace'
  git ls-files --others --exclude-standard -- . ':(exclude).pullspace'
} | sort -u > "$changed"
"$here/plan.sh" | awk -F '\t' '$1 == "page" { print $2 "\t" $3 }' > "$states"

awk -F '\t' -v states="$states" '
  BEGIN { while ((getline line < states) > 0) { split(line, f, "\t"); state[f[1]] = f[2]; } }
  {
    file = $0
    n = split(file, part, "/")
    nearest = "/"
    path = ""
    for (i = 1; i < n; i++) {
      path = (i == 1) ? part[1] : path "/" part[i]
      if (path in state) { under[path]++; nearest = path }
    }
    if ("/" in state) under["/"]++
    near[nearest]++
    if (nearest == "/" && n > 1) { d = file; sub(/\/[^\/]*$/, "", d); if (!(d in state)) orphan[d]++ }
  }
  END {
    for (d in under) printf "%s\t%d\t%d\t%s\n", d, near[d] + 0, under[d], state[d]
    for (d in orphan) printf "nopage\t%s\t%d\n", d, orphan[d]
  }' "$changed" | sort
