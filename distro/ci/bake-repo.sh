#!/usr/bin/env bash
# Copies a built [ogos] repo (from build-repo.sh) into
# airootfs/root/ogos-repo, the offline copy baked into the ISO that
# install-ogos.sh installs from.
#
# Usage: bake-repo.sh <repo-dir>
set -euo pipefail

DISTRO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_DIR="$(cd "${1:?usage: $0 <repo-dir>}" && pwd)"
BAKED_REPO="$DISTRO_DIR/airootfs/root/ogos-repo"

mkdir -p "$BAKED_REPO"
rm -f "$BAKED_REPO"/*.pkg.tar.zst* "$BAKED_REPO"/ogos.*
cp "$REPO_DIR"/*.pkg.tar.zst* "$REPO_DIR"/ogos.* "$BAKED_REPO/"
echo "==> Baked $(find "$BAKED_REPO" -maxdepth 1 -name "*.pkg.tar.zst" | wc -l) packages into $BAKED_REPO"
