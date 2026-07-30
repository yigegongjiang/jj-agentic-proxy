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
tmp_checksums="${tmpdir}/checksums.txt"

curl -fL --progress-bar --retry 3 -o "$tmp_asset" "${base}/${asset}" || err "download failed"
curl -fL --progress-bar --retry 3 -o "$tmp_checksums" "${base}/checksums.txt" \
  || err "checksum download failed"
expected="$(awk -v asset="$asset" '$2 == asset { print $1; found++ } END { if (found != 1) exit 1 }' "$tmp_checksums")" \
  || err "checksum entry missing or duplicated: ${asset}"
actual="$(shasum -a 256 "$tmp_asset" | awk '{print $1}')"
[ "$expected" = "$actual" ] || err "checksum mismatch"

mkdir -p "$INSTALL_DIR"
tmp_bin="${INSTALL_DIR}/.${BIN_NAME}.tmp.$$"
trap 'rm -rf "$tmpdir"; rm -f "$tmp_bin"' EXIT
cp -f "$tmp_asset" "$tmp_bin"
chmod +x "$tmp_bin"
mv -f "$tmp_bin" "${INSTALL_DIR}/${BIN_NAME}"

echo "==> Installed: ${INSTALL_DIR}/${BIN_NAME}"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "warning: add to PATH → export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
