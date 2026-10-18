//! Mounting through rclone + FUSE, entirely user-level. No rclone.conf is
//! written: the remote `OGF:` is defined on the fly through
//! `RCLONE_CONFIG_OGF_*` environment variables of the child process, and
//! passwords only ever reach rclone obscured, via env — the cleartext goes
//! to `rclone obscure -` on stdin, never on a command line.

use super::keyring::Keyring;
use super::sshcfg::{self, SshHost};
use super::url::{effective_port, encode};
use super::{Auth, Conn, Paths, Proto};
use std::path::{Path, PathBuf};
use std::time::Duration;

const REMOTE: &str = "OGF";
const ENV_PREFIX: &str = "RCLONE_CONFIG_OGF_";
pub(crate) const NEED_PASSWORD: &str = "Password required";

// ---------------------------------------------------------------- processes

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    /// added to the inherited environment
    pub env: Vec<(String, String)>,
    pub stdin: Option<String>,
    /// stdout/stderr to /dev/null (daemonizing children keep pipes open)
    pub detach: bool,
    /// start it and return at once (Ok, stdout = pid): Windows mounts,
    /// where rclone can't daemonize itself
    pub background: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs commands; faked in tests to capture them.
pub(crate) trait Runner {
    async fn run(&self, cmd: Cmd, timeout: Duration) -> Result<Out, String>;
}

pub(crate) struct System;

impl Runner for System {
    async fn run(&self, cmd: Cmd, timeout: Duration) -> Result<Out, String> {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;
        let mut c = std::process::Command::new(&cmd.program);
        c.args(&cmd.args);
        if cmd.background {
            crate::platform::detach(&mut c);
        }
        // no console window flashing up on Windows
        crate::platform::no_window(&mut c);
        let mut c = tokio::process::Command::from(c);
        c.kill_on_drop(!cmd.background);
        // never let a stray inherited definition leak into our remote
        for (k, _) in std::env::vars_os() {
            if k.to_str().is_some_and(|k| k.starts_with(ENV_PREFIX)) {
                c.env_remove(k);
            }
        }
        c.envs(cmd.env.iter().map(|(k, v)| (k, v)));
        c.stdin(if cmd.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        if cmd.detach || cmd.background {
            c.stdout(Stdio::null()).stderr(Stdio::null());
        } else {
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        let mut child = c.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("{} is not installed", cmd.program),
            _ => format!("Couldn't start {}: {e}", cmd.program),
        })?;
        if let (Some(input), Some(mut stdin)) = (cmd.stdin, child.stdin.take()) {
            stdin
                .write_all(input.as_bytes())
                .await
                .map_err(|e| format!("{}: {e}", cmd.program))?;
            drop(stdin);
        }
        if cmd.background {
            let pid = child.id().unwrap_or(0);
            // reap it whenever it ends (unmount kills it)
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Ok(Out {
                ok: true,
                stdout: pid.to_string(),
                stderr: String::new(),
            });
        }
        match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Err(_) => Err("timeout".into()),
            Ok(Err(e)) => Err(format!("{}: {e}", cmd.program)),
            Ok(Ok(o)) => Ok(Out {
                ok: o.status.success(),
                stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            }),
        }
    }
}

/// Executable `name` somewhere on $PATH (`name.exe` on Windows)?
pub(crate) fn on_path(name: &str) -> bool {
    crate::platform::find_program(name).is_some()
}

// ------------------------------------------------------------ remote (env)

/// Everything that turns a `Conn` into an rclone invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Remote {
    pub env: Vec<(String, String)>,
    /// `OGF:<path>`
    pub spec: String,
    /// backend flags appended to every command
    pub flags: Vec<String>,
}

/// Machine facts the remote depends on (injected so tests are stable).
#[derive(Debug, Clone, Default)]
pub(crate) struct Facts {
    pub local_user: String,
    /// an ssh-agent socket is available
    pub agent: bool,
    /// first existing ~/.ssh/id_* key
    pub default_key: Option<PathBuf>,
    pub home: PathBuf,
}

impl Facts {
    fn real(ssh_dir: &Path) -> Self {
        Facts {
            local_user: super::local_user(),
            agent: std::env::var_os("SSH_AUTH_SOCK").is_some_and(|s| Path::new(&s).exists()),
            default_key: ["id_ed25519", "id_ecdsa", "id_rsa"]
                .iter()
                .map(|k| ssh_dir.join(k))
                .find(|p| p.is_file()),
            home: crate::config::home(),
        }
    }
}

fn key(name: &str) -> String {
    format!("{ENV_PREFIX}{name}")
}

fn expand_tilde(p: &Path, home: &Path) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix("~/")) {
        Some(rest) => home.join(rest),
        None => p.to_path_buf(),
    }
}

