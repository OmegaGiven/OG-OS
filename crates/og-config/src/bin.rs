//! Where to find OG-toolkit executables (`og-bar`, `og-settings`,
//! `og-power-apply`, ...) when one app needs to start another.
//!
//! Two install layouts have to work at once:
//!
//! - **Packaged** (OG-OS, `[ogos]` repo): everything lives in `/usr/bin`.
//! - **Dev** (`apps/*/scripts/install.sh`): fresh builds are copied into
//!   `~/.local/bin`, and those must win over any packaged copy.
//!
//! Bare names aren't enough: sway's exec environment (and systemd user
//! units) usually don't have `~/.local/bin` on `PATH`, so a dev build would
//! silently lose to `/usr/bin` or not be found at all. Hardcoding
//! `~/.local/bin` is worse: on a fresh install nothing is there, so the
//! click/hotkey does nothing. Every spawn site goes through [`og_bin`]
//! instead.

use std::path::{Path, PathBuf};

/// Directory dev installs copy binaries into (`~/.local/bin`).
pub fn dev_bin_dir() -> PathBuf {
    crate::dirs_home().join(".local/bin")
}

/// Directory packages install binaries into.
pub const SYSTEM_BIN_DIR: &str = "/usr/bin";

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Resolve `name` with explicit directories — the testable core of
/// [`og_bin`]. Order: `dev_dir/name` (if executable), then
/// `system_dir/name` (if executable), else the bare name so the caller's
/// `PATH` still gets a chance.
pub fn resolve_in(name: &str, dev_dir: &Path, system_dir: &Path) -> PathBuf {
    if name.contains('/') {
        return PathBuf::from(name);
    }
    let dev = dev_dir.join(name);
    if is_executable(&dev) {
        return dev;
    }
    let sys = system_dir.join(name);
    if is_executable(&sys) {
        return sys;
    }
    PathBuf::from(name)
}

/// Path to spawn for toolkit executable `name`: `~/.local/bin/<name>` when
/// a dev build is installed there, else `/usr/bin/<name>`, else the bare
/// name (resolved through `PATH`). Names containing `/` are returned as is.
pub fn og_bin(name: &str) -> PathBuf {
    resolve_in(name, &dev_bin_dir(), Path::new(SYSTEM_BIN_DIR))
}

/// `std::process::Command` for toolkit executable `name` (see [`og_bin`]).
pub fn og_command(name: &str) -> std::process::Command {
    std::process::Command::new(og_bin(name))
}

/// Form of `name` to write into a sway config or a `sh -c` string.
///
/// A dev build in `~/.local/bin` is written as `~/.local/bin/<name>` (sway
/// runs exec lines through `sh -c`, which expands `~`, and the line stays
/// readable and portable across usernames); anything else is the bare name,
/// which sway finds in `/usr/bin` through its own `PATH`. Resolved at write
/// time, so a config written on a packaged system never mentions
/// `~/.local/bin` at all.
pub fn og_bin_shell(name: &str) -> String {
    shell_form(name, &dev_bin_dir())
}

/// Testable core of [`og_bin_shell`].
pub fn shell_form(name: &str, dev_dir: &Path) -> String {
    if !name.contains('/') && is_executable(&dev_dir.join(name)) {
        format!("~/.local/bin/{name}")
    } else {
        name.to_string()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("og-bin-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch_exec(dir: &Path, name: &str, mode: u32) {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn dev_build_wins_over_system() {
        let (dev, sys) = (tmpdir("dev1"), tmpdir("sys1"));
        touch_exec(&dev, "og-x", 0o755);
        touch_exec(&sys, "og-x", 0o755);
        assert_eq!(resolve_in("og-x", &dev, &sys), dev.join("og-x"));
        assert_eq!(shell_form("og-x", &dev), "~/.local/bin/og-x");
    }

    #[test]
    fn falls_back_to_system_then_bare() {
        let (dev, sys) = (tmpdir("dev2"), tmpdir("sys2"));
        touch_exec(&sys, "og-y", 0o755);
        assert_eq!(resolve_in("og-y", &dev, &sys), sys.join("og-y"));
        assert_eq!(resolve_in("og-z", &dev, &sys), PathBuf::from("og-z"));
        assert_eq!(shell_form("og-y", &dev), "og-y");
    }

    #[test]
    fn non_executable_dev_copy_is_ignored() {
        let (dev, sys) = (tmpdir("dev3"), tmpdir("sys3"));
        touch_exec(&dev, "og-w", 0o644);
        touch_exec(&sys, "og-w", 0o755);
        assert_eq!(resolve_in("og-w", &dev, &sys), sys.join("og-w"));
        assert_eq!(shell_form("og-w", &dev), "og-w");
    }

    #[test]
    fn paths_pass_through() {
        let (dev, sys) = (tmpdir("dev4"), tmpdir("sys4"));
        assert_eq!(resolve_in("/opt/x", &dev, &sys), PathBuf::from("/opt/x"));
    }
}
