#!/usr/bin/env bash
# Build jj-agentic-proxy locally in release mode: CLI → ~/.local/bin, then the macOS app → /Applications.
# Usage: ./scripts/install-local.sh [--cli-only]   (run from anywhere; script cd's to the repo root automatically)
#   --cli-only   skip the app build/packaging step (app/package.sh)

set -euo pipefail

CLI_ONLY=0
for arg in "$@"; do
  case "$arg" in
    --cli-only) CLI_ONLY=1 ;;
    -h|--help) echo "usage: $0 [--cli-only]"; exit 0 ;;
    *) echo "unknown option: $arg (usage: $0 [--cli-only])" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")/.."

BIN_NAME="jj-agentic-proxy"
INSTALL_DIR="$HOME/.local/bin"

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

if [ "$CLI_ONLY" = 1 ]; then
  echo "==> Skipping app (--cli-only)"
  exit 0
fi

echo
./app/package.sh
