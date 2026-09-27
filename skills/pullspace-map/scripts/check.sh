#!/bin/sh
# Check .pullspace/ pages against FORMAT.md. Prints one line per problem and
# exits non-zero if there were any.
#
#   check.sh [page.html ...]      (default: every page under .pullspace/)
#
# Checks: a <title> and a p.lede; a stamp on every map page; nothing the
# sandbox refuses (scripts, handlers, external resources); that every link is
# /-rooted, names a path that exists in the working tree, and that a #L line
# anchor falls inside the file. Only the attributes of real tags are read, so
# HTML quoted inside <pre>/<code> as an example (escaped as &lt;…&gt;) is fine.
set -eu
cd "$(git rev-parse --show-toplevel)"

if [ "$#" -eq 0 ]; then
  set -- $(find .pullspace/map .pullspace/changes -name '*.html' 2>/dev/null | sort)
fi

bad=0
say() { printf '%s: %s\n' "$1" "$2"; bad=1; }

for page in "$@"; do
  page=${page#./}
  [ -f "$page" ] || { say "$page" "no such file"; continue; }
  case "$page" in
    .pullspace/map/*/index.html|.pullspace/map/index.html)
      grep -Eq '<meta name="pullspace:tree" content="[0-9a-fA-F]{40}">' "$page" \
        || say "$page" 'missing <meta name="pullspace:tree" content="…"> — run scripts/stamp.sh' ;;
    .pullspace/changes/*.html) ;;
    *) say "$page" "not where pages live: .pullspace/map/<dir>/index.html or .pullspace/changes/<branch>.html" ;;
  esac
  grep -qi '<title>' "$page" || say "$page" "no <title>"
  grep -q 'class="lede"' "$page" || say "$page" 'no <p class="lede"> under the h1'

  # Every tag, one per line, so what is checked is markup and not prose.
  tags=$(tr '\n' ' ' < "$page" | grep -o '<[a-zA-Z][^>]*>' || :)
  printf '%s\n' "$tags" | grep -qi '^<script' && say "$page" "has a <script> — the sandbox will not run it"
  printf '%s\n' "$tags" | grep -Eqi '[[:space:]]on[a-z]+[[:space:]]*=' && say "$page" "has an on…= handler — the sandbox will not run it"
  printf '%s\n' "$tags" | grep -Eqi '^<(link|iframe|object|embed|base)[[:space:]>]' && say "$page" "has a link/iframe/object/embed/base tag — the sandbox refuses it"
  printf '%s\n' "$tags" | grep -Eqi '(src|srcset|poster)[[:space:]]*=[[:space:]]*["'"'"']?(https?:)?//' && say "$page" "loads something from the network — the sandbox blocks it"
  grep -Eqi '(@import|url\([[:space:]]*["'"'"']?(https?:)?//)' "$page" && say "$page" "CSS pulls from the network — the sandbox blocks it"

  for href in $(printf '%s\n' "$tags" | grep -i '^<a[[:space:]]' | grep -o 'href="[^"]*"' | sed 's/^href="//; s/"$//'); do
    case "$href" in
      http://*|https://*|'#'*|mailto:*) ;;
      /*)
        path=${href%%#*}; path=${path%%\?*}; path=${path#/}; path=${path%/}
        path=$(printf '%s' "$path" | sed 's/%20/ /g')
        if [ -n "$path" ] && [ ! -e "$path" ]; then say "$page" "link to $href — no such path"; continue; fi
        case "$path" in .pullspace/map/*) say "$page" "link to $href — link the directory, not its page" ;; esac
        case "$href" in
          *'#L'*)
            frag=${href#*#}
            if ! printf '%s' "$frag" | grep -Eq '^L[0-9]+(-L?[0-9]+)?$'; then
              say "$page" "link to $href — a line anchor is #L10 or #L10-L20"
            elif [ -f "$path" ]; then
              last=$(printf '%s' "$frag" | sed 's/^L//; s/.*-L\{0,1\}//')
              lines=$(awk 'END { print NR }' "$path")
              [ "$last" -le "$lines" ] || say "$page" "link to $href — $path has only $lines lines"
            fi ;;
        esac ;;
      *) say "$page" "link to $href — write links from the repository root, starting with /" ;;
    esac
  done
done

[ "$bad" -eq 0 ] && echo "ok: $# page(s)"
exit "$bad"
