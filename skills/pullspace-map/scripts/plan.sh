#!/bin/sh
# Where the summary map stands: every page and whether it is current, then the
# directories that have no page, biggest first.
#
#   plan.sh [--worktree | --index | --head]
#
# Output, tab-separated:
#   page    <dir>   current|stale|unstamped|orphan   <stamp>   <now>
#   nopage  <dir>   <files under it>   <files directly in it>
#
# "orphan" is a page whose directory no longer exists. "/" is the root.
set -eu
mode="${1:---worktree}"
here=$(cd "$(dirname "$0")" && pwd)
cd "$(git rev-parse --show-toplevel)"

case "$mode" in
  --head) tree=$(git rev-parse 'HEAD^{tree}') ;;
  --index) tree=$(git write-tree) ;;
  --worktree)
    tmp=$(mktemp "${TMPDIR:-/tmp}/pullspace-index.XXXXXX")
    trap 'rm -f "$tmp"' EXIT
    # Start from the real index when there is one, for its stat cache; an
    # empty file is not a valid index, so with none git starts a fresh one.
    cp "$(git rev-parse --git-path index)" "$tmp" 2>/dev/null || rm -f "$tmp"
    GIT_INDEX_FILE="$tmp" git add -A -- . >/dev/null 2>&1
    tree=$(GIT_INDEX_FILE="$tmp" git write-tree)
    ;;
  *) echo "plan.sh: unknown mode $mode" >&2; exit 2 ;;
esac

root=$(git ls-tree "$tree" | awk -F '\t' '$2 != ".pullspace"' | git mktree)

have=$(mktemp "${TMPDIR:-/tmp}/pullspace-have.XXXXXX")
trap 'rm -f "$have" ${tmp:-}' EXIT

if [ -d .pullspace/map ]; then
  find .pullspace/map -name index.html | sort | while IFS= read -r page; do
    dir=${page#.pullspace/map}; dir=${dir%index.html}; dir=${dir#/}; dir=${dir%/}
    printf '%s\n' "$dir" >> "$have"
    stamp=$(sed -n 's/.*<meta name="pullspace:tree" content="\([0-9a-fA-F]\{40\}\)".*/\1/p' "$page" | head -n 1 | tr 'A-F' 'a-f')
    if [ -z "$dir" ]; then
      now=$root; shown=/
    else
      now=$(git rev-parse --verify --quiet "$tree:$dir" || :); shown=$dir
    fi
    if [ -z "$now" ]; then state=orphan
    elif [ -z "$stamp" ]; then state=unstamped
    elif [ "$stamp" = "$now" ]; then state=current
    else state=stale
    fi
    printf 'page\t%s\t%s\t%s\t%s\n' "$shown" "$state" "${stamp:--}" "${now:--}"
  done
fi

# Directories without a page, with how many files are under each and directly
# in each. The agent decides which deserve one; seed.md says what to skip.
git ls-tree -r --name-only "$tree" | awk -v have="$have" '
  BEGIN { while ((getline d < have) > 0) seen[d] = 1 }
  /^\.pullspace\// { next }
  {
    n = split($0, part, "/")
    path = ""
    for (i = 1; i < n; i++) {
      path = (i == 1) ? part[1] : path "/" part[i]
      total[path]++
      if (i == n - 1) direct[path]++
    }
  }
  END {
    for (d in total) if (!(d in seen)) printf "nopage\t%s\t%d\t%d\n", d, total[d], direct[d] + 0
  }' | sort -t "$(printf '\t')" -k3,3nr -k2,2
if ! grep -qx '' "$have" 2>/dev/null; then
  printf 'nopage\t/\t%s\t-\n' "$(git ls-tree -r --name-only "$tree" | grep -vc '^\.pullspace/')"
fi