/// Build env + remote spec. `obscured` = password already passed through
/// `rclone obscure` (key passphrase for `Auth::KeyFile`).
pub(crate) fn remote(
    conn: &Conn,
    ssh: Option<&SshHost>,
    obscured: Option<&str>,
    facts: &Facts,
) -> Result<Remote, String> {
    let mut env: Vec<(String, String)> = Vec::new();
    let mut set = |k: &str, v: String| env.push((key(k), v));
    let mut flags = Vec::new();
    let spec;
    match conn.proto {
        Proto::Sftp => {
            set("TYPE", "sftp".into());
            let host = ssh
                .and_then(|s| s.hostname.clone())
                .unwrap_or_else(|| conn.host.clone());
            let user = match ssh.and_then(|s| s.user.clone()) {
                Some(u) if conn.user.is_empty() || conn.user == facts.local_user => u,
                _ if conn.user.is_empty() => facts.local_user.clone(),
                _ => conn.user.clone(),
            };
            let port = conn.port.or(ssh.and_then(|s| s.port)).unwrap_or(22);
            set("HOST", host);
            set("USER", user);
            set("PORT", port.to_string());
            match &conn.auth {
                Auth::Agent => {
                    let ident = ssh.and_then(|s| s.identity.clone()).filter(|p| p.is_file());
                    if let Some(k) = &ident {
                        set("KEY_FILE", k.display().to_string());
                    }
                    if facts.agent {
                        set("KEY_USE_AGENT", "true".into());
                    } else if ident.is_none() {
                        match &facts.default_key {
                            Some(k) => set("KEY_FILE", k.display().to_string()),
                            // nothing else to try: rclone reports the agent error
                            None => set("KEY_USE_AGENT", "true".into()),
                        }
                    }
                }
                Auth::KeyFile(p) => {
                    set(
                        "KEY_FILE",
                        expand_tilde(p, &facts.home).display().to_string(),
                    );
                    if let Some(o) = obscured {
                        set("KEY_FILE_PASS", o.into());
                    }
                }
                Auth::Password => set("PASS", obscured.ok_or(NEED_PASSWORD)?.into()),
                Auth::Anonymous => {
                    return Err("SSH needs a key or a password, not anonymous login".into())
                }
            }
            let path = conn.path.trim();
            let path = if path == "~" {
                ""
            } else {
                path.strip_prefix("~/").unwrap_or(path)
            };
            spec = format!("{REMOTE}:{path}");
            flags.push("--sftp-set-modtime=true".into());
        }
        Proto::Smb => {
            set("TYPE", "smb".into());
            set("HOST", conn.host.clone());
            if let Some(p) = conn.port {
                set("PORT", p.to_string());
            }
            match &conn.auth {
                Auth::Anonymous => {
                    set("USER", "guest".into());
                }
                _ => {
                    let (domain, user) = match conn.user.split_once(['\\', ';']) {
                        Some((d, u)) => (Some(d), u),
                        None => (None, conn.user.as_str()),
                    };
                    if let Some(d) = domain {
                        set("DOMAIN", d.into());
                    }
                    set(
                        "USER",
                        if user.is_empty() {
                            facts.local_user.clone()
                        } else {
                            user.into()
                        },
                    );
                    if conn.auth == Auth::Password {
                        set("PASS", obscured.ok_or(NEED_PASSWORD)?.into());
                    }
                }
            }
            spec = format!("{REMOTE}:{}", conn.path.trim_matches('/'));
        }
        Proto::WebDav => {
            set("TYPE", "webdav".into());
            let https = conn.tls || conn.port == Some(443);
            let host = if conn.host.contains(':') {
                format!("[{}]", conn.host)
            } else {
                conn.host.clone()
            };
            let default = if https { 443 } else { 80 };
            let port = effective_port(conn);
            let mut url = format!("{}://{host}", if https { "https" } else { "http" });
            if port != default {
                url.push_str(&format!(":{port}"));
            }
            let path = conn.path.trim_matches('/');
            if !path.is_empty() {
                url.push('/');
                url.push_str(&encode(path, b""));
            }
            set("URL", url);
            set("VENDOR", "other".into());
            if conn.auth != Auth::Anonymous && !conn.user.is_empty() {
                set("USER", conn.user.clone());
            }
            if conn.auth == Auth::Password {
                set("PASS", obscured.ok_or(NEED_PASSWORD)?.into());
            }
            spec = format!("{REMOTE}:");
        }
        Proto::Ftp => {
            set("TYPE", "ftp".into());
            set("HOST", conn.host.clone());
            set("PORT", effective_port(conn).to_string());
            let user = if conn.auth == Auth::Anonymous || conn.user.is_empty() {
                "anonymous".to_string()
            } else {
                conn.user.clone()
            };
            set("USER", user);
            set("PASS", obscured.ok_or(NEED_PASSWORD)?.into());
            set("EXPLICIT_TLS", conn.tls.to_string());
            spec = format!("{REMOTE}:{}", conn.path.trim_start_matches('/'));
        }
    }
    Ok(Remote { env, spec, flags })
}

/// The password the remote needs: given > keyring > Err(NEED_PASSWORD).
/// FTP anonymous logins use the conventional dummy.
async fn secret_for(
    kr: &impl Keyring,
    conn: &Conn,
    given: Option<String>,
) -> Result<Option<String>, String> {
    let given = given.filter(|p| !p.is_empty());
    match &conn.auth {
        Auth::Password => match given {
            Some(p) => Ok(Some(p)),
            None => match kr.lookup(&conn.id).await {
                Ok(Some(p)) => Ok(Some(p)),
                _ => Err(NEED_PASSWORD.into()),
            },
        },
        Auth::KeyFile(_) => Ok(given),
        Auth::Anonymous if conn.proto == Proto::Ftp => Ok(Some("anonymous@".into())),
        _ => Ok(None),
    }
}

async fn obscure(r: &impl Runner, password: &str) -> Result<String, String> {
    let out = r
        .run(
            Cmd {
                program: "rclone".into(),
                args: vec!["obscure".into(), "-".into()],
                stdin: Some(format!("{password}\n")),
                ..Default::default()
            },
            Duration::from_secs(10),
        )
        .await?;
    let o = out.stdout.trim();
    if !out.ok || o.is_empty() {
        return Err(format!("rclone obscure failed: {}", out.stderr.trim()));
    }
    Ok(o.to_string())
}

/// Env + spec ready to run, resolving ssh aliases and the password.
async fn prepare(
    paths: &Paths,
    r: &impl Runner,
    kr: &impl Keyring,
    conn: &Conn,
    password: Option<String>,
    facts: &Facts,
) -> Result<(Remote, Option<String>), String> {
    let secret = secret_for(kr, conn, password).await?;
    let obscured = match &secret {
        Some(p) => Some(obscure(r, p).await?),
        None => None,
    };
    let ssh = (conn.proto == Proto::Sftp)
        .then(|| sshcfg::Config::load(&paths.ssh_dir).resolve(&conn.host))
        .flatten();
    let rem = remote(conn, ssh.as_ref(), obscured.as_deref(), facts)?;
    Ok((rem, secret))
}

// ----------------------------------------------------------------- mounts

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MountEntry {
    pub source: String,
    pub target: PathBuf,
    pub fstype: String,
}

