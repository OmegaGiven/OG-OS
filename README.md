# OG-OS

An Arch-based Linux distribution built around a lightweight Sway desktop
and its own suite of native (Rust + iced) desktop apps: a status bar,
settings center, file manager, launcher, clipboard and notification
center, app store and more.

Everything that makes up the distro lives in this one repository: the
apps, their shared libraries, the pacman packaging, the ISO profile and
the test harness. A package and the ISO it ships in always come from the
same commit.

> **Status:** pre-release. The ISO builds and installs in a VM, but there
> is no published release yet.

## Layout

| Path | What |
|---|---|
| `apps/` | Desktop apps (`og-bar`, `og-settings`, `og-files`, `og-search`, `og-clip`, `og-notif-center`, `og-notify`, `og-apps`, `og-links`, `og-hotkeys`, `og-voice`, `og-wallpaper`, `og-wallpaper-studio`, `og-note`) |
| `crates/` | Shared libraries (`og-config`, `og-theme`, `og-drag`, `og-wayland`, `og-wallpaper-core`) |
| `vendor/` | Patched copies of upstream crates, wired in via `[patch.crates-io]` — see `vendor/README.md` for every patch and why |
| `distro/` | The OS: archiso profile (`profiledef.sh`, `airootfs/`, `packages.x86_64`), PKGBUILDs for the `[ogos]` pacman repo, installer, VM tests |
| `distro/ci/` | Build/publish scripts shared by local builds and CI |
| `.github/workflows/` | CI — see `distro/CI.md` |

## Building

Apps (any Linux with Rust, Wayland and libxkbcommon):

```sh
cargo build --release              # whole workspace
cargo build --release -p og-bar    # one app
cargo test --workspace
```

Packages and the `[ogos]` repo (Arch, `base-devel`):

```sh
distro/ci/build-repo.sh distro/local-repo
```

ISO (Arch, `archiso`, root):

```sh
distro/ci/bake-repo.sh distro/local-repo
sudo mkarchiso -v -w distro/work -o distro/out distro
```

See `distro/README.md` and `distro/OS-PLAN.md` for the full picture, and
`distro/testing/README.md` for the VM install tests.

## License

MIT — see `LICENSE`. Some bundled third-party assets carry their own
license files next to them.
