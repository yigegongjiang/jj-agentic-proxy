#!/usr/bin/env bash
# Install the latest jj-agentic-proxy binary from GitHub Releases.
# Usage: curl -fsSL https://raw.githubusercontent.com/yigegongjiang/jj-agentic-proxy/master/scripts/install.sh | bash

set -euo pipefail

REPO="yigegongjiang/jj-agentic-proxy"
INSTALL_DIR="$HOME/.local/bin"
BIN_NAME="${REPO##*/}"

err() { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || err "curl is required"

[ "$(uname -s)" = "Darwin" ] || err "unsupported OS: $(uname -s) (macOS only)"
case "$(uname -m)" in
  arm64)  arch="arm64" ;;
  x86_64) arch="x64" ;;
  *)      err "unsupported arch: $(uname -m)" ;;
esac

asset="${BIN_NAME}-darwin-${arch}"
base="https://github.com/${REPO}/releases/latest/download"

echo "==> Installing ${BIN_NAME} → ${INSTALL_DIR}"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
tmp_asset="${tmpdir}/${asset}"

curl -fL --progress-bar --retry 3 -o "$tmp_asset" "${base}/${asset}" || err "download failed"

if hash_line="$(curl -fsSL --retry 3 "${base}/checksums.txt" 2>/dev/null | grep " ${asset}$" || true)"; then
  if [ -n "$hash_line" ]; then
    expected="${hash_line%% *}"
    actual="$(shasum -a 256 "$tmp_asset" | awk '{print $1}')"
    [ "$expected" = "$actual" ] || err "checksum mismatch"
  fi
fi

mkdir -p "$INSTALL_DIR"
chmod +x "$tmp_asset"
mv -f "$tmp_asset" "${INSTALL_DIR}/${BIN_NAME}"

echo "==> Installed: ${INSTALL_DIR}/${BIN_NAME}"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "warning: add to PATH → export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