/// /proc/mounts lines; octal escapes (`\040` = space) decoded.
#[cfg(any(not(any(target_os = "macos", windows)), test))]
pub(crate) fn parse_mounts(text: &str) -> Vec<MountEntry> {
    fn unescape(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' && i + 4 <= b.len() {
                if let Ok(v) =
                    u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 4]).unwrap_or("x"), 8)
                {
                    out.push(v);
                    i += 4;
                    continue;
                }
            }
            out.push(b[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    text.lines()
        .filter_map(|l| {
            let mut f = l.split(' ');
            let source = unescape(f.next()?);
            let target = PathBuf::from(unescape(f.next()?));
            let fstype = unescape(f.next()?);
            Some(MountEntry {
                source,
                target,
                fstype,
            })
        })
        .collect()
}

/// `mount` output on macOS: `SRC on TARGET (TYPE, opt, …)`. The
/// filesystem type is reported as `fuse.rclone` for rclone's own mounts
/// (source `OGF:…` on macFUSE / FUSE-T) so the rest matches Linux.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn parse_mount_output(text: &str) -> Vec<MountEntry> {
    text.lines()
        .filter_map(|l| {
            let (source, rest) = l.split_once(" on ")?;
            let open = rest.rfind(" (")?;
            let target = PathBuf::from(&rest[..open]);
            let opts = rest[open + 2..].trim_end_matches(')');
            let mut fstype = opts.split(',').next().unwrap_or("").trim().to_string();
            if source.starts_with(&format!("{REMOTE}:")) {
                fstype = "fuse.rclone".into();
            }
            Some(MountEntry {
                source: source.to_string(),
                target,
                fstype,
            })
        })
        .collect()
}

#[cfg(not(any(target_os = "macos", windows)))]
fn read_mounts(paths: &Paths) -> Vec<MountEntry> {
    parse_mounts(&std::fs::read_to_string(&paths.proc_mounts).unwrap_or_default())
}

#[cfg(target_os = "macos")]
fn read_mounts(_paths: &Paths) -> Vec<MountEntry> {
    let out = std::process::Command::new("/sbin/mount")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) => parse_mount_output(&String::from_utf8_lossy(&o.stdout)),
        Err(_) => Vec::new(),
    }
}

/// Windows has no mount table we can read: rclone mounts started by us are
/// remembered in `rclone-<id>.pid` (pid, then mount folder); one counts
/// while its folder exists (WinFsp removes it when rclone exits).
#[cfg(windows)]
fn read_mounts(paths: &Paths) -> Vec<MountEntry> {
    let Ok(rd) = std::fs::read_dir(&paths.state_dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("rclone-") && n.ends_with(".pid")
        })
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            let (_pid, target) = parse_pid_file(&text)?;
            target.exists().then(|| MountEntry {
                source: format!("{REMOTE}:"),
                target,
                fstype: "fuse.rclone".into(),
            })
        })
        .collect()
}

/// `rclone-<id>.pid`: "<pid>\n<mount folder>\n".
#[cfg(any(windows, test))]
pub(crate) fn parse_pid_file(text: &str) -> Option<(u32, PathBuf)> {
    let mut l = text.lines();
    let pid = l.next()?.trim().parse().ok()?;
    let target = l.next().map(str::trim).filter(|t| !t.is_empty())?;
    Some((pid, PathBuf::from(target)))
}

#[cfg(windows)]
fn pid_file(paths: &Paths, c: &Conn) -> PathBuf {
    log_file(paths, c).with_extension("pid")
}

fn is_rclone_mount(paths: &Paths, at: &Path) -> bool {
    read_mounts(paths)
        .iter()
        .any(|m| m.fstype == "fuse.rclone" && m.target == at)
}

/// Mount folder name: the display name minus anything a path can't hold.
pub(crate) fn folder_name(c: &Conn) -> String {
    let mut s: String = c
        .name
        .chars()
        .map(|ch| {
            if ch == '/' || ch == '\\' || ch.is_control() {
                '-'
            } else {
                ch
            }
        })
        .collect();
    s = s.trim().trim_start_matches('.').trim().to_string();
    if s.chars().count() > 80 {
        s = s
            .chars()
            .take(80)
            .collect::<String>()
            .trim_end()
            .to_string();
    }
    if s.is_empty() {
        s = if c.id.is_empty() {
            "connection".into()
        } else {
            c.id.clone()
        };
        s = s.replace(['/', '\\'], "-");
        if s.starts_with('.') {
            s.insert(0, '_');
        }
    }
    s
}

pub(crate) fn mount_point(paths: &Paths, c: &Conn) -> PathBuf {
    paths.mount_root.join(folder_name(c))
}

pub(crate) fn log_file(paths: &Paths, c: &Conn) -> PathBuf {
    let id = if c.id.is_empty() {
        folder_name(c)
    } else {
        c.id.replace('/', "-")
    };
    paths.state_dir.join(format!("rclone-{id}.log"))
}

