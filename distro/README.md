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

See OS-PLAN.md section 5 for phase order and current status.
