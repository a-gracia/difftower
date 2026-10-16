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
asset="difftower-x86_64-unknown-linux-gnu.tar.gz"
prefix="${DIFFTOWER_PREFIX:-$HOME/.local}"
version="${DIFFTOWER_VERSION:-latest}"

say() { printf '%s\n' "$*"; }
fail() { printf 'difftower: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = Linux ] || fail "only Linux is supported for now; macOS is coming."
[ "$(uname -m)" = x86_64 ] || fail "only x86_64 is supported for now, not $(uname -m)."
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

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "Downloading Difftower ($version)..."
fetch "$base/$asset" "$tmp/$asset" || fail "could not download $base/$asset"
fetch "$base/$asset.sha256" "$tmp/$asset.sha256" || fail "could not download the checksum"
(cd "$tmp" && sha256sum -c "$asset.sha256" >/dev/null) || fail "the download does not match its checksum"

tar -C "$tmp" -xzf "$tmp/$asset"
dir="$tmp/${asset%.tar.gz}"

install -Dm755 "$dir/difftower" "$prefix/bin/difftower"
install -Dm644 "$dir/difftower.svg" "$prefix/share/icons/hicolor/scalable/apps/difftower.svg"
# The launcher entry runs this binary even where $prefix/bin is not on PATH.
sed "s|^Exec=difftower|Exec=$prefix/bin/difftower|" "$dir/difftower.desktop" \
    > "$tmp/difftower.desktop"
install -Dm644 "$tmp/difftower.desktop" "$prefix/share/applications/difftower.desktop"
command -v update-desktop-database >/dev/null 2>&1 \
    && update-desktop-database -q "$prefix/share/applications" 2>/dev/null || true

say "Installed $("$prefix/bin/difftower" --version) in $prefix/bin."
case ":$PATH:" in
    *":$prefix/bin:"*) say "Run: difftower path/to/repo" ;;
    *) say "Add $prefix/bin to your PATH, or run: $prefix/bin/difftower path/to/repo" ;;
esac