pub(crate) fn mount_args(rem: &Remote, mp: &Path, log: &Path, volname: &str) -> Vec<String> {
    // rclone can't daemonize on Windows: it is started in the background
    // instead (`Cmd::background`)
    let daemon: &[&str] = if cfg!(windows) {
        &[]
    } else {
        &["--daemon", "--daemon-wait", "10s"]
    };
    let mp_s = mp.display().to_string();
    let mut a: Vec<String> = ["mount", rem.spec.as_str(), mp_s.as_str()]
        .into_iter()
        .chain(daemon.iter().copied())
        .map(str::to_string)
        .collect();
    a.extend(
        [
            "--vfs-cache-mode",
            "writes",
            "--dir-cache-time",
            "30s",
            "--poll-interval",
            "30s",
            "--log-file",
            &log.display().to_string(),
            "--log-level",
            "NOTICE",
            "--volname",
            volname,
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    a.extend(rem.flags.iter().cloned());
    a
}

/// Is the folder there and empty (or absent)? Err explains a refusal.
fn check_mount_point(mp: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(mp) {
        Err(_) => Ok(()),
        Ok(m) if !m.is_dir() => Err(format!("{} exists and isn't a folder", mp.display())),
        Ok(_) => match std::fs::read_dir(mp) {
            Ok(mut rd) => {
                if rd.next().is_some() {
                    Err(format!(
                        "{} already exists and isn't empty — rename the connection",
                        mp.display()
                    ))
                } else {
                    Ok(())
                }
            }
            Err(e) => Err(format!("Can't use {}: {e}", mp.display())),
        },
    }
}

pub(crate) async fn mount(
    paths: &Paths,
    r: &impl Runner,
    kr: &impl Keyring,
    conn: Conn,
    password: Option<String>,
    remember: bool,
) -> Result<PathBuf, String> {
    let facts = Facts::real(&paths.ssh_dir);
    mount_with(
        paths,
        r,
        kr,
        &conn,
        password,
        remember,
        &facts,
        Duration::from_secs(10),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn mount_with(
    paths: &Paths,
    r: &impl Runner,
    kr: &impl Keyring,
    conn: &Conn,
    password: Option<String>,
    remember: bool,
    facts: &Facts,
    wait: Duration,
) -> Result<PathBuf, String> {
    let mp = mount_point(paths, conn);
    if is_rclone_mount(paths, &mp) {
        return Ok(mp);
    }
    if read_mounts(paths).iter().any(|m| m.target == mp) {
        return Err(format!("Something else is mounted at {}", mp.display()));
    }
    check_mount_point(&mp)?;
    let given = password.is_some();
    let (rem, secret) = prepare(paths, r, kr, conn, password, facts).await?;

    std::fs::create_dir_all(&mp).map_err(|e| format!("Can't create {}: {e}", mp.display()))?;
    if cfg!(windows) {
        // WinFsp creates the mount folder itself and refuses an existing one
        let _ = std::fs::remove_dir(&mp);
    }
    let _ = std::fs::create_dir_all(&paths.state_dir);
    let log = log_file(paths, conn);
    let _ = std::fs::remove_file(&log);
    let cmd = Cmd {
        program: "rclone".into(),
        args: mount_args(&rem, &mp, &log, &conn.name),
        env: rem.env.clone(),
        stdin: None,
        detach: true,
        background: cfg!(windows),
    };
    let fail = |msg: String| {
        let _ = std::fs::remove_dir(&mp); // only if still empty
        Err(msg)
    };
    match r.run(cmd, Duration::from_secs(20)).await {
        #[cfg(windows)]
        Ok(out) if out.ok => {
            let _ = std::fs::write(
                pid_file(paths, conn),
                format!("{}\n{}\n", out.stdout.trim(), mp.display()),
            );
        }
        Ok(out) if out.ok => {}
        Ok(_) => return fail(friendly_error(&log_tail(&log), conn)),
        Err(e) if e == "timeout" => {}
        Err(e) => return fail(e),
    }
    // wait for the kernel to show the mount
    let start = tokio::time::Instant::now();
    while !is_rclone_mount(paths, &mp) {
        if start.elapsed() >= wait {
            let tail = log_tail(&log);
            return fail(if tail.trim().is_empty() {
                format!("The mount didn't appear within {} s", wait.as_secs())
            } else {
                friendly_error(&tail, conn)
            });
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if remember && given {
        if let Some(p) = &secret {
            if let Err(e) = kr
                .store(&conn.id, &format!("og-files: {}", conn.name), p)
                .await
            {
                eprintln!("og-files: couldn't remember the password: {e}");
            }
        }
    }
    Ok(mp)
}

pub(crate) async fn test(
    paths: &Paths,
    r: &impl Runner,
    kr: &impl Keyring,
    conn: Conn,
    password: Option<String>,
) -> Result<String, String> {
    let facts = Facts::real(&paths.ssh_dir);
    test_with(paths, r, kr, &conn, password, &facts).await
}

pub(crate) async fn test_with(
    paths: &Paths,
    r: &impl Runner,
    kr: &impl Keyring,
    conn: &Conn,
    password: Option<String>,
    facts: &Facts,
) -> Result<String, String> {
    let (rem, _) = prepare(paths, r, kr, conn, password, facts).await?;
    let mut args: Vec<String> = [
        "lsf",
        "--max-depth",
        "1",
        "--contimeout",
        "10s",
        "--low-level-retries",
        "1",
        "--retries",
        "1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(rem.spec.clone());
    args.extend(rem.flags.iter().cloned());
    let cmd = Cmd {
        program: "rclone".into(),
        args,
        env: rem.env,
        stdin: None,
        detach: false,
        background: false,
    };
    match r.run(cmd, Duration::from_secs(15)).await {
        Ok(out) if out.ok => {
            let n = out.stdout.lines().filter(|l| !l.trim().is_empty()).count();
            Ok(if n == 1 {
                "1 item".into()
            } else {
                format!("{n} items")
            })
        }
        Ok(out) => Err(friendly_error(&out.stderr, conn)),
        Err(e) if e == "timeout" => Err(format!("Timed out connecting to {}", conn.host)),
        Err(e) => Err(e),
    }
}

/// macOS: `umount`, then a forced `diskutil unmount` when it's busy.
#[cfg(target_os = "macos")]
pub(crate) async fn unmount(paths: &Paths, r: &impl Runner, conn: &Conn) -> Result<(), String> {
    let mp = mount_point(paths, conn);
    if is_rclone_mount(paths, &mp) {
        let t = Duration::from_secs(10);
        let m = mp.display().to_string();
        let first = r
            .run(
                Cmd {
                    program: "/sbin/umount".into(),
                    args: vec![m.clone()],
                    ..Default::default()
                },
                t,
            )
            .await;
        if !matches!(first, Ok(Out { ok: true, .. })) {
            let o = r
                .run(
                    Cmd {
                        program: "/usr/sbin/diskutil".into(),
                        args: vec!["unmount".into(), "force".into(), m],
                        ..Default::default()
                    },
                    t,
                )
                .await;
            match o {
                Ok(o) if o.ok => {}
                Ok(o) => return Err(format!("Unmount failed: {}", o.stderr.trim())),
                Err(e) => return Err(format!("Unmount failed: {e}")),
            }
        }
    }
    let _ = std::fs::remove_dir(&mp); // only removes an empty folder
    Ok(())
}

/// Windows: end the rclone process we started (WinFsp then removes the
/// mount folder).
#[cfg(windows)]
pub(crate) async fn unmount(paths: &Paths, r: &impl Runner, conn: &Conn) -> Result<(), String> {
    let pf = pid_file(paths, conn);
    let entry = std::fs::read_to_string(&pf)
        .ok()
        .and_then(|t| parse_pid_file(&t));
    if let Some((pid, mp)) = entry {
        if mp.exists() {
            let o = r
                .run(
                    Cmd {
                        program: "taskkill".into(),
                        args: vec!["/PID".into(), pid.to_string(), "/T".into(), "/F".into()],
                        ..Default::default()
                    },
                    Duration::from_secs(10),
                )
                .await;
            if let Ok(o) = &o {
                if !o.ok && mp.exists() {
                    return Err(format!("Unmount failed: {}", o.stderr.trim()));
                }
            }
            let start = tokio::time::Instant::now();
            while mp.exists() && start.elapsed() < Duration::from_secs(5) {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
    let _ = std::fs::remove_file(&pf);
    Ok(())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) async fn unmount(paths: &Paths, r: &impl Runner, conn: &Conn) -> Result<(), String> {
    let mp = mount_point(paths, conn);
    if is_rclone_mount(paths, &mp) {
        let run = |args: &[&str]| Cmd {
            program: "fusermount3".into(),
            args: args
                .iter()
                .map(|s| s.to_string())
                .chain([mp.display().to_string()])
                .collect(),
            ..Default::default()
        };
        let t = Duration::from_secs(10);
        let first = r.run(run(&["-u"]), t).await;
        if !matches!(first, Ok(Out { ok: true, .. })) {
            match r.run(run(&["-uz"]), t).await {
                Ok(o) if o.ok => eprintln!(
                    "og-files: {} was busy; detached it (it disappears once nothing uses it)",
                    mp.display()
                ),
                Ok(o) => {
                    let why = first.map(|f| f.stderr).unwrap_or_default();
                    let why = if why.trim().is_empty() { o.stderr } else { why };
                    return Err(if why.contains("busy") {
                        format!(
                            "{} is in use — close files and try again",
                            folder_name(conn)
                        )
                    } else {
                        format!("Unmount failed: {}", why.trim())
                    });
                }
                Err(e) => return Err(format!("Unmount failed: {e}")),
            }
        }
    }
    let _ = std::fs::remove_dir(&mp); // only removes an empty folder
    Ok(())
}

pub(crate) fn mounted(paths: &Paths, saved: &[Conn]) -> Vec<(String, PathBuf)> {
    read_mounts(paths)
        .into_iter()
        .filter(|m| {
            m.fstype == "fuse.rclone" && m.target.parent() == Some(paths.mount_root.as_path())
        })
        .filter_map(|m| {
            saved
                .iter()
                .find(|c| mount_point(paths, c) == m.target)
                .map(|c| (c.id.clone(), m.target))
        })
        .collect()
}

// ----------------------------------------------------------------- errors

fn log_tail(log: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(log) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(8192)));
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    s
}

/// A short message for the user from rclone's log/stderr.
pub(crate) fn friendly_error(text: &str, conn: &Conn) -> String {
    let t = text.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| t.contains(n));
    let target = format!("{}:{}", conn.host, effective_port(conn));
    if has(&[
        "unable to authenticate",
        "permission denied (publickey",
        "auth failed",
        "authentication failed",
        "status_logon_failure",
        "401 unauthorized",
        "login incorrect",
        "530 ",
        "invalid credentials",
        "no supported methods remain",
    ]) {
        return "Login failed — check the user name and password or key".into();
    }
    if has(&[
        "ssh-agent",
        "ssh_auth_sock",
        "couldn't connect to ssh-agent",
    ]) {
        return "No SSH key available — start ssh-agent or pick a key file".into();
    }
    if has(&[
        "no such host",
        "name or service not known",
        "server misbehaving",
        "lookup ",
    ]) && !has(&["directory not found"])
    {
        return format!("Unknown host \"{}\"", conn.host);
    }
    if has(&["connection refused"]) {
        return format!("{target} refused the connection — is the service running?");
    }
    if has(&[
        "no route to host",
        "network is unreachable",
        "i/o timeout",
        "connection timed out",
        "deadline exceeded",
    ]) {
        return format!("Can't reach {target}");
    }
    if has(&["status_bad_network_name"]) {
        return "That share doesn't exist on the server".into();
    }
    if has(&[
        "directory not found",
        "object not found",
        "no such file",
        "404 not found",
        "file does not exist",
    ]) {
        return "Remote folder not found".into();
    }
    if has(&["fusermount", "fuse: ", "/dev/fuse"]) {
        return "FUSE mount failed — is fuse3 installed and /dev/fuse usable?".into();
    }
    if has(&["certificate", "x509"]) {
        return "The server's TLS certificate isn't trusted".into();
    }
    // fall back to the last error line, trimmed of rclone's prefix
    let line = text
        .lines()
        .rev()
        .find(|l| l.contains("ERROR") || l.contains("CRITICAL") || l.contains("Fatal"))
        .or_else(|| text.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("rclone failed");
    let msg = line
        .rsplit_once(": ")
        .map(|(_, m)| m)
        .filter(|m| m.len() > 8)
        .unwrap_or(line)
        .trim();
    let mut msg: String = msg.chars().take(160).collect();
    if msg.is_empty() {
        msg = "rclone failed".into();
    }
    msg
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::testdir::TestDir;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Records every command; plays rclone/fusermount3 against the fake
    /// /proc/mounts file.
    struct Fake {
        cmds: Mutex<Vec<Cmd>>,
        mounts: PathBuf,
        mount_ok: bool,
    }

    impl Fake {
        fn new(paths: &Paths) -> Self {
            Fake {
                cmds: Mutex::new(Vec::new()),
                mounts: paths.proc_mounts.clone(),
                mount_ok: true,
            }
        }
        fn cmds(&self) -> Vec<Cmd> {
            self.cmds.lock().unwrap().clone()
        }
    }

    impl Runner for Fake {
        async fn run(&self, cmd: Cmd, _t: Duration) -> Result<Out, String> {
            self.cmds.lock().unwrap().push(cmd.clone());
            let ok = |s: &str| {
                Ok(Out {
                    ok: true,
                    stdout: s.into(),
                    stderr: String::new(),
                })
            };
            match (cmd.program.as_str(), cmd.args.first().map(String::as_str)) {
                ("rclone", Some("obscure")) => ok("OBSCURED-xyz\n"),
                ("rclone", Some("mount")) => {
                    let mp = &cmd.args[2];
                    let log =
                        &cmd.args[cmd.args.iter().position(|a| a == "--log-file").unwrap() + 1];
                    if self.mount_ok {
                        let line = format!(
                            "{} {} fuse.rclone rw,nosuid,nodev 0 0\n",
                            cmd.args[1],
                            mp.replace(' ', "\\040")
                        );
                        let mut cur = std::fs::read_to_string(&self.mounts).unwrap();
                        cur.push_str(&line);
                        std::fs::write(&self.mounts, cur).unwrap();
                        ok("")
                    } else {
                        std::fs::write(log, "2026/10/12 ERROR : sftp: ssh: handshake failed: ssh: unable to authenticate, attempted methods [none password]\n").unwrap();
                        Ok(Out {
                            ok: false,
                            ..Default::default()
                        })
                    }
                }
                ("rclone", Some("lsf")) => ok("a.txt\nsub/\n\nb\n"),
                ("fusermount3", Some("-u")) => {
                    let mp = cmd.args[1].replace(' ', "\\040");
                    let cur = std::fs::read_to_string(&self.mounts).unwrap();
                    let kept: String = cur
                        .lines()
                        .filter(|l| !l.contains(&mp))
                        .map(|l| format!("{l}\n"))
                        .collect();
                    std::fs::write(&self.mounts, kept).unwrap();
                    ok("")
                }
                _ => Err(format!("unexpected {cmd:?}")),
            }
        }
    }

    #[derive(Default)]
    struct FakeKeyring(Mutex<HashMap<String, String>>);

    impl Keyring for FakeKeyring {
        async fn lookup(&self, id: &str) -> Result<Option<String>, String> {
            Ok(self.0.lock().unwrap().get(id).cloned())
        }
        async fn store(&self, id: &str, _label: &str, pw: &str) -> Result<(), String> {
            self.0.lock().unwrap().insert(id.into(), pw.into());
            Ok(())
        }
    }

    fn facts(agent: bool) -> Facts {
        Facts {
            local_user: "me".into(),
            agent,
            default_key: Some("/home/me/.ssh/id_ed25519".into()),
            home: "/home/me".into(),
        }
    }

    fn conn(url: &str) -> Conn {
        let mut c = crate::net::url::parse(url, "me").unwrap();
        c.id = "test-id".into();
        c
    }

    fn env_map(r: &Remote) -> HashMap<String, String> {
        r.env
            .iter()
            .map(|(k, v)| (k.strip_prefix(ENV_PREFIX).unwrap().to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn net_rclone_env_sftp_agent_and_alias() {
        let c = conn("sftp://go/~/media");
        let r = remote(&c, None, None, &facts(true)).unwrap();
        let e = env_map(&r);
        assert_eq!(e["TYPE"], "sftp");
        assert_eq!(
            (e["HOST"].as_str(), e["USER"].as_str(), e["PORT"].as_str()),
            ("go", "me", "22")
        );
        assert_eq!(e["KEY_USE_AGENT"], "true");
        assert!(!e.contains_key("KEY_FILE") && !e.contains_key("PASS"));
        assert_eq!(r.spec, "OGF:media");
        assert_eq!(r.flags, vec!["--sftp-set-modtime=true"]);

        // no agent → default key
        let e = env_map(&remote(&c, None, None, &facts(false)).unwrap());
        assert_eq!(e["KEY_FILE"], "/home/me/.ssh/id_ed25519");
        assert!(!e.contains_key("KEY_USE_AGENT"));

        // ssh alias: HostName/User/Port from ~/.ssh/config
        let t = TestDir::new("rclone-alias");
        let key = t.path().join("id_go");
        std::fs::write(&key, "k").unwrap();
        let ssh = SshHost {
            alias: "go".into(),
            hostname: Some("100.64.0.7".into()),
            user: Some("alice".into()),
            port: Some(2222),
            identity: Some(key.clone()),
        };
        let r = remote(&conn("sftp://go/srv"), Some(&ssh), None, &facts(true)).unwrap();
        let e = env_map(&r);
        assert_eq!(
            (e["HOST"].as_str(), e["USER"].as_str(), e["PORT"].as_str()),
            ("100.64.0.7", "alice", "2222")
        );
        assert_eq!(e["KEY_FILE"], key.display().to_string());
        assert_eq!(e["KEY_USE_AGENT"], "true");
        assert_eq!(r.spec, "OGF:/srv");
        // explicit user/port in the conn win
        let e =
            env_map(&remote(&conn("sftp://bob@go:23"), Some(&ssh), None, &facts(true)).unwrap());
        assert_eq!((e["USER"].as_str(), e["PORT"].as_str()), ("bob", "23"));
    }

    #[test]
    fn net_rclone_env_sftp_password_and_key() {
        let mut c = conn("sftp://u@h");
        c.auth = Auth::Password;
        assert_eq!(
            remote(&c, None, None, &facts(true)).unwrap_err(),
            NEED_PASSWORD
        );
        let e = env_map(&remote(&c, None, Some("OBS"), &facts(true)).unwrap());
        assert_eq!(e["PASS"], "OBS");
        assert!(!e.contains_key("KEY_USE_AGENT"));
        c.auth = Auth::KeyFile("~/.ssh/work".into());
        let e = env_map(&remote(&c, None, Some("OBS"), &facts(true)).unwrap());
        let want = PathBuf::from("/home/me").join(".ssh/work");
        assert_eq!(e["KEY_FILE"], want.to_string_lossy());
        assert_eq!(e["KEY_FILE_PASS"], "OBS");
        c.auth = Auth::Anonymous;
        assert!(remote(&c, None, None, &facts(true)).is_err());
    }

    #[test]
    fn net_rclone_env_smb_webdav_ftp() {
        let r = remote(
            &conn("smb://CORP;bob@nas:1445/media/films/"),
            None,
            Some("OBS"),
            &facts(true),
        )
        .unwrap();
        let e = env_map(&r);
        assert_eq!(e["TYPE"], "smb");
        assert_eq!(
            (e["DOMAIN"].as_str(), e["USER"].as_str(), e["PORT"].as_str()),
            ("CORP", "bob", "1445")
        );
        assert_eq!(e["PASS"], "OBS");
        assert_eq!(r.spec, "OGF:media/films");
        assert!(r.flags.is_empty());
        let e = env_map(&remote(&conn("smb://nas/public"), None, None, &facts(true)).unwrap());
        assert_eq!(e["USER"], "guest");
        assert!(!e.contains_key("PASS") && !e.contains_key("PORT"));

        let r = remote(
            &conn("davs://u@cloud.example.com/remote.php/dav/My Files"),
            None,
            Some("OBS"),
            &facts(true),
        )
        .unwrap();
        let e = env_map(&r);
        assert_eq!(
            e["URL"],
            "https://cloud.example.com/remote.php/dav/My%20Files"
        );
        assert_eq!(
            (e["VENDOR"].as_str(), e["USER"].as_str(), e["PASS"].as_str()),
            ("other", "u", "OBS")
        );
        assert_eq!(r.spec, "OGF:");
        let mut c = conn("dav://[fd00::1]:8080/dav");
        c.auth = Auth::Anonymous;
        let e = env_map(&remote(&c, None, None, &facts(true)).unwrap());
        assert_eq!(e["URL"], "http://[fd00::1]:8080/dav");
        assert!(!e.contains_key("USER") && !e.contains_key("PASS"));
        // LAN-discovered _webdavs: port 443 without tls flag → https
        let mut c = conn("dav://nas");
        c.port = Some(443);
        assert_eq!(
            env_map(&remote(&c, None, None, &facts(true)).unwrap())["URL"],
            "https://nas"
        );

        let r = remote(&conn("ftp://h/pub/x"), None, Some("OBS"), &facts(true)).unwrap();
        let e = env_map(&r);
        assert_eq!(
            (e["TYPE"].as_str(), e["USER"].as_str(), e["PORT"].as_str()),
            ("ftp", "anonymous", "21")
        );
        assert_eq!(e["EXPLICIT_TLS"], "false");
        assert_eq!(r.spec, "OGF:pub/x");
        let e = env_map(&remote(&conn("ftps://u@h"), None, Some("OBS"), &facts(true)).unwrap());
        assert_eq!(
            (e["USER"].as_str(), e["EXPLICIT_TLS"].as_str()),
            ("u", "true")
        );
    }

    #[test]
    fn net_rclone_folder_names() {
        let mut c = conn("sftp://go");
        c.name = "go — media/films".into();
        assert_eq!(folder_name(&c), "go — media-films");
        c.name = "  ..hidden\n ".into();
        assert_eq!(folder_name(&c), "hidden-");
        c.name = "...".into();
        assert_eq!(folder_name(&c), "test-id");
        c.name = "x".repeat(200);
        assert_eq!(folder_name(&c).len(), 80);
    }

    #[test]
    fn net_rclone_mac_mount_output_and_pid_file() {
        let text = "/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)\n\
                    OGF:media on /Users/me/Network/go media (macfuse, nodev, nosuid, synchronous, mounted by me)\n\
                    map auto_home on /System/Volumes/Data/home (autofs, automounted, nobrowse)\n";
        let m = parse_mount_output(text);
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].fstype, "apfs");
        assert_eq!(m[0].target, PathBuf::from("/"));
        assert_eq!(m[1].fstype, "fuse.rclone");
        assert_eq!(m[1].target, PathBuf::from("/Users/me/Network/go media"));
        assert_eq!(m[2].fstype, "autofs");
        assert_eq!(
            parse_pid_file("4242\nC:\\Users\\me\\Network\\nas\n"),
            Some((4242, PathBuf::from("C:\\Users\\me\\Network\\nas")))
        );
        assert_eq!(parse_pid_file("x\nC:\\a"), None);
        assert_eq!(parse_pid_file("12\n"), None);
    }

    #[cfg(target_os = "linux")] // /proc/self/mountinfo format
    #[test]
    fn net_rclone_proc_mounts_parsing() {
        let text = "proc /proc proc rw 0 0\n\
                    OGF:/srv /home/me/Network/go\\040—\\040media fuse.rclone rw,nosuid,nodev,relatime,user_id=1000 0 0\n\
                    /dev/sda1 /home/me/Network/disk ext4 rw 0 0\n\
                    OGF: /home/me/elsewhere fuse.rclone rw 0 0\n";
        let m = parse_mounts(text);
        assert_eq!(m.len(), 4);
        assert_eq!(m[1].target, PathBuf::from("/home/me/Network/go — media"));
        assert_eq!(m[1].fstype, "fuse.rclone");
        assert_eq!(m[1].source, "OGF:/srv");

        let t = TestDir::new("rclone-mounts");
        let mut paths = t.paths();
        paths.mount_root = "/home/me/Network".into();
        std::fs::write(&paths.proc_mounts, text).unwrap();
        let mut a = conn("sftp://go");
        a.name = "go — media".into();
        let mut b = conn("sftp://x");
        b.id = "other".into();
        b.name = "disk".into(); // ext4 there, not ours
        assert_eq!(
            mounted(&paths, &[a, b]),
            vec![(
                "test-id".to_string(),
                PathBuf::from("/home/me/Network/go — media")
            )]
        );
    }

    #[cfg(target_os = "linux")] // /proc/self/mountinfo format
    #[tokio::test]
    async fn net_rclone_mount_flow_password_never_in_argv() {
        let t = TestDir::new("rclone-mount");
        let paths = t.paths();
        let fake = Fake::new(&paths);
        let kr = FakeKeyring::default();
        let mut c = conn("smb://bob@nas/media");
        c.name = "nas media".into();
        c.auth = Auth::Password;

        // no password, none remembered
        let e = mount_with(
            &paths,
            &fake,
            &kr,
            &c,
            None,
            false,
            &facts(true),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(e.unwrap_err(), NEED_PASSWORD);
        assert!(fake.cmds().is_empty());

        let mp = mount_with(
            &paths,
            &fake,
            &kr,
            &c,
            Some("hunter2".into()),
            true,
            &facts(true),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(mp, paths.mount_root.join("nas media"));
        assert!(mp.is_dir());
        let cmds = fake.cmds();
        assert_eq!(cmds.len(), 2);
        // obscure got the password on stdin, not argv
        assert_eq!(cmds[0].args, vec!["obscure", "-"]);
        assert_eq!(cmds[0].stdin.as_deref(), Some("hunter2\n"));
        let m = &cmds[1];
        assert!(m.detach && m.stdin.is_none());
        for c in &cmds {
            assert!(
                c.args.iter().all(|a| !a.contains("hunter2")),
                "{:?}",
                c.args
            );
        }
        assert!(m.env.iter().all(|(_, v)| !v.contains("hunter2")));
        assert!(m
            .env
            .contains(&("RCLONE_CONFIG_OGF_PASS".into(), "OBSCURED-xyz".into())));
        let log = paths.state_dir.join("rclone-test-id.log");
        let want: Vec<String> = [
            "mount",
            "OGF:media",
            &mp.display().to_string(),
            "--daemon",
            "--daemon-wait",
            "10s",
            "--vfs-cache-mode",
            "writes",
            "--dir-cache-time",
            "30s",
            "--poll-interval",
            "30s",
            "--log-file",
            &log.display().to_string(),
            "--log-level",
            "NOTICE",
            "--volname",
            "nas media",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(m.args, want);
        // remembered after success
        assert_eq!(
            kr.0.lock().unwrap().get("test-id").map(String::as_str),
            Some("hunter2")
        );
        assert_eq!(
            mounted(&paths, std::slice::from_ref(&c)),
            vec![("test-id".to_string(), mp.clone())]
        );

        // mounting again is a no-op returning the same folder
        let again = mount_with(
            &paths,
            &fake,
            &kr,
            &c,
            None,
            false,
            &facts(true),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(again.unwrap(), mp);
        assert_eq!(fake.cmds().len(), 2);

        // unmount → fusermount3 -u, folder removed
        unmount(&paths, &fake, &c).await.unwrap();
        let last = fake.cmds().pop().unwrap();
        assert_eq!(last.program, "fusermount3");
        assert_eq!(last.args, vec!["-u".to_string(), mp.display().to_string()]);
        assert!(!mp.exists());
        assert!(mounted(&paths, &[c.clone()]).is_empty());

        // next mount uses the keyring
        mount_with(
            &paths,
            &fake,
            &kr,
            &c,
            None,
            false,
            &facts(true),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(
            fake.cmds()[fake.cmds().len() - 2].stdin.as_deref(),
            Some("hunter2\n")
        );
        unmount(&paths, &fake, &c).await.unwrap();
    }

    #[tokio::test]
    async fn net_rclone_mount_refusals_and_errors() {
        let t = TestDir::new("rclone-refuse");
        let paths = t.paths();
        let mut fake = Fake::new(&paths);
        let kr = FakeKeyring::default();
        let c = conn("sftp://go/srv");
        let mp = mount_point(&paths, &c);
        std::fs::create_dir_all(&mp).unwrap();
        std::fs::write(mp.join("precious.txt"), "x").unwrap();
        let e = mount_with(
            &paths,
            &fake,
            &kr,
            &c,
            None,
            false,
            &facts(true),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(e.contains("isn't empty"), "{e}");
        assert!(mp.join("precious.txt").exists());
        assert!(fake.cmds().is_empty());
        std::fs::remove_file(mp.join("precious.txt")).unwrap();

        // auth failure → readable message from the log, folder cleaned up
        fake.mount_ok = false;
        let e = mount_with(
            &paths,
            &fake,
            &kr,
            &c,
            None,
            true,
            &facts(true),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert_eq!(e, "Login failed — check the user name and password or key");
        assert!(!mp.exists());
        assert!(kr.0.lock().unwrap().is_empty());
        // agent auth: no obscure call at all
        assert!(fake.cmds().iter().all(|c| c.args[0] != "obscure"));
    }

    #[tokio::test]
    async fn net_rclone_test_command() {
        let t = TestDir::new("rclone-test");
        let paths = t.paths();
        let fake = Fake::new(&paths);
        let kr = FakeKeyring::default();
        let mut c = conn("ftp://h/pub");
        c.auth = Auth::Anonymous;
        let ok = test_with(&paths, &fake, &kr, &c, None, &facts(true))
            .await
            .unwrap();
        assert_eq!(ok, "3 items");
        let cmds = fake.cmds();
        assert_eq!(cmds[0].stdin.as_deref(), Some("anonymous@\n"));
        let lsf = &cmds[1];
        assert_eq!(&lsf.args[..3], &["lsf", "--max-depth", "1"]);
        assert_eq!(lsf.args.last().unwrap(), "OGF:pub");
        assert!(!lsf.detach);
    }

    #[test]
    fn net_rclone_friendly_errors() {
        let c = conn("sftp://go");
        let f = |s: &str| friendly_error(s, &c);
        assert_eq!(
            f("ERROR : dial tcp 100.1.1.1:22: connect: connection refused"),
            "go:22 refused the connection — is the service running?"
        );
        assert_eq!(
            f("dial tcp: lookup go: no such host"),
            "Unknown host \"go\""
        );
        assert_eq!(f("dial tcp 1.2.3.4:22: i/o timeout"), "Can't reach go:22");
        assert_eq!(
            f("ERROR : : error reading source root directory: directory not found"),
            "Remote folder not found"
        );
        assert_eq!(
            f("smb: STATUS_LOGON_FAILURE"),
            "Login failed — check the user name and password or key"
        );
        assert_eq!(
            f("2026/10/12 CRITICAL: Fatal error: something odd happened here"),
            "something odd happened here"
        );
        assert_eq!(f(""), "rclone failed");
    }

    /// Real rclone, only when installed: obscure via stdin round trip.
    #[tokio::test]
    async fn net_rclone_integration_obscure() {
        if !on_path("rclone") {
            eprintln!("rclone not installed; skipping");
            return;
        }
        let o = obscure(&System, "s3cret pass").await.unwrap();
        assert!(!o.is_empty() && !o.contains("s3cret"));
        let out = System
            .run(
                Cmd {
                    program: "rclone".into(),
                    args: vec!["reveal".into(), o],
                    ..Default::default()
                },
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout.trim(), "s3cret pass");
    }

    #[tokio::test]
    async fn net_rclone_system_runner_stdin() {
        let out = System
            .run(
                Cmd {
                    program: "cat".into(),
                    stdin: Some("hello".into()),
                    ..Default::default()
                },
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert!(out.ok);
        assert_eq!(out.stdout, "hello");
        let e = System
            .run(
                Cmd {
                    program: "og-files-no-such-tool".into(),
                    ..Default::default()
                },
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
        assert!(e.contains("not installed"));
        assert!(on_path("sh"));
        assert!(!on_path("og-files-no-such-tool"));
    }
}
