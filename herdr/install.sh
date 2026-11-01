#!/usr/bin/env bash
# herdr's `[[build]]` step: put this platform's release binary in bin/; `plugin link` skips it.
set -euo pipefail

NAME="herdr-reviewr"
# Every line this step prints starts with the plugin's name, as the action lines do.
SAY="reviewr"
REPO="kuitos/herdr-reviewr"   # fork: releases come from the fork, not upstream

# The root from this script's place: build commands may not receive herdr's runtime env.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$ROOT/bin"

# The release tag matches the manifest version, so a checkout always pulls its own release.
VERSION="$(grep -m1 '^version' "$ROOT/herdr-plugin.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
TAG="v${VERSION}"

# Map the running platform to the release target triple.
os="$(uname -s)"
arch="$(uname -m)"
case "$os-$arch" in
  Darwin-arm64)              target="aarch64-apple-darwin" ;;
  Darwin-x86_64)             target="x86_64-apple-darwin" ;;
  Linux-aarch64 | Linux-arm64) target="aarch64-unknown-linux-musl" ;;
  Linux-x86_64)              target="x86_64-unknown-linux-musl" ;;
  *)
    echo "$SAY: no prebuilt binary for $os-$arch, build with 'cargo build --release' and copy target/release/herdr-reviewr into bin/" >&2
    exit 1
    ;;
esac

archive="${NAME}-${target}.tar.gz"
# taiki-e's checksum sidecar drops the archive extension: <name>-<target>.sha256, not <archive>.sha256.
checksum="${NAME}-${target}.sha256"
base="https://github.com/${REPO}/releases/download/${TAG}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# GitHub's CDN can 404 a fresh release's asset for minutes, so every failure retries, here
# rather than with curl's retry flags, which older curls (RHEL 8, CentOS 7) lack.
dl() {
  for _ in 1 2 3 4 5 6; do
    curl -fsSL "$1" -o "$2" && return
    sleep 3
  done
  return 1
}

echo "$SAY: downloading $archive ($TAG)"
dl "$base/$archive" "$tmp/$archive"
dl "$base/$checksum" "$tmp/$checksum"

echo "$SAY: verifying checksum"
expected="$(awk '{print $1}' "$tmp/$checksum")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$archive" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')"
fi
if [ "$expected" != "$actual" ]; then
  echo "$SAY: checksum mismatch (expected $expected, got $actual)" >&2
  exit 1
fi

mkdir -p "$BIN_DIR"
tar -xzf "$tmp/$archive" -C "$tmp"
install -m 0755 "$tmp/$NAME" "$BIN_DIR/$NAME"
echo "$SAY: installed $BIN_DIR/$NAME"

# Stable launch paths as symlinks, re-pointed each install; a user's own file there is left alone.
# A staging checkout gets renamed, so every action re-points them at the live root too.
LINK_ROOT="${HERDR_PLUGIN_ROOT:-$ROOT}"
link_binary() {
  if mkdir -p "$1" 2>/dev/null && { [ -L "$1/$NAME" ] || [ ! -e "$1/$NAME" ]; } &&
    ln -sfn "$LINK_ROOT/bin/$NAME" "$1/$NAME" 2>/dev/null; then
    echo "$SAY: linked $1/$NAME"
  else
    echo "$SAY: warning: could not link $1/$NAME" >&2
  fi
}
link_binary "$HOME/.local/state/herdr/plugins/persiyanov.reviewr/bin"
if [ -d "$HOME/.local/bin" ]; then
  link_binary "$HOME/.local/bin"
fi
