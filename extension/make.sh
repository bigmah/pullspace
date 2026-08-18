#!/usr/bin/env bash
#
# The zip the Chrome Web Store wants: this directory, minus the parts of it
# that are about developing it rather than running it.
#
#   ./extension/make.sh          -> extension/pullspace-<version>.zip
#
# The store takes a zip of the *contents* of the extension, not of a folder
# holding them — a manifest.json one level down is the single most common
# rejection, so this zips from inside.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"

version="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' manifest.json | head -1)"
out="pullspace-${version}.zip"

# Left out deliberately: package.json and test.js are how node reads the
# modules beside them (see test.js), the .svg is what the .pngs were drawn
# from, and this script is this script. None of them is code the extension
# runs, and everything shipped is something a reviewer has to read.
rm -f "$out"
zip -r -X "$out" . \
	-x 'package.json' \
	-x 'test.js' \
	-x 'make.sh' \
	-x 'README.md' \
	-x 'icons/icon.svg' \
	-x '*.zip' \
	-x '.*' \
	-x '__MACOSX/*' \
	-x '*/.DS_Store' \
	-x '.DS_Store' >/dev/null

echo "$out"
unzip -l "$out" | sed -n '4,$p' | head -20
