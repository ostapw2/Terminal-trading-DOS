#!/bin/sh
#
# install.sh — installer for dos-commander (macOS, Linux).
#
#   curl -fsSL https://raw.githubusercontent.com/ostapw2/trading/main/install.sh | sh
#
# Downloads the archive for this machine from the latest GitHub release,
# VERIFIES its SHA-256, and puts the binary in ~/.local/bin (no sudo).
# Re-running upgrades.  Windows: use install.ps1.
#
# Environment: DOS_TAG (default: latest), DOS_INSTALL_DIR, DOS_REPO.
# While the repository is private the download needs the GitHub CLI: run
# `gh auth login` once and this script uses `gh release download`.

set -eu

REPO="${DOS_REPO:-ostapw2/trading}"
INSTALL_DIR="${DOS_INSTALL_DIR:-${HOME}/.local/bin}"
TAG="${DOS_TAG:-}"
BASE="${DOS_BASE_URL:-https://github.com/${REPO}/releases/download}"  # tests override

die() { printf 'error: %s\n' "$1" 1>&2; exit 1; }

# ── platform ──────────────────────────────────────────────────────────

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case "${OS}-${ARCH}" in
    linux-x86_64)               TARGET="x86_64-unknown-linux-musl";;
    linux-aarch64|linux-arm64)  TARGET="aarch64-unknown-linux-musl";;
    darwin-arm64)               TARGET="aarch64-apple-darwin";;
    darwin-x86_64)              TARGET="x86_64-apple-darwin";;
    *) die "unsupported platform ${OS}-${ARCH} (Windows: use install.ps1)";;
esac

HAVE_GH=0
if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then HAVE_GH=1; fi

# ── tag ───────────────────────────────────────────────────────────────

if [ -z "$TAG" ]; then
    TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null \
            | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1 || true)
fi
if [ -z "$TAG" ] && [ "$HAVE_GH" = 1 ]; then
    TAG=$(gh release view --repo "$REPO" --json tagName -q .tagName 2>/dev/null || true)
fi
[ -n "$TAG" ] || die "could not find a release of ${REPO} (private repo? run: gh auth login)"

# ── download ──────────────────────────────────────────────────────────

ASSET="dos-${TAG}-${TARGET}.tar.gz"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

printf 'Downloading %s (%s)...\n' "$ASSET" "$TAG"
if ! curl -fsL -o "$TMP/$ASSET" "${BASE}/${TAG}/${ASSET}" 2>/dev/null \
   || ! curl -fsL -o "$TMP/$ASSET.sha256" "${BASE}/${TAG}/${ASSET}.sha256" 2>/dev/null; then
    [ "$HAVE_GH" = 1 ] || die "download failed (private repo? run: gh auth login, then retry)"
    gh release download "$TAG" --repo "$REPO" --dir "$TMP" --clobber \
        --pattern "$ASSET" --pattern "$ASSET.sha256" || die "download failed"
fi

# ── verify ────────────────────────────────────────────────────────────

WANT=$(awk '{print $1}' "$TMP/$ASSET.sha256")
if command -v sha256sum >/dev/null 2>&1; then
    GOT=$(sha256sum "$TMP/$ASSET" | awk '{print $1}')
else
    GOT=$(shasum -a 256 "$TMP/$ASSET" | awk '{print $1}')
fi
[ -n "$WANT" ] && [ "$WANT" = "$GOT" ] || die "checksum mismatch, nothing installed"
printf 'Checksum OK.\n'

# ── install ───────────────────────────────────────────────────────────

tar -C "$TMP" -xzf "$TMP/$ASSET" dos-commander
mkdir -p "$INSTALL_DIR"
mv "$TMP/dos-commander" "$INSTALL_DIR/dos-commander"
chmod +x "$INSTALL_DIR/dos-commander"

printf '\nInstalled dos-commander %s into %s\n' "$TAG" "$INSTALL_DIR"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        printf '\n%s is not in your PATH. Add this line to your shell profile:\n' "$INSTALL_DIR"
        printf '  export PATH="%s:$PATH"\n' "$INSTALL_DIR"
        ;;
esac
printf '\nTry it without keys or network:\n  dos-commander --demo\n'
