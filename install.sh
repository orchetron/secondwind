#!/bin/sh
# secondwind installer.
#
#   curl -fsSL https://raw.githubusercontent.com/orchetron/secondwind/main/install.sh | sh
#
# Downloads a prebuilt, checksum-verified binary for your platform. Falls back to
# building from source with cargo when no prebuilt binary is published for it.
#
# Overrides (env):
#   SECONDWIND_REPO   owner/name of the GitHub repo        (default below)
#   SECONDWIND_VERSION tag to install, e.g. v0.1.0         (default: latest)
#   INSTALL_DIR       where to put the binary              (default: ~/.local/bin)
#   SECONDWIND_FROM_SOURCE=1  skip the download, build with cargo
set -eu

REPO="${SECONDWIND_REPO:-orchetron/secondwind}"
VERSION="${SECONDWIND_VERSION:-latest}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
BIN="secondwind"

say() { printf '%s\n' "$*"; }
err() { printf 'install: %s\n' "$*" >&2; }
die() { err "$*"; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

target() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Darwin) os=apple-darwin ;;
    Linux)  os=unknown-linux-gnu ;;
    *) return 1 ;;
  esac
  case "$arch" in
    x86_64|amd64) arch=x86_64 ;;
    arm64|aarch64) arch=aarch64 ;;
    *) return 1 ;;
  esac
  printf '%s-%s' "$arch" "$os"
}

fetch() { # url dest
  if have curl; then curl -fsSL "$1" -o "$2"
  elif have wget; then wget -qO "$2" "$1"
  else die "need curl or wget"; fi
}

checksum_ok() { # file expected
  got=""
  if have sha256sum; then got=$(sha256sum "$1" | awk '{print $1}')
  elif have shasum; then got=$(shasum -a 256 "$1" | awk '{print $1}')
  else err "no sha256 tool, skipping verification"; return 0; fi
  [ "$got" = "$2" ] || die "checksum mismatch for $1"
}

from_source() {
  have cargo || die "no prebuilt binary for your platform and cargo is not installed: https://rustup.rs"
  say "building from source with cargo (this takes a minute)"
  cargo install --git "https://github.com/$REPO" "$BIN" --root "${INSTALL_DIR%/bin}"
  say "installed $BIN to $INSTALL_DIR"
  path_hint
  exit 0
}

path_hint() {
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) : ;;
    *) say ""; say "add $INSTALL_DIR to your PATH:"; say "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
  esac
}

main() {
  [ "${SECONDWIND_FROM_SOURCE:-0}" = "1" ] && from_source
  triple=$(target) || from_source

  if [ -n "${SECONDWIND_BASE_URL:-}" ]; then
    url="$SECONDWIND_BASE_URL"
  elif [ "$VERSION" = "latest" ]; then
    url="https://github.com/$REPO/releases/latest/download"
  else
    url="https://github.com/$REPO/releases/download/$VERSION"
  fi

  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  tarball="$BIN-$triple.tar.gz"

  say "downloading $tarball"
  if ! fetch "$url/$tarball" "$tmp/$tarball"; then
    err "no published binary for $triple"
    from_source
  fi

  if fetch "$url/SHA256SUMS" "$tmp/SHA256SUMS" 2>/dev/null; then
    want=$(grep " $tarball\$" "$tmp/SHA256SUMS" | awk '{print $1}')
    [ -n "$want" ] && checksum_ok "$tmp/$tarball" "$want"
  fi

  tar -xzf "$tmp/$tarball" -C "$tmp"
  mkdir -p "$INSTALL_DIR"
  install -m 0755 "$tmp/$BIN" "$INSTALL_DIR/$BIN" 2>/dev/null \
    || { mv "$tmp/$BIN" "$INSTALL_DIR/$BIN"; chmod 0755 "$INSTALL_DIR/$BIN"; }

  say "installed $("$INSTALL_DIR/$BIN" --version 2>/dev/null || echo "$BIN") to $INSTALL_DIR"
  path_hint
  say ""
  say "next:  $BIN doctor    then    $BIN dashboard"
}

main
