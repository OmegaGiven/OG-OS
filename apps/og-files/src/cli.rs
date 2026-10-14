//! Command-line subcommands that run without opening a window: archive
//! extract / compress / list and "open with". Detected in `main` before
//! iced starts; anything else falls through to the GUI flags.

use crate::archops::ArchiveJob;
use crate::openwith::{self, Env, Registry};
use crate::viewers::archive::{self, Format};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub const HELP: &str = "\
OG Files — file manager with built-in viewers and editors

Usage:
  og-files [--view tree|list|grid] [--search TEXT] [--focus] [--usage DIR] [PATH]
      Open the window. A folder PATH is shown in the files pane, a file PATH
      opens in a tab. --view picks the files pane view, --search starts a
      search, --focus widens the files pane, --usage opens the disk usage
      analyzer on DIR.

  og-files extract ARCHIVE [DEST] [--password PW]
      Extract into folder DEST (default: the current folder). An archive with
      one top-level item puts it straight into DEST; otherwise a new folder
      named after the archive is made. Taken names get \" (2)\". Prints the
      result's path.

  og-files compress OUT.{zip,tar.gz,tar.zst,tar.xz,7z} FILE...
      Pack files and folders into a new archive (format from OUT's name).

  og-files list ARCHIVE [--password PW]
      List the entries with their sizes (zip, tar[.gz/.xz/.zst/.bz2], 7z,
      single .gz/.xz/.zst/.bz2 files).

  og-files open FILE...
      Open files in their default apps (mimeapps.list, as other desktops do).

  og-files open-with APP_ID FILE...
      Open files with the app whose desktop file id is APP_ID
      (e.g. org.gnome.Loupe or libreoffice-writer; \".desktop\" optional).

  og-files apps FILE
      List the apps that can open FILE; the default is marked with *.

  og-files --help
      This text.

Archive work runs in-process (no external tools). Progress goes to stderr
for big jobs; errors exit with status 1 (2 for bad arguments). To open a
file literally named like a subcommand in the window, write ./NAME.
";

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Help,
    Extract {
        archive: PathBuf,
        dest: Option<PathBuf>,
        password: Option<String>,
    },
    Compress {
        out: PathBuf,
        files: Vec<PathBuf>,
    },
    List {
        archive: PathBuf,
        password: Option<String>,
    },
    Open {
        files: Vec<PathBuf>,
    },
    OpenWith {
        app: String,
        files: Vec<PathBuf>,
    },
    Apps {
        file: PathBuf,
    },
}

/// `Ok(None)` = not a subcommand: start the GUI with these args.
pub fn parse(args: &[OsString]) -> Result<Option<Cmd>, String> {
    let Some(first) = args.first().and_then(|a| a.to_str()) else {
        return Ok(None);
    };
    if matches!(first, "-h" | "--help") {
        return Ok(Some(Cmd::Help));
    }
    let sub = match first {
        "extract" | "compress" | "list" | "open" | "open-with" | "apps" => first,
        _ => return Ok(None),
    };
    // positional args + --password
    let mut pos: Vec<OsString> = Vec::new();
    let mut password = None;
    let mut it = args[1..].iter();
    let mut opts_done = false;
    while let Some(a) = it.next() {
        let s = a.to_str().unwrap_or("");
        if opts_done {
            pos.push(a.clone());
            continue;
        }
        match s {
            "--" => opts_done = true,
            "-h" | "--help" => return Ok(Some(Cmd::Help)),
            "--password" | "-p" => {
                password = Some(
                    it.next()
                        .and_then(|p| p.to_str())
                        .ok_or("--password needs a value")?
                        .to_string(),
                )
            }
            _ if s.starts_with("--password=") => {
                password = Some(s["--password=".len()..].to_string())
            }
            _ if s.starts_with('-') && s.len() > 1 => {
                return Err(format!("{sub}: unknown option {s}"))
            }
            _ => pos.push(a.clone()),
        }
    }
    let takes_pw = matches!(sub, "extract" | "list");
    if password.is_some() && !takes_pw {
        return Err(format!(
            "{sub}: --password only applies to extract and list"
        ));
    }
    let p = |a: &OsString| PathBuf::from(a);
    let usage = |u: &str| Err(format!("usage: og-files {u}  (see og-files --help)"));
    let cmd = match sub {
        "extract" => match pos.as_slice() {
            [a] => Cmd::Extract {
                archive: p(a),
                dest: None,
                password,
            },
            [a, d] => Cmd::Extract {
                archive: p(a),
                dest: Some(p(d)),
                password,
            },
            _ => return usage("extract ARCHIVE [DEST] [--password PW]"),
        },
        "compress" => match pos.as_slice() {
            [out, files @ ..] if !files.is_empty() => {
                let out = p(out);
                if Format::from_name(&out).is_none() {
                    return Err(format!(
                        "compress: {} — the name must end in .zip, .tar.gz, .tar.zst, .tar.xz or .7z",
                        out.display()
                    ));
                }
                Cmd::Compress {
                    out,
                    files: files.iter().map(p).collect(),
                }
            }
            _ => return usage("compress OUT.{zip,tar.gz,tar.zst,tar.xz,7z} FILE..."),
        },
        "list" => match pos.as_slice() {
            [a] => Cmd::List {
                archive: p(a),
                password,
            },
            _ => return usage("list ARCHIVE [--password PW]"),
        },
        "open" => match pos.as_slice() {
            [] => return usage("open FILE..."),
            fs => Cmd::Open {
                files: fs.iter().map(p).collect(),
            },
        },
        "open-with" => match pos.as_slice() {
            [app, fs @ ..] => Cmd::OpenWith {
                app: app.to_string_lossy().into_owned(),
                files: fs.iter().map(p).collect(),
            },
            _ => return usage("open-with APP_ID FILE..."),
        },
        _ => match pos.as_slice() {
            [f] => Cmd::Apps { file: p(f) },
            _ => return usage("apps FILE"),
        },
    };
    Ok(Some(cmd))
}

/// Absolute path without resolving symlinks (`std::path::absolute`).
fn abs(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn must_exist(files: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    files
        .iter()
        .map(|f| {
            if std::fs::symlink_metadata(f).is_ok() {
                Ok(abs(f))
            } else {
                Err(format!("{}: no such file or folder", f.display()))
            }
        })
        .collect()
}

/// Run an archive job, with a percentage on stderr when it is big and
/// stderr is a terminal.
fn run_job(job: &ArchiveJob) -> Result<(PathBuf, Vec<String>), String> {
    let cancel = AtomicBool::new(false);
    let total = job.total(&cancel);
    // SAFETY: isatty only inspects the descriptor
    let tty = unsafe { libc::isatty(2) } == 1;
    let show = tty && total >= 8 << 20;
    let label = job.label();
    let (mut done, mut last) = (0u64, Instant::now());
    let r = job.run(&cancel, &mut |n| {
        done += n;
        if show && last.elapsed() >= Duration::from_millis(200) {
            last = Instant::now();
            let pct = (done as f64 / total as f64 * 100.0).min(100.0);
            eprint!("\r{label}: {pct:3.0}%");
            let _ = std::io::stderr().flush();
        }
    });
    if show {
        eprintln!("\r{label}: {}", if r.is_ok() { "done" } else { "failed" });
    }
    r
}

fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < U.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn exec(cmd: Cmd) -> Result<(), String> {
    match cmd {
        Cmd::Help => {
            print!("{HELP}");
            Ok(())
        }
        Cmd::Extract {
            archive,
            dest,
            password,
        } => {
            let archive = must_exist(&[archive])?.remove(0);
            if !archive::supported(&archive) {
                return Err(format!("{}: not a supported archive", archive.display()));
            }
            let parent = abs(&dest.unwrap_or_else(|| PathBuf::from(".")));
            std::fs::create_dir_all(&parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            let had_pw = password.is_some();
            let job = ArchiveJob::ExtractHere {
                archive,
                parent,
                password,
            };
            match run_job(&job) {
                Ok((out, skipped)) => {
                    for s in &skipped {
                        eprintln!("skipped (unsafe or unsupported): {s}");
                    }
                    println!("{}", out.display());
                    Ok(())
                }
                Err(e) if archive::is_password_error(&e) && !had_pw => {
                    Err(format!("{e} (use --password PW)"))
                }
                Err(e) => Err(e),
            }
        }
        Cmd::Compress { out, files } => {
            let files = must_exist(&files)?;
            let out = abs(&out);
            if std::fs::symlink_metadata(&out).is_ok() {
                return Err(format!("{}: already exists", out.display()));
            }
            let format = Format::from_name(&out).ok_or("unsupported output format")?;
            let job = ArchiveJob::Compress {
                sources: files,
                dest: out,
                format,
            };
            let (out, _) = run_job(&job)?;
            println!("{}", out.display());
            Ok(())
        }
        Cmd::List { archive, password } => {
            let entries = archive::list_entries(&archive, password.as_deref()).map_err(|e| {
                if archive::is_password_error(&e) && password.is_none() {
                    format!("{e} (use --password PW)")
                } else {
                    e
                }
            })?;
            let mut out = std::io::stdout().lock();
            let mut total = 0;
            for e in &entries {
                let size = match (e.is_dir, e.size) {
                    (true, _) => "-".to_string(),
                    (false, Some(s)) => {
                        total += s;
                        s.to_string()
                    }
                    (false, None) => "?".to_string(),
                };
                let mark = if e.is_dir { "/" } else { "" };
                let lock = if e.encrypted { "  (encrypted)" } else { "" };
                let _ = writeln!(out, "{size:>12}  {}{mark}{lock}", e.path);
            }
            let files = entries.iter().filter(|e| !e.is_dir).count();
            let _ = writeln!(out, "{files} files, {} ({total} bytes)", human(total));
            Ok(())
        }
        Cmd::Open { files } => {
            let files = must_exist(&files)?;
            let reg = Registry::load(&Env::from_env());
            for line in openwith::open_default(&reg, &files)? {
                eprintln!("{line}");
            }
            Ok(())
        }
        Cmd::OpenWith { app, files } => {
            let files = must_exist(&files)?;
            let reg = Registry::load(&Env::from_env());
            let a = reg
                .get(&app)
                .ok_or_else(|| format!("no app with id {app} (og-files apps FILE lists them)"))?;
            openwith::launch(a, &files)?;
            eprintln!("Opened {} in {}", openwith::describe(&files), a.name);
            Ok(())
        }
        Cmd::Apps { file } => {
            let file = must_exist(&[file])?.remove(0);
            let reg = Registry::load(&Env::from_env());
            let mime = reg.mime_of(&file);
            let c = reg.choices(&mime);
            println!("{mime}");
            if c.apps.is_empty() {
                println!("  (no app is registered for this type)");
            }
            for id in &c.apps {
                if let Some(a) = reg.get(id) {
                    let mark = if c.default.as_deref() == Some(id) {
                        '*'
                    } else {
                        ' '
                    };
                    println!("{mark} {:<40} {}", a.short_id(), a.name);
                }
            }
            Ok(())
        }
    }
}

/// Run a subcommand; returns the process exit code.
pub fn run(cmd: Cmd) -> i32 {
    match exec(cmd) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("og-files: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_subcommands() {
        assert_eq!(parse(&a(&[])).unwrap(), None);
        assert_eq!(parse(&a(&["--view", "grid", "/tmp"])).unwrap(), None);
        assert_eq!(parse(&a(&["notes.md"])).unwrap(), None);
        assert_eq!(parse(&a(&["--help"])).unwrap(), Some(Cmd::Help));
        assert_eq!(parse(&a(&["-h"])).unwrap(), Some(Cmd::Help));
        assert_eq!(parse(&a(&["list", "--help"])).unwrap(), Some(Cmd::Help));
        assert_eq!(
            parse(&a(&["extract", "a.zip"])).unwrap(),
            Some(Cmd::Extract {
                archive: "a.zip".into(),
                dest: None,
                password: None
            })
        );
        assert_eq!(
            parse(&a(&["extract", "--password", "pw", "a.7z", "out"])).unwrap(),
            Some(Cmd::Extract {
                archive: "a.7z".into(),
                dest: Some("out".into()),
                password: Some("pw".into())
            })
        );
        assert_eq!(
            parse(&a(&["list", "a.zip", "--password=s e"])).unwrap(),
            Some(Cmd::List {
                archive: "a.zip".into(),
                password: Some("s e".into())
            })
        );
        assert_eq!(
            parse(&a(&["compress", "o.tar.zst", "x", "--", "-dash"])).unwrap(),
            Some(Cmd::Compress {
                out: "o.tar.zst".into(),
                files: vec!["x".into(), "-dash".into()]
            })
        );
        assert_eq!(
            parse(&a(&["open-with", "org.gnome.Loupe", "a.png"])).unwrap(),
            Some(Cmd::OpenWith {
                app: "org.gnome.Loupe".into(),
                files: vec!["a.png".into()]
            })
        );
        assert_eq!(
            parse(&a(&["open", "a", "b"])).unwrap(),
            Some(Cmd::Open {
                files: vec!["a".into(), "b".into()]
            })
        );
        assert_eq!(
            parse(&a(&["apps", "a.png"])).unwrap(),
            Some(Cmd::Apps {
                file: "a.png".into()
            })
        );
        for bad in [
            &["extract"][..],
            &["extract", "a", "b", "c"],
            &["extract", "a", "--password"],
            &["compress", "o.zip"],
            &["compress", "o.rar", "x"],
            &["list"],
            &["open"],
            &["open-with"],
            &["apps", "a", "b"],
            &["apps", "--password", "x", "a"],
            &["list", "--frobnicate", "a"],
        ] {
            assert!(parse(&a(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn cli_round_trip_every_format() {
        let t = std::env::temp_dir().join(format!("ogx-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(t.join("src/proj/sub")).unwrap();
        std::fs::write(t.join("src/proj/a.txt"), "alpha").unwrap();
        std::fs::write(t.join("src/proj/sub/b.txt"), "bravo").unwrap();
        for ext in ["zip", "tar.gz", "tar.zst", "tar.xz", "7z"] {
            let out = t.join(format!("proj.{ext}"));
            let cmd = Cmd::Compress {
                out: out.clone(),
                files: vec![t.join("src/proj")],
            };
            assert_eq!(run(cmd.clone()), 0, "{ext}");
            assert_eq!(run(cmd), 1, "{ext}: refuses to overwrite");
            assert_eq!(
                run(Cmd::List {
                    archive: out.clone(),
                    password: None
                }),
                0
            );
            let dest = t.join(format!("x-{ext}"));
            for _ in 0..2 {
                let r = run(Cmd::Extract {
                    archive: out.clone(),
                    dest: Some(dest.clone()),
                    password: None,
                });
                assert_eq!(r, 0, "{ext}");
            }
            // one top-level folder → straight in; the second run gets " (2)"
            assert_eq!(
                std::fs::read_to_string(dest.join("proj/sub/b.txt")).unwrap(),
                "bravo"
            );
            assert!(dest.join("proj (2)/a.txt").is_file(), "{ext}");
        }
        assert_eq!(
            run(Cmd::Extract {
                archive: t.join("missing.zip"),
                dest: None,
                password: None
            }),
            1
        );
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1536), "1.5 KB");
        let _ = std::fs::remove_dir_all(&t);
    }
}
