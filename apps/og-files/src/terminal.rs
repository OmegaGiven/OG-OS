//! "Open terminal here" and the terminal used for Terminal=true apps.
//!
//! Which terminal: `terminal` in `$XDG_CONFIG_HOME/og-files/settings.json`,
//! else `$TERMINAL`, else `x-terminal-emulator`, else the first installed
//! of [`KNOWN`]. Each known terminal gets its own working-directory flag;
//! the process's current dir is set too, which covers the rest.
//!
//! macOS: `terminal` may name a command on $PATH (handled as on Linux) or an
//! app (`open -a <App> <dir>`, e.g. "iTerm"); default Terminal.app.
//! Windows: `terminal` may be "wt", "powershell", "pwsh" or "cmd"; default
//! Windows Terminal when installed, else cmd.exe — always started *in* the
//! folder (process working dir), so no path ever goes through a shell
//! command line.

#[cfg(not(windows))]
use crate::openwith::spawn_detached;
use crate::platform::find_in;
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
    let path: Vec<PathBuf> = crate::platform::search_path();
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
#[cfg_attr(windows, allow(dead_code))]
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
#[cfg(not(any(target_os = "macos", windows)))]
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

/// macOS: what to run for "Open terminal here". A `setting` naming a
/// command on $PATH (`on_path`) runs like on Linux; any other setting is
/// an app name for `open -a`; no setting = Terminal.app.
#[cfg(any(target_os = "macos", test))]
pub fn mac_here_args(
    setting: Option<&str>,
    on_path: &dyn Fn(&str) -> bool,
    dir: &Path,
) -> (String, Vec<String>) {
    let d = dir.to_string_lossy().into_owned();
    if let Some(s) = setting.map(str::trim).filter(|s| !s.is_empty()) {
        let words: Vec<String> = s.split_whitespace().map(str::to_string).collect();
        if on_path(&words[0]) {
            return (base(&words).to_string(), here_args(&words, dir));
        }
        let app = s.trim_end_matches(".app").to_string();
        return (app.clone(), vec!["open".into(), "-a".into(), app, d]);
    }
    (
        "Terminal".into(),
        vec!["open".into(), "-a".into(), "Terminal".into(), d],
    )
}

#[cfg(target_os = "macos")]
pub fn open_here(dir: &Path) -> Result<(String, u32), String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    let setting = crate::config::Settings::load().terminal;
    let path = crate::platform::search_path();
    let (name, argv) = mac_here_args(setting.as_deref(), &|p| find_in(p, &path).is_some(), dir);
    let pid = spawn_detached(&argv, Some(dir))?;
    Ok((name, pid))
}

/// Windows: (display name, argv, needs its own console). The folder is
/// passed as the process working directory, never on the command line.
#[cfg(any(windows, test))]
pub fn windows_here_args(setting: Option<&str>, has_wt: bool) -> (String, Vec<String>, bool) {
    let pick = setting
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| if has_wt { "wt".into() } else { "cmd".into() });
    let pick = pick.trim_end_matches(".exe").to_string();
    match pick.as_str() {
        // "-d ." = the working directory we start it in
        "wt" if has_wt => (
            "Windows Terminal".into(),
            vec!["wt.exe".into(), "-d".into(), ".".into()],
            false,
        ),
        "powershell" => (
            "PowerShell".into(),
            vec!["powershell.exe".into(), "-NoExit".into()],
            true,
        ),
        "pwsh" => (
            "PowerShell".into(),
            vec!["pwsh.exe".into(), "-NoExit".into()],
            true,
        ),
        _ => (
            "Command Prompt".into(),
            vec!["cmd.exe".into(), "/K".into()],
            true,
        ),
    }
}

#[cfg(windows)]
pub fn open_here(dir: &Path) -> Result<(String, u32), String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    if !dir.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    let setting = crate::config::Settings::load().terminal;
    let has_wt = crate::platform::find_program("wt").is_some();
    let (name, argv, console) = windows_here_args(setting.as_deref(), has_wt);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(
            CREATE_NEW_PROCESS_GROUP
                | if console {
                    CREATE_NEW_CONSOLE
                } else {
                    CREATE_NO_WINDOW
                },
        );
    if console {
        // the new console needs real handles, not NUL
        cmd.stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
    }
    let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", argv[0]))?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok((name, pid))
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
    fn mac_terminal_choice() {
        let d = Path::new("/Users/me/a b");
        let none = |_: &str| false;
        assert_eq!(
            mac_here_args(None, &none, d),
            (
                "Terminal".into(),
                s(&["open", "-a", "Terminal", "/Users/me/a b"])
            )
        );
        assert_eq!(
            mac_here_args(Some("iTerm.app"), &none, d),
            ("iTerm".into(), s(&["open", "-a", "iTerm", "/Users/me/a b"]))
        );
        assert_eq!(
            mac_here_args(Some(" "), &none, d).0,
            "Terminal",
            "blank setting = default"
        );
        let kitty = |p: &str| p == "kitty";
        assert_eq!(
            mac_here_args(Some("kitty"), &kitty, d),
            (
                "kitty".into(),
                s(&["kitty", "--directory", "/Users/me/a b"])
            )
        );
    }

    #[test]
    fn windows_terminal_choice() {
        assert_eq!(
            windows_here_args(None, true),
            ("Windows Terminal".into(), s(&["wt.exe", "-d", "."]), false)
        );
        assert_eq!(
            windows_here_args(None, false),
            ("Command Prompt".into(), s(&["cmd.exe", "/K"]), true)
        );
        // wt asked for but missing: cmd
        assert_eq!(
            windows_here_args(Some("wt"), false).1,
            s(&["cmd.exe", "/K"])
        );
        assert_eq!(
            windows_here_args(Some("PowerShell.exe"), true),
            ("PowerShell".into(), s(&["powershell.exe", "-NoExit"]), true)
        );
        assert_eq!(
            windows_here_args(Some("pwsh"), true).1,
            s(&["pwsh.exe", "-NoExit"])
        );
        // no argument ever carries the folder
        for set in [None, Some("cmd"), Some("wt"), Some("powershell")] {
            let (_, argv, _) = windows_here_args(set, true);
            assert!(argv.iter().all(|a| !a.contains('\\') && !a.contains(':')));
        }
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
