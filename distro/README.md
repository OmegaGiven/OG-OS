# og-os

Everything that turns the OG-OS apps into an installable Arch-based ISO:
the archiso profile (`profiledef.sh`, `pacman.conf`, `packages.x86_64`,
`airootfs/`), the `[ogos]` PKGBUILDs (`pkgbuilds/`), the build/publish
scripts (`ci/`, see `CI.md`), and the VM install tests (`testing/`).

- `OS-PLAN.md` — the plan (approach, phases, open decisions).
- `packages.x86_64` — frozen snapshot of `pacman -Qqe` (official-repo explicit packages).
- `foreign-packages.txt` — frozen snapshot of `pacman -Qqm` (AUR/foreign packages).
- `INVENTORY-DATE.txt` — when the above snapshots were taken; re-freeze periodically, don't assume they stay current.
- `CI.md` — CI/CD pipeline, runner setup, signing, release blockers.
- `testing/README.md` — VM install tests, and **"Install speed"**: the
  per-step install timing table (Calamares click-to-finish 107 s → ~40 s
  in a KVM VM) and what the installer does after the image copy.

Installer: Calamares (auto-starts in the live session) copies the live
squashfs onto the disk (`unpackfs`), then only edits files —
`airootfs/usr/local/lib/ogos-installer/target-setup` strips live-only
state and writes the boot entry; one `mkinitcpio` run, no package
transactions or downloads. `airootfs/root/install-ogos.sh` is the older
pacstrap-based script installer, kept as a manual fallback (it is what
`/root/ogos-repo` on the ISO exists for).

See OS-PLAN.md section 5 for phase order and current status.
