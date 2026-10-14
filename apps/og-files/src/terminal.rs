//! "Open terminal here" and the terminal used for Terminal=true apps.
//!
//! Which terminal: `terminal` in `$XDG_CONFIG_HOME/og-files/settings.json`,
//! else `$TERMINAL`, else `x-terminal-emulator`, else the first installed
//! of [`KNOWN`]. Each known terminal gets its own working-directory flag;
//! the process's current dir is set too, which covers the rest.

use crate::openwith::{find_in, spawn_detached};
use std::path::{Path, PathBuf};

pub const KNOWN: [&str; 8] = [
    "alacritty",
    "foot",
    "kitty",
    "wezterm",
    "gnome-terminal",
    "konsole",
    "xfce4-terminal",
    "xterm",
];

/// The terminal command (program + any configured args), first match of:
/// `setting`, `env_terminal`, x-terminal-emulator, [`KNOWN`].
pub fn resolve(
    setting: Option<&str>,
    env_terminal: Option<&str>,
    exists: &dyn Fn(&str) -> bool,
) -> Option<Vec<String>> {
    let words = |s: &str| -> Option<Vec<String>> {
        let v: Vec<String> = s.split_whitespace().map(str::to_string).collect();
        v.first().filter(|p| exists(p)).map(|_| v.clone())
    };
    setting
        .and_then(words)
        .or_else(|| env_terminal.and_then(words))
        .or_else(|| words("x-terminal-emulator"))
        .or_else(|| KNOWN.iter().find_map(|t| words(t)))
}

/// [`resolve`] with the real settings, $TERMINAL and $PATH.
pub fn resolve_env() -> Option<Vec<String>> {
    let path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let setting = crate::config::Settings::load().terminal;
    let env_t = std::env::var("TERMINAL").ok();
    resolve(setting.as_deref(), env_t.as_deref(), &|p| {
        find_in(p, &path).is_some()
    })
}

fn base(term: &[String]) -> &str {
    term.first()
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .unwrap_or("")
}

/// Full command line that opens `term` in `dir`.
pub fn here_args(term: &[String], dir: &Path) -> Vec<String> {
    let d = dir.to_string_lossy().into_owned();
    let mut v = term.to_vec();
    let extra: Vec<String> = match base(term) {
        "alacritty" | "xfce4-terminal" | "gnome-terminal" => {
            vec!["--working-directory".into(), d]
        }
        "foot" | "footclient" => vec!["-D".into(), d],
        "kitty" => vec!["--directory".into(), d],
        "wezterm" => vec!["start".into(), "--cwd".into(), d],
        "konsole" => vec!["--workdir".into(), d],
        // xterm, x-terminal-emulator, others: the current dir does it
        _ => Vec::new(),
    };
    v.extend(extra);
    v
}

/// Prefix that makes `term` run a command (`alacritty -e cmd args…`).
pub fn exec_prefix(term: &[String]) -> Vec<String> {
    let mut v = term.to_vec();
    match base(term) {
        "foot" | "footclient" | "kitty" => {}
        "wezterm" => v.extend(["start".into(), "--".into()]),
        "gnome-terminal" => v.push("--".into()),
        "xfce4-terminal" => v.push("-x".into()),
        _ => v.push("-e".into()),
    }
    v
}

/// Open a terminal in `dir`, detached. Ok = (terminal name, pid).
pub fn open_here(dir: &Path) -> Result<(String, u32), String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    let term = resolve_env().ok_or(
        "No terminal found: set \"terminal\" in ~/.config/og-files/settings.json or $TERMINAL",
    )?;
    let argv = here_args(&term, dir);
    let pid = spawn_detached(&argv, Some(dir))?;
    Ok((base(&term).to_string(), pid))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn resolution_order() {
        let have = |set: &'static [&'static str]| move |p: &str| set.contains(&p);
        let all = have(&["mine", "kitty", "x-terminal-emulator", "foot", "xterm"]);
        assert_eq!(
            resolve(Some("mine --flag"), Some("kitty"), &all),
            Some(s(&["mine", "--flag"]))
        );
        assert_eq!(
            resolve(Some("absent"), Some("kitty"), &all),
            Some(s(&["kitty"]))
        );
        assert_eq!(resolve(None, None, &all), Some(s(&["x-terminal-emulator"])));
        assert_eq!(
            resolve(None, Some(""), &have(&["xterm", "foot"])),
            Some(s(&["foot"]))
        );
        assert_eq!(resolve(None, None, &have(&[])), None);
    }

    /// Manual check (opens a real terminal window): prints its pid; the
    /// caller closes it. `cargo test real_terminal -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_terminal_opens_here() {
        let d = std::env::temp_dir();
        let (name, pid) = open_here(&d).unwrap();
        println!("TERMINAL {name} PID {pid}");
        assert!(open_here(Path::new("/no/such/dir")).is_err());
    }

    #[test]
    fn working_dir_flags() {
        let d = Path::new("/tmp/a b");
        let cases: [(&str, &[&str]); 9] = [
            (
                "alacritty",
                &["alacritty", "--working-directory", "/tmp/a b"],
            ),
            ("foot", &["foot", "-D", "/tmp/a b"]),
            ("kitty", &["kitty", "--directory", "/tmp/a b"]),
            ("wezterm", &["wezterm", "start", "--cwd", "/tmp/a b"]),
            (
                "gnome-terminal",
                &["gnome-terminal", "--working-directory", "/tmp/a b"],
            ),
            ("konsole", &["konsole", "--workdir", "/tmp/a b"]),
            (
                "xfce4-terminal",
                &["xfce4-terminal", "--working-directory", "/tmp/a b"],
            ),
            ("xterm", &["xterm"]),
            ("/usr/bin/foot", &["/usr/bin/foot", "-D", "/tmp/a b"]),
        ];
        for (t, want) in cases {
            assert_eq!(here_args(&s(&[t]), d), s(want), "{t}");
        }
        assert_eq!(exec_prefix(&s(&["alacritty"])), s(&["alacritty", "-e"]));
        assert_eq!(exec_prefix(&s(&["foot"])), s(&["foot"]));
        assert_eq!(
            exec_prefix(&s(&["wezterm"])),
            s(&["wezterm", "start", "--"])
        );
        assert_eq!(
            exec_prefix(&s(&["xfce4-terminal"])),
            s(&["xfce4-terminal", "-x"])
        );
        assert_eq!(
            exec_prefix(&s(&["gnome-terminal"])),
            s(&["gnome-terminal", "--"])
        );
    }
}
