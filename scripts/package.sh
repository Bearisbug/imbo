#!/bin/sh
# Build release packages into crates/dist: imbo-<platform>.tar.gz,
# checksums.txt, install.sh and version, laid out like a GitHub release.
#   scripts/package.sh                 # this computer's platform
#   scripts/package.sh <target> <platform> [<target> <platform> …]
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
DIST="$ROOT/dist"
mkdir -p "$DIST"

host_platform() {
  os=$(uname -s); arch=$(uname -m)
  case "$os" in Darwin) os=macos ;; Linux) os=linux ;; esac
  case "$arch" in arm64 | aarch64) arch=aarch64 ;; x86_64 | amd64) arch=x86_64 ;; esac
  echo "$os-$arch"
}

pack() { # <binary> <platform>
  stage=$(mktemp -d)
  cp "$1" "$stage/imbo"
  chmod 755 "$stage/imbo"
  tar -czf "$DIST/imbo-$2.tar.gz" -C "$stage" imbo
  rm -rf "$stage"
  echo "packed imbo-$2.tar.gz"
}

if [ $# -eq 0 ]; then
  cargo build --release -p imbo
  pack target/release/imbo "$(host_platform)"
else
  while [ $# -ge 2 ]; do
    cargo build --release -p imbo --target "$1"
    pack "target/$1/release/imbo" "$2"
    shift 2
  done
fi

cp install.sh "$DIST/install.sh"
sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1 > "$DIST/version"
cd "$DIST"
if command -v shasum >/dev/null 2>&1; then
  shasum -a 256 imbo-*.tar.gz > checksums.txt
else
  sha256sum imbo-*.tar.gz > checksums.txt
fi
cat checksums.txt
