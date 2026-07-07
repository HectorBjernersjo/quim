#!/usr/bin/env sh
# qb installer
#
# Usage:
#   curl -fsSL https://github.com/HectorBjernersjo/querybench/releases/latest/download/install.sh | sh
#
# Env vars:
#   QB_VERSION     Pin a specific tag (e.g. v0.1.0). Defaults to "latest".
#   QB_INSTALL_DIR Where to drop the binary. Defaults to $HOME/.local/bin.

set -eu

REPO="HectorBjernersjo/querybench"
VERSION="${QB_VERSION:-latest}"
INSTALL_DIR="${QB_INSTALL_DIR:-$HOME/.local/bin}"

uname_s="$(uname -s)"
uname_m="$(uname -m)"

case "$uname_s" in
    Linux)  os="unknown-linux-musl" ;;
    Darwin) os="apple-darwin" ;;
    *) echo "unsupported OS: $uname_s" >&2; exit 1 ;;
esac

case "$uname_m" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="aarch64" ;;
    *) echo "unsupported arch: $uname_m" >&2; exit 1 ;;
esac

target="${arch}-${os}"
asset="qb-${target}.tar.gz"

if [ "$VERSION" = "latest" ]; then
    url="https://github.com/${REPO}/releases/latest/download/${asset}"
else
    url="https://github.com/${REPO}/releases/download/${VERSION}/${asset}"
fi

echo "Installing qb (${VERSION}) for ${target} to ${INSTALL_DIR}"

mkdir -p "$INSTALL_DIR"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "$url" -o "$tmp/qb.tar.gz"
tar -xzf "$tmp/qb.tar.gz" -C "$tmp"
mv "$tmp/qb" "$INSTALL_DIR/qb"
chmod +x "$INSTALL_DIR/qb"

echo "Installed: $INSTALL_DIR/qb"

echo ""
echo "Get started:"
echo ""
echo "  qb           # launch the TUI (press 'a' to add a database)"
echo "  qb --check   # test config + connection + schema headlessly"
echo "  ?            # in the app: help / all keybindings"

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo ""; echo "NOTE: $INSTALL_DIR is not in your PATH. Add it to your shell rc:"; echo "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
