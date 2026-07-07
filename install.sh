#!/usr/bin/env sh
# quim installer
#
# Usage:
#   curl -fsSL https://github.com/HectorBjernersjo/quim/releases/latest/download/install.sh | sh
#
# Env vars:
#   QUIM_VERSION     Pin a specific tag (e.g. v0.1.0). Defaults to "latest".
#   QUIM_INSTALL_DIR Where to drop the binary. Defaults to $HOME/.local/bin.

set -eu

REPO="HectorBjernersjo/quim"
VERSION="${QUIM_VERSION:-latest}"
INSTALL_DIR="${QUIM_INSTALL_DIR:-$HOME/.local/bin}"

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
asset="quim-${target}.tar.gz"

if [ "$VERSION" = "latest" ]; then
    url="https://github.com/${REPO}/releases/latest/download/${asset}"
else
    url="https://github.com/${REPO}/releases/download/${VERSION}/${asset}"
fi

echo "Installing quim (${VERSION}) for ${target} to ${INSTALL_DIR}"

mkdir -p "$INSTALL_DIR"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "$url" -o "$tmp/quim.tar.gz"
tar -xzf "$tmp/quim.tar.gz" -C "$tmp"
mv "$tmp/quim" "$INSTALL_DIR/quim"
chmod +x "$INSTALL_DIR/quim"

echo "Installed: $INSTALL_DIR/quim"

echo ""
echo "Get started:"
echo ""
echo "  quim           # launch the TUI (press 'a' to add a database)"
echo "  quim --check   # test config + connection + schema headlessly"
echo "  ?            # in the app: help / all keybindings"

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo ""; echo "NOTE: $INSTALL_DIR is not in your PATH. Add it to your shell rc:"; echo "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
