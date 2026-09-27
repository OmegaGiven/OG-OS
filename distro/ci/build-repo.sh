#!/usr/bin/env bash
# Builds every [ogos] PKGBUILD and assembles them into a pacman repo.
# Shared by publish.sh (local) and the CI package/ISO workflows, so both
# produce the repo exactly the same way.
#
# Usage: build-repo.sh <out-dir>
#
# Environment:
#   OGOS_SRC       source tree to build from (default: this repo). CI
#                  points it at the checked-out commit.
#   OGOS_PKGVER    package version for the OG-toolkit packages (default:
#                  derived from git: <commit date>.r<commit count>, which
#                  only ever increases along main).
#   OGOS_SIGN_KEY  GPG key id/fingerprint. When set, every package and the
#                  repo database are signed with it (the key must already
#                  be in the gpg keyring). Unset = unsigned repo.
#   OGOS_PACKAGES  space-separated subset to build (default: all).
#   CARGO_TARGET_DIR  shared cargo build dir (default:
#                  ~/.cache/ogos-build/cargo-target, reused across runs).
set -euo pipefail

DISTRO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKGBUILDS_DIR="$DISTRO_DIR/pkgbuilds"
OUT="${1:?usage: $0 <out-dir>}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
# Start clean: leftover packages from an older build would otherwise be
# swept into the repo by the repo-add glob below.
rm -f "$OUT"/*.pkg.tar.zst "$OUT"/*.pkg.tar.zst.sig "$OUT"/ogos.*

export OGOS_SRC="${OGOS_SRC:-$(cd "$DISTRO_DIR/.." && pwd)}"
if [ -z "${OGOS_PKGVER:-}" ]; then
    OGOS_PKGVER="$(git -C "$OGOS_SRC" log -1 --format=%cd --date=format:%Y%m%d).r$(git -C "$OGOS_SRC" rev-list --count HEAD)"
fi
export OGOS_PKGVER

# yay/calamares are third-party builds whose own PKGBUILD sources pull
# upstream release tarballs; the loop below is the same for them.
# calamares is a real C++/Qt6 build — expect it to dominate build time.
DEFAULT_PACKAGES="og-settings og-apps og-bar og-clip og-files og-notif-center og-notify og-search og-links og-scripts yay calamares"
read -r -a PACKAGES <<< "${OGOS_PACKAGES:-$DEFAULT_PACKAGES}"

SIGN_ARGS=()
if [ -n "${OGOS_SIGN_KEY:-}" ]; then
    SIGN_ARGS=(--sign --key "$OGOS_SIGN_KEY")
fi

# Build dependencies. Some packages depend on others built here (og-apps
# needs yay), which no Arch repo can provide, so `makepkg --syncdeps`
# can't be used. Instead install everything the PKGBUILDs need from the
# Arch repos up front — minus our own package names — and build with
# --nodeps. sudo is only invoked if something is actually missing.
echo "==> Resolving build dependencies"
own=()
deps=()
for p in "${PACKAGES[@]}"; do
    srcinfo="$(cd "$PKGBUILDS_DIR/$p" && makepkg --printsrcinfo)"
    while read -r name; do own+=("$name"); done < <(awk -F' = ' '/^pkgname = /{print $2}' <<< "$srcinfo")
    while read -r dep; do deps+=("$dep"); done < <(awk -F' = ' '/^\t(depends|makedepends) = /{print $2}' <<< "$srcinfo")
done
external=()
for d in "${deps[@]}"; do
    name="${d%%[<>=]*}"
    [[ " ${own[*]} " == *" $name "* ]] || external+=("$d")
done
mapfile -t missing < <(pacman -T "${external[@]}" || true)
if [ "${#missing[@]}" -gt 0 ]; then
    echo "    installing: ${missing[*]}"
    sudo pacman -S --needed --noconfirm --asdeps "${missing[@]}"
fi

# One cargo target dir shared by every package (and kept between runs):
# each PKGBUILD otherwise compiles the entire dependency graph from
# scratch in its own srcdir. PKGBUILDs install from $CARGO_TARGET_DIR
# when it's set.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/ogos-build/cargo-target}"
mkdir -p "$CARGO_TARGET_DIR"

echo "==> Building ${#PACKAGES[@]} packages (src=$OGOS_SRC ver=$OGOS_PKGVER signed=${OGOS_SIGN_KEY:+yes})"
for p in "${PACKAGES[@]}"; do
    echo "--- $p ---"
    dir="$PKGBUILDS_DIR/$p"
    rm -rf "$dir/src" "$dir/pkg" "$dir"/*.pkg.tar.zst "$dir"/*.pkg.tar.zst.sig 2>/dev/null || true
    ( cd "$dir" && makepkg -f --nodeps --noconfirm --skipchecksums "${SIGN_ARGS[@]}" )
    cp "$dir"/*.pkg.tar.zst "$OUT/"
    if [ -n "${OGOS_SIGN_KEY:-}" ]; then
        cp "$dir"/*.pkg.tar.zst.sig "$OUT/"
    fi
    # Debug split packages aren't part of the install set and just double
    # the repo's size.
    rm -f "$OUT"/*-debug-*.pkg.tar.zst "$OUT"/*-debug-*.pkg.tar.zst.sig
    rm -rf "$dir/src" "$dir/pkg" "$dir"/*.pkg.tar.zst "$dir"/*.pkg.tar.zst.sig
done

echo "==> repo-add"
REPO_ADD_ARGS=()
if [ -n "${OGOS_SIGN_KEY:-}" ]; then
    REPO_ADD_ARGS=(--sign --key "$OGOS_SIGN_KEY" --verify)
fi
( cd "$OUT" && repo-add "${REPO_ADD_ARGS[@]}" ogos.db.tar.gz ./*.pkg.tar.zst )

# repo-add's ogos.db/ogos.files are symlinks to the .tar.gz files — fine
# for file:// repos, but GitHub Pages doesn't reliably serve symlinks.
# Resolve them here so every consumer gets real files.
for l in ogos.db ogos.files ogos.db.sig ogos.files.sig; do
    if [ -L "$OUT/$l" ]; then
        cp --remove-destination "$(readlink -f "$OUT/$l")" "$OUT/$l"
    fi
done

echo "==> Repo ready in $OUT"
