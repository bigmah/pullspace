#!/bin/sh
# Print the pullspace:tree stamp for a directory.
#
#   stamp.sh <dir> [--worktree | --index | --head] [--write]
#
# --write also puts the stamp, and today's date, into the directory's page
# (.pullspace/map/<dir>/index.html), replacing the pullspace:tree and
# pullspace:written tags already there.
#
# <dir> is a path from the repository root; "" or "." is the root.
#
#   --worktree  (default) the directory as it is on disk right now, tracked and
#               untracked-but-not-ignored files alike. Uses a throwaway index,
#               so your staging area is not touched.
#   --index     as staged.
#   --head      as of the last commit.
#
# The root is stamped with .pullspace left out; every other directory is
# `git rev-parse <tree>:<dir>`. See FORMAT.md.
set -eu

dir="${1:-}"
shift || :
mode=--worktree
write=
for arg in "$@"; do
  case "$arg" in
    --write) write=1 ;;
    *) mode=$arg ;;
  esac
done
dir="${dir#./}"; dir="${dir#/}"; dir="${dir%/}"
[ "$dir" = "." ] && dir=""

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
  *) echo "stamp.sh: unknown mode $mode" >&2; exit 2 ;;
esac

if [ -z "$dir" ]; then
  stamp=$(git ls-tree "$tree" | awk -F '\t' '$2 != ".pullspace"' | git mktree)
  page=.pullspace/map/index.html
else
  stamp=$(git rev-parse --verify --quiet "$tree:$dir") || {
    echo "stamp.sh: no directory '$dir' in that tree" >&2; exit 1; }
  page=.pullspace/map/$dir/index.html
fi
echo "$stamp"

if [ -n "$write" ]; then
  [ -f "$page" ] || { echo "stamp.sh: no page at $page" >&2; exit 1; }
  grep -q '<meta name="pullspace:tree" content="[^"]*">' "$page" || {
    echo "stamp.sh: $page has no pullspace:tree tag to replace — add one as FORMAT.md shows" >&2; exit 1; }
  today=$(date +%Y-%m-%d)
  sed -e "s/<meta name=\"pullspace:tree\" content=\"[^\"]*\">/<meta name=\"pullspace:tree\" content=\"$stamp\">/" \
      -e "s/<meta name=\"pullspace:written\" content=\"[^\"]*\">/<meta name=\"pullspace:written\" content=\"$today\">/" \
      "$page" > "$page.tmp" && mv "$page.tmp" "$page"
  echo "stamp.sh: wrote it into $page" >&2
fi
