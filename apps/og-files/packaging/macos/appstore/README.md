# Mac App Store build

Automatic: every release (`release.yml`, which runs when `main` carries a
version in Cargo.toml that has no GitHub Release yet) also runs
`.github/workflows/appstore.yml`. It builds a universal app with
`--no-default-features --features appstore`, signs it, packages a signed
`.pkg`, validates and uploads it to App Store Connect (TestFlight), makes
sure the macOS App Store version matching Cargo.toml exists (renaming the
one still being prepared, or creating the next one, which copies the
listing) and attaches the build once Apple has processed it (`asc.py`).
It never submits for review: press "Add for Review" in App Store Connect.
Until the signing secrets below exist, the job is skipped with a notice.
It can also be run by hand (Actions > Mac App Store > Run workflow). The
build number is the UTC date and time, so it always grows.

To release: bump `version` in `Cargo.toml` and push to `main`.

What's different from the direct-download build:
- **Sandboxed** (`OGFiles.entitlements`). Folders are read only after the
  user grants them in the open panel; grants are kept as security-scoped
  bookmarks in the container (`src/macos.rs`). Downloads, Pictures, Music
  and Movies are allowed up front.
- **No GPL code**: no libmpv, no FFmpeg. Video plays through AVFoundation
  (`src/viewers/video/av.rs`): mp4, m4v, mov (H.264, HEVC, ProRes); not
  mkv/webm. The workflow fails if the binary links either library.
- **No** network mounts (rclone/FUSE, ssh config, tailnet), "Open terminal
  here" or disk eject: the sandbox can't run those tools.
- Opening files and "Open with" go through NSWorkspace, the Trash through
  NSFileManager (both also used by the direct-download build).
- `PrivacyInfo.xcprivacy`: no tracking, no data collected; the required-
  reason APIs a file manager uses.
- `review-notes.txt`: paste into App Review Information > Notes.

## One-time setup

Bundle ID `com.omegagiven.ogfiles`, team `FGFW7ZAJUC` (the same Apple
developer account as OG Paper).

1. developer.apple.com > Identifiers: register the App ID
   `com.omegagiven.ogfiles` (macOS; no extra capabilities).
2. Profiles: a **Mac App Store** distribution profile for that App ID with
   the Apple Distribution certificate; download it.
3. App Store Connect > Apps > +: new macOS app "OG Files", bundle ID
   `com.omegagiven.ogfiles`, SKU e.g. `ogfiles`.
4. Repository secrets on `OmegaGiven/og-files` (the certificates and API
   key are the ones OG Paper already uses):

| Secret | What |
| --- | --- |
| `APPLE_DIST_CERTIFICATE`, `_PASSWORD` | Apple Distribution certificate (.p12, base64) and its password |
| `APPLE_DIST_SIGNING_IDENTITY` | `Apple Distribution: <name> (FGFW7ZAJUC)` |
| `APPLE_INSTALLER_CERTIFICATE`, `_PASSWORD` | Mac Installer Distribution certificate (.p12, base64) and its password |
| `APPLE_INSTALLER_SIGNING_IDENTITY` | `3rd Party Mac Developer Installer: <name> (FGFW7ZAJUC)` |
| `APPLE_MAC_PROVISIONING_PROFILE` | the profile from step 2 (base64) |
| `APPLE_API_KEY`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER` | App Store Connect API key (.p8 contents), its ID and the issuer ID |

`.p12` files made with OpenSSL 3 need `openssl pkcs12 -export -legacy`, or
the Mac keychain refuses them.

## Direct downloads: signing and notarization (optional)

`release.yml` signs the .dmg/.zip with a **Developer ID Application**
certificate and notarizes them when these secrets exist (otherwise the app
is ad-hoc signed and users right-click > Open the first time):

| Secret | What |
| --- | --- |
| `APPLE_DEVID_CERTIFICATE`, `_PASSWORD` | Developer ID Application certificate (.p12, base64) and its password |
| `APPLE_DEVID_SIGNING_IDENTITY` | `Developer ID Application: <name> (FGFW7ZAJUC)` |

Notarization reuses the `APPLE_API_*` key above.
