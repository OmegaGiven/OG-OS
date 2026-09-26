# CI/CD

Three workflows under `.github/workflows/`. Local builds and CI run the
same scripts in `distro/ci/`, so "works on my machine" and "works in CI"
build the packages and ISO the same way.

| Workflow | Trigger | Runner | Does |
|---|---|---|---|
| `ci.yml` | every push / PR | GitHub-hosted (Arch container) | workspace build + tests + clippy (clippy is reported, not enforced yet), PKGBUILD parse check, shellcheck on `distro/ci` |
| `packages.yml` | manual | self-hosted `ogos-builder` | build the signed `[ogos]` repo from the commit; optionally publish it to `OmegaGiven/OG-os-repo` (GitHub Pages — the update channel installed systems use) |
| `iso.yml` | manual | self-hosted `ogos-builder` | repo → ISO (installs exactly this commit's packages) → optional full VM install test → optional GitHub pre-release with `.sha256` and `.sig` |

## Scripts (`distro/ci/`)

| Script | Purpose |
|---|---|
| `build-repo.sh <out>` | build every PKGBUILD from `$OGOS_SRC` (default: this repo), `repo-add`, sign when `$OGOS_SIGN_KEY` is set. Version = `<commit date>.r<commit count>` so pacman upgrades are monotonic |
| `bake-repo.sh <repo>` | copy the repo into `airootfs/root/ogos-repo` (offline installer copy) |
| `build-iso.sh <repo> <out>` | bake + temp profile pointed at `<repo>` + `mkarchiso` + checksum/signature |
| `publish-pages.sh <repo> <clone>` | push the repo into an OG-os-repo clone |
| `import-signing-key.sh` | CI only: load the signing key from secrets into a throwaway keyring |

`distro/pkgbuilds/publish.sh` is the all-in-one local entry point
(build + bake + publish).

## Self-hosted runner

Needs an **Arch Linux** machine or VM (makepkg/mkarchiso are Arch tools)
with **KVM** (the VM install test runs qemu; nested virtualization if the
runner is itself a VM) and ~40 GB free disk.

1. Packages: `base-devel git rust archiso qemu-desktop edk2-ovmf sshpass python github-cli rsync`.
2. A dedicated non-root user (makepkg refuses root) in the `kvm` group,
   with passwordless sudo for the ISO steps, e.g. `/etc/sudoers.d/ogos-runner`:
   `runner ALL=(root) NOPASSWD: /usr/bin/pacman, /usr/bin/mkarchiso, /usr/bin/rm, /usr/bin/chown`
   (`pacman` because `build-repo.sh` runs `makepkg --syncdeps` to install
   each package's dependencies).
   That is effectively root (mkarchiso needs it anyway), so the real
   isolation boundary is the machine: use a **dedicated VM** for the
   runner, not a box with anything else on it.
3. Register it: repo Settings → Actions → Runners → New self-hosted
   runner, with the extra label **`ogos-builder`**. Install it as a
   service (`./svc.sh install`).

Self-hosted runners on a public repo: only trusted workflows touch it.
Both heavy workflows are `workflow_dispatch` only (maintainers only), and
`ci.yml` stays on GitHub-hosted runners, so PRs from forks never execute
on this machine. Keep it that way.

## Secrets

| Secret | Used by | What |
|---|---|---|
| `OGOS_GPG_PRIVATE_KEY` | packages, iso | ASCII-armored secret key, **no passphrase**, used only for signing packages/repo/ISO. Unset = unsigned build (with a warning) |
| `OGOS_REPO_TOKEN` | packages (publish) | fine-grained PAT with Contents: read/write on `OmegaGiven/OG-os-repo` only |

### Creating the signing key (one time)

```sh
export GNUPGHOME="$(mktemp -d)"
gpg --batch --passphrase '' --quick-gen-key "OG-OS Package Signing <ci@og-os.invalid>" ed25519 sign 3y
gpg --armor --export-secret-keys > ogos-signing.sec.asc   # -> OGOS_GPG_PRIVATE_KEY, then delete this file
gpg --armor --export > ogos-signing.pub.asc               # public: commit to the repo (keyring package)
gpg --gen-revoke --output ogos-signing.rev "$(gpg --with-colons -k | awk -F: '$1=="fpr"{print $10; exit}')"   # store offline
```

## Roadmap

- [x] Hosted CI: build, test, clippy, PKGBUILD + shellcheck
- [x] Reproducible packaging: PKGBUILDs build from the commit (`$OGOS_SRC`), not a dev machine path
- [x] Manual signed repo build/publish, manual ISO build + VM test + pre-release
- [ ] Register the self-hosted runner and add the secrets
- [ ] `ogos-keyring` package (public key + `pacman-key --populate` hook); then flip `[ogos]` `SigLevel` from `Optional TrustAll` to `Required` in `pacman.conf` and the installer's config
- [ ] Enforce clippy (`-D warnings`) and rustfmt once the existing code is cleaned up
- [ ] Weekly scheduled ISO build (not released) to catch Arch rolling-release breakage early

## Release blockers (not CI, but must be fixed before a public ISO)

- **Live ISO remote root.** The live session has `liveuser`/`ogos` with
  NOPASSWD sudo, and `sshd` is enabled with `PasswordAuthentication yes`
  (archiso releng default). Anyone on the same network as a booted live
  ISO can log in as root. The VM tests rely on that SSH access, so the
  fix needs both sides: sshd off by default in the ISO, and the test
  harness enabling it only in the test VM (e.g. kernel cmdline
  `systemd.wants=sshd.service` via direct kernel boot, or a test-only
  profile).
- **HRIR audio assets** (`apps/og-settings/src/assets/hrir/`): HeSuVi's own
  code is MIT (license included), but several impulse responses are
  captures of commercial virtualizers (Dolby Atmos, DTS Headphone:X,
  Windows Sonic, Creative, Waves). Confirm redistribution rights or
  replace them with openly licensed HRIRs before publishing an ISO.
- `apps/og-settings/runtime/spatial` hardcodes
  `$HOME/.local/src/sway-control/...` (a dev-machine path) for the HRIR
  directory; it won't find them on an installed system.
