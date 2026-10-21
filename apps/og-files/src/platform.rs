//! Small OS differences in one place: finding programs, starting detached
//! or console-less child processes, and (Windows) the console of a GUI-
//! subsystem binary. Everything else that differs per OS lives next to the
//! feature it belongs to, behind `cfg(target_os = …)`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The Mac App Store build (`--features appstore`). Leaves out what the App
/// Sandbox can't do: network mounts (rclone/FUSE, ssh config, tailnet),
/// terminals and ejecting disks.
pub const STORE: bool = cfg!(feature = "appstore");

/// Running inside the Mac App Store sandbox, where folders must be granted
/// through the open panel before they can be read.
pub fn sandboxed() -> bool {
    #[cfg(target_os = "macos")]
    return crate::macos::sandboxed();
    #[cfg(not(target_os = "macos"))]
    false
}

/// Is `p` a file this OS would run? Unix: any execute bit. Windows: a file
/// whose extension is in `%PATHEXT%` (.exe, .bat, …).
pub fn is_executable(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else {
        return false;
    };
    if !m.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exe_exts().iter().any(|x| x.eq_ignore_ascii_case(e)))
    }
}

/// Executable extensions (without the dot), from `%PATHEXT%`.
#[cfg(not(unix))]
fn exe_exts() -> Vec<String> {
    let v = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    v.split(';')
        .map(|s| s.trim().trim_start_matches('.').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Names to try for `prog` in one folder: as is, plus (Windows, when it has
/// no extension) each `%PATHEXT%` extension.
fn variants(prog: &str) -> Vec<String> {
    #[cfg(not(unix))]
    {
        if Path::new(prog).extension().is_none() {
            return exe_exts()
                .iter()
                .map(|e| format!("{prog}.{}", e.to_ascii_lowercase()))
                .chain([prog.to_string()])
                .collect();
        }
    }
    vec![prog.to_string()]
}

/// Look `prog` up in `path` (absolute or containing a separator = checked
/// as is).
pub fn find_in(prog: &str, path: &[PathBuf]) -> Option<PathBuf> {
    if prog.is_empty() {
        return None;
    }
    let has_sep = prog.contains('/') || (cfg!(windows) && prog.contains('\\'));
    if has_sep {
        return variants(prog)
            .into_iter()
            .map(PathBuf::from)
            .find(|p| is_executable(p));
    }
    path.iter()
        .flat_map(|d| variants(prog).into_iter().map(move |v| d.join(v)))
        .find(|p| is_executable(p))
}

/// `$PATH` as a list.
pub fn search_path() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

/// `prog` on `$PATH`?
pub fn find_program(prog: &str) -> Option<PathBuf> {
    find_in(prog, &search_path())
}

/// Make `link` a symbolic link to `target`. Windows needs to know whether
/// the target is a folder (resolved relative to the link's folder) and the
/// right to create links (Developer Mode or admin), else this fails.
pub fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        let resolved = link.parent().unwrap_or(Path::new(".")).join(target);
        if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
    }
}

/// Hidden in listings: dot files everywhere, plus the "hidden" attribute
/// on Windows (`meta` = the entry's own, not-followed metadata).
pub fn is_hidden(name: &str, meta: Option<&std::fs::Metadata>) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        if meta.is_some_and(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0) {
            return true;
        }
    }
    #[cfg(not(windows))]
    let _ = meta;
    false
}

/// Windows: no console window for a helper process (rclone, tailscale…)
/// started from the GUI. No-op elsewhere.
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Make `cmd` outlive og-files: its own session on unix (setsid), detached
/// from our console and process group on Windows.
pub fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; nothing else runs between
        // fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
}

/// Windows release builds use the GUI subsystem (no console window), so a
/// CLI subcommand started from cmd / PowerShell would print nothing. Attach
/// to the parent's console when there is one. No-op elsewhere.
pub fn attach_parent_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        // SAFETY: plain Win32 call; fails harmlessly (no parent console, or
        // already attached in a console-subsystem debug build)
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_find_in_path() {
        let d = crate::config::home()
            .join(".cache/ogx-platform-tests")
            .join(format!("find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let name = if cfg!(windows) { "tool.exe" } else { "tool" };
        let p = d.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                find_in("tool", std::slice::from_ref(&d)),
                None,
                "not executable"
            );
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(find_in("tool", std::slice::from_ref(&d)), Some(p.clone()));
        assert!(find_in(&p.to_string_lossy(), &[]).is_some());
        assert_eq!(find_in("", std::slice::from_ref(&d)), None);
        assert_eq!(find_in("other", std::slice::from_ref(&d)), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
