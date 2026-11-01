#!/usr/bin/env bash
# swap-binary.sh <src> <dst>: replace an executable through a fresh inode and re-sign it, since
# macOS SIGKILLs a binary overwritten in place at every launch. `just install` and qa-install use it.
set -euo pipefail

src="$1"
dst="$2"
[ -f "$src" ] || { echo "swap-binary: missing source $src" >&2; exit 1; }

rm -f "$dst"
cp "$src" "$dst.staging"
mv "$dst.staging" "$dst"
[ "$(uname)" = "Darwin" ] && codesign --force --sign - "$dst"
exit 0
