#!/usr/bin/env bash
# Copies a built [ogos] repo (from build-repo.sh) into a clone of
# OmegaGiven/OG-os-repo and pushes it. That repo is served by GitHub Pages
# at https://omegagiven.github.io/OG-os-repo/x86_64/ and is the permanent
# update path an installed system's /etc/pacman.conf points at.
#
# Usage: publish-pages.sh <repo-dir> <OG-os-repo clone>
set -euo pipefail

REPO_DIR="$(cd "${1:?usage: $0 <repo-dir> <OG-os-repo clone>}" && pwd)"
CLONE="${2:?usage: $0 <repo-dir> <OG-os-repo clone>}"

if [ ! -d "$CLONE/.git" ]; then
    echo "error: $CLONE is not a git clone (gh repo clone OmegaGiven/OG-os-repo)" >&2
    exit 1
fi

mkdir -p "$CLONE/x86_64"
rm -f "$CLONE"/x86_64/*.pkg.tar.zst* "$CLONE"/x86_64/ogos.*
cp "$REPO_DIR"/*.pkg.tar.zst* "$REPO_DIR"/ogos.* "$CLONE/x86_64/"
touch "$CLONE/.nojekyll"

cd "$CLONE"
git add -A
if git diff --cached --quiet; then
    echo "nothing changed, skipping commit/push"
    exit 0
fi
git commit -q -m "Publish OG-OS suite packages ${OGOS_PKGVER:-$(date +%Y%m%d)}"
git push
echo "==> Published. Verify: curl -sI https://omegagiven.github.io/OG-os-repo/x86_64/ogos.db"
