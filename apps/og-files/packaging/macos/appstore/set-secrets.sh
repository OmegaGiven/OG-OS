#!/usr/bin/env bash
# Uploads the Mac App Store signing secrets to the og-files GitHub repo
# from a folder holding the Apple signing files (the same certificates and
# App Store Connect API key the other OG apps use):
#
#   packaging/macos/appstore/set-secrets.sh ~/.config/og-paper-signing [profile]
#
# Expects in that folder: apple-distribution.p12 (+ .p12.password),
# mac-installer.p12 (+ .p12.password), AuthKey_<KEYID>.p8. The provisioning
# profile defaults to ~/Downloads/OG_Files_Mac_App_Store.provisionprofile.
# Needs gh (logged in) and openssl. Prints only secret names, never values.
set -euo pipefail
DIR=${1:?usage: set-secrets.sh SIGNING_DIR [PROFILE]}
PROFILE=${2:-$HOME/Downloads/OG_Files_Mac_App_Store.provisionprofile}
REPO=${REPO:-OmegaGiven/og-files}

need() { [ -f "$1" ] || { echo "missing: $1" >&2; exit 1; }; }
for f in apple-distribution.p12 apple-distribution.p12.password mac-installer.p12 mac-installer.p12.password; do
  need "$DIR/$f"
done
need "$PROFILE"
KEYFILE=$(ls "$DIR"/AuthKey_*.p8 2>/dev/null | head -1)
need "$KEYFILE"
KEY_ID=$(basename "$KEYFILE" .p8); KEY_ID=${KEY_ID#AuthKey_}

# signing identity = the certificate's common name
cn() {
  local pw; pw=$(cat "$2")
  openssl pkcs12 -in "$1" -nokeys -passin "pass:$pw" -legacy 2>/dev/null \
    || openssl pkcs12 -in "$1" -nokeys -passin "pass:$pw"
}
DIST_ID=$(cn "$DIR/apple-distribution.p12" "$DIR/apple-distribution.p12.password" | openssl x509 -noout -subject -nameopt multiline | sed -n 's/^ *commonName *= *//p')
INST_ID=$(cn "$DIR/mac-installer.p12" "$DIR/mac-installer.p12.password" | openssl x509 -noout -subject -nameopt multiline | sed -n 's/^ *commonName *= *//p')

ISSUER=${ASC_ISSUER:-}
if [ -z "$ISSUER" ]; then
  read -rp "App Store Connect issuer ID (App Store Connect > Users and Access > Integrations > Keys): " ISSUER
fi

put() { printf '%s' "$2" | gh secret set "$1" -R "$REPO" >/dev/null && echo "set $1"; }
put APPLE_DIST_CERTIFICATE "$(base64 -w0 "$DIR/apple-distribution.p12")"
put APPLE_DIST_CERTIFICATE_PASSWORD "$(cat "$DIR/apple-distribution.p12.password")"
put APPLE_DIST_SIGNING_IDENTITY "$DIST_ID"
put APPLE_INSTALLER_CERTIFICATE "$(base64 -w0 "$DIR/mac-installer.p12")"
put APPLE_INSTALLER_CERTIFICATE_PASSWORD "$(cat "$DIR/mac-installer.p12.password")"
put APPLE_INSTALLER_SIGNING_IDENTITY "$INST_ID"
put APPLE_MAC_PROVISIONING_PROFILE "$(base64 -w0 "$PROFILE")"
put APPLE_API_KEY "$(cat "$KEYFILE")"
put APPLE_API_KEY_ID "$KEY_ID"
put APPLE_API_ISSUER "$ISSUER"
echo "Done. Identities: $DIST_ID / $INST_ID"
