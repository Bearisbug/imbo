#!/bin/sh
# Imbo installer: curl -fsSL https://imbo.dev/install | sh
# Downloads the imbo binary for this computer, checks it, puts it in
# ~/.local/bin and runs `imbo setup`. Arguments after `sh -s --` go to setup,
# e.g. `sh -s -- --server https://imbo.example.com`.
set -eu

BASE="${IMBO_DOWNLOAD_BASE:-https://github.com/Bearisbug/imbo/releases/latest/download}"
BIN_DIR="${IMBO_BIN_DIR:-$HOME/.local/bin}"
SUPPORTED="macos-aarch64 macos-x86_64 linux-x86_64 linux-aarch64"

if [ -t 2 ]; then
  B="$(printf '\033[1m')"; D="$(printf '\033[2m')"; G="$(printf '\033[32m')"; R="$(printf '\033[31m')"; N="$(printf '\033[0m')"
else
  B=""; D=""; G=""; R=""; N=""
fi
say() { printf '%s\n' "$*" >&2; }
ok() { say "${G}✓${N} $*"; }
die() { say "${R}✗${N} $*"; exit 1; }

raw_os="${IMBO_UNAME_S:-$(uname -s)}"
raw_arch="${IMBO_UNAME_M:-$(uname -m)}"
os=$raw_os
arch=$raw_arch
case "$os" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) os="unsupported:$os" ;;
esac
case "$arch" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) arch="unsupported:$arch" ;;
esac
platform="$os-$arch"
case " $SUPPORTED " in
  *" $platform "*) ;;
  *) die "unsupported platform: $raw_os $raw_arch. Imbo runs on: $SUPPORTED" ;;
esac

say "${B}imbo${N} ${D}installer${N}"
ok "platform $platform"

command -v curl >/dev/null 2>&1 || die "curl is required"
if command -v shasum >/dev/null 2>&1; then
  sha() { shasum -a 256 "$1" | cut -d' ' -f1; }
elif command -v sha256sum >/dev/null 2>&1; then
  sha() { sha256sum "$1" | cut -d' ' -f1; }
else
  die "need shasum or sha256sum to check the download"
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/imbo-install.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM
pkg="imbo-$platform.tar.gz"

say "${D}downloading $BASE/$pkg${N}"
if [ -t 2 ]; then progress="-#"; else progress="-s"; fi
curl -fL $progress -o "$tmp/$pkg" "$BASE/$pkg" || die "download failed: $BASE/$pkg"
curl -fsSL -o "$tmp/checksums.txt" "$BASE/checksums.txt" || die "download failed: $BASE/checksums.txt"

want="$(grep " $pkg\$" "$tmp/checksums.txt" | cut -d' ' -f1 | head -n1)"
[ -n "$want" ] || die "checksum mismatch: $pkg is not listed in checksums.txt"
got="$(sha "$tmp/$pkg")"
[ "$want" = "$got" ] || die "checksum mismatch for $pkg (expected $want, got $got); nothing was installed"
ok "checksum ok"

tar -xzf "$tmp/$pkg" -C "$tmp"
[ -f "$tmp/imbo" ] || die "the package has no imbo binary"
mkdir -p "$BIN_DIR"
chmod 755 "$tmp/imbo"
mv -f "$tmp/imbo" "$BIN_DIR/imbo"
ok "installed $BIN_DIR/imbo ${D}($("$BIN_DIR/imbo" --version 2>/dev/null || echo imbo))${N}"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    say "${D}$BIN_DIR is not on your PATH. Add it:${N}"
    case "${SHELL:-}" in
      */zsh) say "  echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.zshrc" ;;
      */bash) say "  echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.bashrc" ;;
      */fish) say "  fish_add_path $BIN_DIR" ;;
      *) say "  export PATH=\"$BIN_DIR:\$PATH\"" ;;
    esac
    ;;
esac

say ""
if [ -r /dev/tty ] && ( : < /dev/tty ) 2>/dev/null; then
  # exec replaces this shell, so the EXIT trap never runs: clean up first.
  rm -rf "$tmp"
  trap - EXIT INT TERM
  exec "$BIN_DIR/imbo" setup "$@" < /dev/tty
else
  say "Next: run ${B}imbo setup${N} in a terminal to sign in and connect your AI coding tools."
fi
