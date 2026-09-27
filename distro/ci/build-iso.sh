#!/usr/bin/env bash
# Builds the OG-OS ISO from a freshly built [ogos] repo (build-repo.sh),
# so the ISO contains exactly the packages from this source tree rather
# than whatever is currently published to OG-os-repo.
#
# Usage: build-iso.sh <repo-dir> <out-dir>
#
# Needs: archiso, and root via sudo for mkarchiso. Honors
# SOURCE_DATE_EPOCH (ISO label/version) and, when OGOS_SIGN_KEY is set,
# writes a detached signature next to the ISO. Always writes <iso>.sha256.
set -euo pipefail

DISTRO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_DIR="$(cd "${1:?usage: $0 <repo-dir> <out-dir>}" && pwd)"
OUT="${2:?usage: $0 <repo-dir> <out-dir>}"
TMP="${RUNNER_TEMP:-$(mktemp -d)}"
PROFILE="$TMP/profile"
WORK="$TMP/work"

# GitHub release assets are capped at 2 GiB per file.
MAX_ISO_BYTES=$((2 * 1024 * 1024 * 1024))

# 1. The offline copy the installer uses, baked into airootfs.
"$DISTRO_DIR/ci/bake-repo.sh" "$REPO_DIR"

# 2. A throwaway copy of the profile whose [ogos] repo is the local build.
sudo rm -rf "$PROFILE" "$WORK"
mkdir -p "$PROFILE" "$OUT"
cp -a "$DISTRO_DIR"/. "$PROFILE"/
rm -rf "$PROFILE/work" "$PROFILE/out" "$PROFILE/local-repo"
sed -i "/^\[ogos\]/,/^\[/ s#^Server = .*#Server = file://$REPO_DIR#" "$PROFILE/pacman.conf"
grep -A2 '^\[ogos\]' "$PROFILE/pacman.conf"

# 3. Build.
# SOURCE_DATE_EPOCH reaches mkarchiso via sudoers env_keep (see CI.md);
# without it the ISO label/version just fall back to the build date.
sudo mkarchiso -v -w "$WORK" -o "$OUT" "$PROFILE"
sudo chown -R "$(id -u):$(id -g)" "$OUT"

iso="$(find "$OUT" -maxdepth 1 -name '*.iso' | head -1)"
size="$(stat -c %s "$iso")"
echo "==> $iso ($((size / 1024 / 1024)) MiB)"
if [ "$size" -gt "$MAX_ISO_BYTES" ]; then
    echo "::warning::ISO is over 2 GiB — too big to attach to a GitHub release"
fi

( cd "$OUT" && sha256sum "$(basename "$iso")" > "$(basename "$iso").sha256" )
if [ -n "${OGOS_SIGN_KEY:-}" ]; then
    gpg --batch --yes --local-user "$OGOS_SIGN_KEY" --detach-sign "$iso"
fi
