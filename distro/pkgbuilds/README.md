# pkgbuilds/

Phase 2 of `OS-PLAN.md`. One PKGBUILD per own-repo, run on the dev
machine (this box) with `makepkg` — NOT built inside the archiso chroot.
Output `.pkg.tar.zst` files get `repo-add`ed into `../local-repo/` as the
`[ogos]` pacman repo the ISO's `packages.x86_64`/`pacman.conf` consumes.

## Why local `git+file://` sources

Every PKGBUILD here sources from `$HOME/.local/src/<repo>` via
`git+file://` rather than GitHub, because most of the OG-suite path-deps
on `OG-toolkit`'s shared crates (`og-config`, `og-theme`, `og-drag`,
`og-wayland`) as a **sibling directory** (`../OG-toolkit/crates/...` in
each app's `Cargo.toml`) — makepkg's `$srcdir` layout reproduces that by
naming both sources explicitly (`og-bar::git+file://...`,
`OG-toolkit::git+file://...`), so `$srcdir/og-bar` and `$srcdir/OG-toolkit`
end up as siblings, matching what the path deps expect. Building against
whatever's actually pushed to GitHub would risk a stale/unpushed local
state silently diverging from the packaged binary.

`og-clip` and `og-notif-center` had no `.git` at all before this — git-init'd
locally (single "Initial commit") so the same `git+file://` pattern
applies uniformly across all 9. No GitHub remote for those two yet.

## Repos packaged (9)

`og-bar`, `og-settings` (repo: sway-control), `og-search` (repo:
omegagiven-search), `og-clip`, `og-notif-center`, `og-notify`, `og-apps`
(repo: app-store), `og-files` (repo: file-manager), `og-links` (repo:
galias).

**Not packaged yet**: `og-note` (Tauri+Svelte+npm, not a plain cargo
build — needs its own PKGBUILD approach with webkit2gtk, out of scope
for this batch), `og-hotkeys` / `og-wallpaper` / `og-wallpaper-studio`
(exist and build fine, but weren't in `BASE-PACKAGES.md`'s original
10-package OG-suite base list — add PKGBUILDs for these later if they
get promoted into base).

## Build order

`OG-toolkit` itself has no PKGBUILD (it's a crates-only lib repo, never
installed as a package) — it's pulled in as a build-time source by every
app's PKGBUILD instead.

Each app PKGBUILD is independent (all pull their own `OG-toolkit`
snapshot at build time), so there's no cross-package build order — build
any/all of the 9 in any order:

```sh
for d in og-bar og-settings og-search og-clip og-notif-center og-notify og-apps og-files og-links; do
  (cd "$d" && makepkg -si --noconfirm)
done
```

(`-s` installs missing makedeps via pacman, `-i` installs the built
package after — drop `-i` once these are meant to go into `local-repo/`
instead of straight onto this machine.)

## Known gaps

- No `LICENSE` file in any of the 12 source repos — `license=('unknown')`
  in every PKGBUILD until that's picked.
- No version tags anywhere — all repos are `0.1.0` in `Cargo.toml` with
  no `pkgver()` bump strategy yet; bump manually per release until a real
  tagging scheme exists.
- `og-links`' default bind port (80) needs `cap_net_bind_service` or root
  — the shipped systemd unit uses 8080 instead to avoid granting that
  automatically; see the security note in `OS-PLAN.md` about
  NOPASSWD/setcap-style privilege grants before changing this.
