#!/bin/sh
# Installs Difftower for the current user, no root needed:
#
#   curl -fsSL https://difftower.com/install.sh | sh
#
# DIFFTOWER_VERSION=v0.1.0 picks a release (default: the latest),
# DIFFTOWER_PREFIX a prefix other than ~/.local, and DIFFTOWER_BASE_URL
# another place to download from, such as a mirror.
set -eu

repo="a-gracia/difftower"
prefix="${DIFFTOWER_PREFIX:-$HOME/.local}"
version="${DIFFTOWER_VERSION:-latest}"

say() { printf '%s\n' "$*"; }
fail() { printf 'difftower: %s\n' "$*" >&2; exit 1; }

os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
    Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
    Darwin/arm64) target=aarch64-apple-darwin ;;
    Darwin/x86_64) target=x86_64-apple-darwin ;;
    Linux/*) fail "only x86_64 is supported on Linux for now, not $arch." ;;
    *) fail "$os is not supported yet; Difftower runs on Linux and macOS." ;;
esac
asset="difftower-$target.tar.gz"
command -v git >/dev/null 2>&1 || say "note: Difftower needs git; install it too."

if [ -n "${DIFFTOWER_BASE_URL:-}" ]; then
    base="$DIFFTOWER_BASE_URL"
elif [ "$version" = latest ]; then
    base="https://github.com/$repo/releases/latest/download"
else
    base="https://github.com/$repo/releases/download/$version"
fi

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
else
    fail "needs curl or wget."
fi
if command -v sha256sum >/dev/null 2>&1; then
    check() { sha256sum -c "$1"; }
else
    check() { shasum -a 256 -c "$1"; }
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "Downloading Difftower ($version)..."
fetch "$base/$asset" "$tmp/$asset" || fail "could not download $base/$asset"
fetch "$base/$asset.sha256" "$tmp/$asset.sha256" || fail "could not download the checksum"
(cd "$tmp" && check "$asset.sha256" >/dev/null) || fail "the download does not match its checksum"

tar -C "$tmp" -xzf "$tmp/$asset"
dir="$tmp/${asset%.tar.gz}"

mkdir -p "$prefix/bin"
# Beside the old binary, then moved over it, so a running Difftower is not
# cut short.
cp "$dir/difftower" "$prefix/bin/.difftower.new"
chmod 755 "$prefix/bin/.difftower.new"
mv -f "$prefix/bin/.difftower.new" "$prefix/bin/difftower"

if [ "$os" = Linux ]; then
    mkdir -p "$prefix/share/icons/hicolor/scalable/apps" "$prefix/share/applications"
    cp "$dir/difftower.svg" "$prefix/share/icons/hicolor/scalable/apps/difftower.svg"
    # The launcher entry runs this binary even where $prefix/bin is not on PATH.
    sed "s|^Exec=difftower|Exec=$prefix/bin/difftower|" "$dir/difftower.desktop" \
        > "$prefix/share/applications/difftower.desktop"
    command -v update-desktop-database >/dev/null 2>&1 \
        && update-desktop-database -q "$prefix/share/applications" 2>/dev/null || true
fi

say "Installed $("$prefix/bin/difftower" --version) in $prefix/bin."
case ":$PATH:" in
    *":$prefix/bin:"*) say "Run: difftower path/to/repo" ;;
    *) say "Add $prefix/bin to your PATH, or run: $prefix/bin/difftower path/to/repo" ;;
esac
