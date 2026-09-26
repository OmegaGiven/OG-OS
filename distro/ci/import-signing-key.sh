#!/usr/bin/env bash
# CI helper: imports the package-signing key from $OGOS_GPG_PRIVATE_KEY
# (ASCII-armored secret key) into a throwaway GNUPGHOME under
# $RUNNER_TEMP, and exports GNUPGHOME + OGOS_SIGN_KEY to later steps via
# $GITHUB_ENV so build-repo.sh signs with it.
#
# No secret configured = unsigned build, with a warning (forks and first
# runs before the key exists still work).
set -euo pipefail

if [ -z "${OGOS_GPG_PRIVATE_KEY:-}" ]; then
    echo "::warning::OGOS_GPG_PRIVATE_KEY not set — building an UNSIGNED repo"
    exit 0
fi

export GNUPGHOME="$RUNNER_TEMP/gnupg"
rm -rf "$GNUPGHOME"
mkdir -m 700 "$GNUPGHOME"
printf '%s\n' "$OGOS_GPG_PRIVATE_KEY" | gpg --batch --import
fpr="$(gpg --batch --with-colons --list-secret-keys | awk -F: '$1 == "fpr" { print $10; exit }')"
if [ -z "$fpr" ]; then
    echo "::error::no secret key found in OGOS_GPG_PRIVATE_KEY"
    exit 1
fi

{
    echo "GNUPGHOME=$GNUPGHOME"
    echo "OGOS_SIGN_KEY=$fpr"
} >> "$GITHUB_ENV"
echo "Signing with $fpr"
