#!/bin/sh
# Builds pullspace for embedding and writes the bundle to crates/embed/dist, which the
# pullspace-embed crate includes. Run it after changing pullspace and commit dist/ with
# the change, so a host depending on the crate gets this build without a wasm toolchain.
#
# Needs the dioxus CLI (`cargo install dioxus-cli`) and the wasm32-unknown-unknown target.
# The files keep fixed names with a version on their URLs, so a browser can cache them for
# good, and the module is gzipped. `--no-build` repackages the last build.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
out="$root/crates/embed/dist"

cd "$root"
if [ "${1:-}" != "--no-build" ]; then
    dx build --platform web --release --base-path pullspace
fi

public="$root/target/dx/pullspace/release/web/public"
js=$(ls -t "$public"/assets/pullspace-*.js | head -n 1)
wasm=$(ls -t "$public"/assets/pullspace_bg-*.wasm | head -n 1)
if ! grep -q "/pullspace/assets/$(basename "$js")" "$public/index.html"; then
    echo "$public/index.html doesn't load $(basename "$js"); was it built with --base-path pullspace?" >&2
    exit 1
fi
version=$(cat "$js" "$wasm" | shasum | cut -c1-12)

mkdir -p "$out"
gzip -9 -n -c "$wasm" >"$out/pullspace_bg.wasm.gz"
sed "s#/pullspace/assets/$(basename "$wasm")#/pullspace/pullspace_bg.wasm?v=$version#g" \
    "$js" >"$out/pullspace.js"
sed "s#/pullspace/assets/$(basename "$js")#/pullspace/pullspace.js?v=$version#g" \
    "$public/index.html" >"$out/index.html"
grep -q "pullspace_bg.wasm?v=$version" "$out/pullspace.js" || {
    echo "couldn't point pullspace.js at the module" >&2
    exit 1
}
# Which commit this is, marked when the tree had changes on top of it.
commit=$(git rev-parse HEAD)
git diff --quiet HEAD -- . ':!crates/embed/dist' || commit="$commit+dirty"
printf '%s\n' "$commit" >"$out/COMMIT"
ls -l "$out"
