#!/usr/bin/env bash
# Builds every OG-suite PKGBUILD, assembles the [ogos] pacman repo, and
# publishes it two places:
#   1. distro/local-repo/ -> distro/airootfs/root/ogos-repo/ (baked into
#      the ISO itself, so a fresh install works fully offline — see
#      install-ogos.sh's pacman-install.conf, which points here).
#   2. https://omegagiven.github.io/OG-os-repo/x86_64/ (a real hosted
#      repo an *installed* system's own /etc/pacman.conf points at
#      permanently — without this, an installed system has no update
#      path for its own desktop suite at all).
#
# Both copies come from the same build, same run — never let them drift.
# The build and publish steps live in ../ci/ so the CI workflows run the
# exact same code; this script is the local, all-in-one entry point.
#
# Requires: the OG-os-repo GitHub repo already cloned somewhere and its
# path passed as $1 (or set OGOS_REPO_CLONE), push access via `gh`/git
# credentials already configured. Honors build-repo.sh's env vars
# (OGOS_SIGN_KEY to sign, OGOS_PACKAGES to build a subset, ...).
set -euo pipefail

DISTRO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOCAL_REPO="$DISTRO_DIR/local-repo"
OGOS_REPO_CLONE="${1:-${OGOS_REPO_CLONE:-}}"

if [ -z "$OGOS_REPO_CLONE" ]; then
    echo "usage: $0 <path to a clone of OmegaGiven/OG-os-repo>" >&2
    echo "  (or set OGOS_REPO_CLONE env var)" >&2
    exit 1
fi

"$DISTRO_DIR/ci/build-repo.sh" "$LOCAL_REPO"
"$DISTRO_DIR/ci/bake-repo.sh" "$LOCAL_REPO"
"$DISTRO_DIR/ci/publish-pages.sh" "$LOCAL_REPO" "$OGOS_REPO_CLONE"
