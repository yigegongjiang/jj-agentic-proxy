#!/usr/bin/env bash
# Build jj-agentic-proxy locally in release mode and install it to ~/.local/bin — for local verification.
# Usage: ./scripts/install-local.sh   (run from anywhere; script cd's to the repo root automatically)
# Override target dir: INSTALL_DIR=/some/dir ./scripts/install-local.sh

set -euo pipefail

cd "$(dirname "$0")/.."

BIN_NAME="jj-agentic-proxy"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

echo "==> Building ${BIN_NAME} (release)"
cargo build --release

echo "==> Installing ${BIN_NAME} → ${INSTALL_DIR}"
mkdir -p "$INSTALL_DIR"
# Copy to a temp file then atomically rename: overwriting an executable in place
# invalidates the kernel's code-signature cache on macOS ("Killed: 9").
tmp_bin="${INSTALL_DIR}/.${BIN_NAME}.tmp.$$"
trap 'rm -f "$tmp_bin"' EXIT
cp -f "target/release/${BIN_NAME}" "$tmp_bin"
chmod +x "$tmp_bin"
mv -f "$tmp_bin" "${INSTALL_DIR}/${BIN_NAME}"

echo "==> Installed: ${INSTALL_DIR}/${BIN_NAME}"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "warning: add to PATH → export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

echo "==> Verifying"
"${INSTALL_DIR}/${BIN_NAME}" --version
