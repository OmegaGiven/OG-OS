//! Archive extract / compress as jobs: the context menu runs them through
//! [`start`] so they show in the Details jobs list (progress, Cancel, undo =
//! trash the output), and the CLI runs the same [`ArchiveJob::run`]
//! synchronously. The engines live in `viewers::archive` (in-process, no
//! external tools).

use crate::ops::{self, JobControl, JobEvent, Summary, UndoOp};
use crate::viewers::archive::{self, Format};
use iced::futures::channel::mpsc::unbounded;
use iced::futures::Stream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq)]
pub enum ArchiveJob {
    /// smart "Extract here" into folder `parent` (see `archive::extract_here`)
    ExtractHere {
        archive: PathBuf,
        parent: PathBuf,
        password: Option<String>,
    },
    /// everything into the new folder `dest`
    ExtractTo {
        archive: PathBuf,
        dest: PathBuf,
        password: Option<String>,
    },
    Compress {
        sources: Vec<PathBuf>,
        dest: PathBuf,
        format: Format,
    },
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

impl ArchiveJob {
    pub fn label(&self) -> String {
        match self {
            ArchiveJob::ExtractHere { archive, .. } | ArchiveJob::ExtractTo { archive, .. } => {
                format!("Extracting {}", name(archive))
            }
            ArchiveJob::Compress { sources, dest, .. } => match sources.as_slice() {
                [one] => format!("Compressing {} to {}", name(one), name(dest)),
                s => format!("Compressing {} items to {}", s.len(), name(dest)),
            },
        }
    }

    pub fn is_extract(&self) -> bool {
        !matches!(self, ArchiveJob::Compress { .. })
    }

    /// The same extraction, retried with `pw`.
    pub fn with_password(mut self, pw: String) -> Self {
        if let ArchiveJob::ExtractHere { password, .. } | ArchiveJob::ExtractTo { password, .. } =
            &mut self
        {
            *password = Some(pw);
        }
        self
    }

    /// The archive being read or written.
    pub fn archive(&self) -> &Path {
        match self {
            ArchiveJob::ExtractHere { archive, .. } | ArchiveJob::ExtractTo { archive, .. } => {
                archive
            }
            ArchiveJob::Compress { dest, .. } => dest,
        }
    }

    /// Folder whose listing the job changes.
    pub fn out_dir(&self) -> PathBuf {
        match self {
            ArchiveJob::ExtractHere { parent, .. } => parent.clone(),
            ArchiveJob::ExtractTo { dest, .. } | ArchiveJob::Compress { dest, .. } => dest
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| dest.clone()),
        }
    }

    /// Progress total in bytes (0 = unknown).
    pub fn total(&self, cancel: &AtomicBool) -> u64 {
        match self {
            ArchiveJob::ExtractHere {
                archive, password, ..
            }
            | ArchiveJob::ExtractTo {
                archive, password, ..
            } => archive::extract_total(archive, password.as_deref()),
            ArchiveJob::Compress { sources, .. } => archive::source_bytes(sources, cancel),
        }
    }

    /// Do the work (blocking). Ok = (output path, members skipped as unsafe).
    pub fn run(
        &self,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(PathBuf, Vec<String>), String> {
        match self {
            ArchiveJob::ExtractHere {
                archive,
                parent,
                password,
            } => archive::extract_here(archive, parent, password.as_deref(), cancel, progress)
                .map(|x| (x.out, x.skipped)),
            ArchiveJob::ExtractTo {
                archive,
                dest,
                password,
            } => archive::extract_to(archive, &free(dest), password.as_deref(), cancel, progress)
                .map(|x| (x.out, x.skipped)),
            ArchiveJob::Compress {
                sources,
                dest,
                format,
            } => {
                let dest = free(dest);
                archive::compress_with(sources, &dest, *format, cancel, progress)
                    .map(|()| (dest, Vec::new()))
            }
        }
    }
}

/// `p`, or "name (2)…" beside it when `p` is taken (never merge into or
/// overwrite something that exists).
fn free(p: &Path) -> PathBuf {
    match (p.parent(), p.file_name()) {
        (Some(dir), Some(n)) if std::fs::symlink_metadata(p).is_ok() => {
            ops::unique_name(dir, &n.to_string_lossy())
        }
        _ => p.to_path_buf(),
    }
}

/// Run `job` on its own thread with the `ops` event protocol: one
/// `Started`, throttled `Progress`, one `Finished` whose undo trashes the
/// output (`RemoveCopies`).
pub fn start(job: ArchiveJob) -> (JobControl, impl Stream<Item = JobEvent> + Send + 'static) {
    let (control, cancel) = ops::external_control();
    let id = control.id;
    let (tx, rx) = unbounded();
    let current = job.archive().to_path_buf();
    let spawned = std::thread::Builder::new()
        .name(format!("ogx-archive-{id}"))
        .spawn({
            let tx = tx.clone();
            move || {
                let total = job.total(&cancel);
                let _ = tx.unbounded_send(JobEvent::Started {
                    id,
                    label: job.label(),
                    total_items: 1,
                    total_bytes: total,
                });
                let (mut done, mut last) = (0u64, Instant::now());
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    job.run(&cancel, &mut |n| {
                        done += n;
                        if last.elapsed() >= TICK {
                            last = Instant::now();
                            let _ = tx.unbounded_send(JobEvent::Progress {
                                id,
                                done_items: 0,
                                done_bytes: done,
                                current: current.clone(),
                            });
                        }
                    })
                }))
                .unwrap_or_else(|_| Err("Internal error".into()));
                let cancelled = cancel.load(Ordering::Relaxed);
                let finished = match r {
                    Ok((out, skipped)) => JobEvent::Finished {
                        id,
                        result: Ok(Summary {
                            items_done: 1,
                            items_skipped: 0,
                            bytes_done: done,
                            errors: skipped,
                        }),
                        undo: Some(UndoOp::RemoveCopies(vec![out])),
                    },
                    Err(e) => JobEvent::Finished {
                        id,
                        result: Err(if cancelled { "Cancelled".into() } else { e }),
                        undo: None,
                    },
                };
                let _ = tx.unbounded_send(finished);
            }
        });
    if spawned.is_err() {
        let _ = tx.unbounded_send(JobEvent::Started {
            id,
            label: "Archive".into(),
            total_items: 0,
            total_bytes: 0,
        });
        let _ = tx.unbounded_send(JobEvent::Finished {
            id,
            result: Err("Couldn't start the job".into()),
            undo: None,
        });
    }
    (control, rx)
}

/// Output path of a finished archive job (from its undo).
pub fn output_of(undo: &Option<UndoOp>) -> Option<&Path> {
    match undo {
        Some(UndoOp::RemoveCopies(v)) => v.first().map(PathBuf::as_path),
        _ => None,
    }
}

/// Default "Compress" name for `items`: the lone item's name (without its
/// extension for files), else "Archive".
pub fn default_name(items: &[PathBuf]) -> String {
    match items {
        [p] if p.is_dir() => name(p),
        [p] => p
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Archive".into()),
        _ => "Archive".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::StreamExt;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ogx-archops-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[tokio::test]
    async fn job_stream_compress_then_extract() {
        let t = tmp("stream");
        fs::create_dir_all(t.join("src/proj")).unwrap();
        fs::write(t.join("src/proj/a.txt"), "alpha").unwrap();
        let dest = t.join("proj.tar.zst");
        let (_c, s) = start(ArchiveJob::Compress {
            sources: vec![t.join("src/proj")],
            dest: dest.clone(),
            format: Format::TarZst,
        });
        let evs: Vec<JobEvent> = s.collect().await;
        assert!(matches!(
            evs.first(),
            Some(JobEvent::Started { total_bytes: 5, .. })
        ));
        let Some(JobEvent::Finished { result, undo, .. }) = evs.last() else {
            panic!("{evs:?}")
        };
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(output_of(undo), Some(dest.as_path()));

        let out = t.join("out");
        fs::create_dir_all(&out).unwrap();
        let (_c, s) = start(ArchiveJob::ExtractHere {
            archive: dest,
            parent: out.clone(),
            password: None,
        });
        let evs: Vec<JobEvent> = s.collect().await;
        let Some(JobEvent::Finished { result, undo, .. }) = evs.last() else {
            panic!()
        };
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(output_of(undo), Some(out.join("proj").as_path()));
        assert_eq!(fs::read_to_string(out.join("proj/a.txt")).unwrap(), "alpha");
        let _ = fs::remove_dir_all(&t);
    }

    #[tokio::test]
    async fn cancelled_job_reports_cancel() {
        let t = tmp("cancel");
        fs::write(t.join("a.bin"), vec![7u8; 4 << 20]).unwrap();
        let (c, s) = start(ArchiveJob::Compress {
            sources: vec![t.join("a.bin")],
            dest: t.join("a.7z"),
            format: Format::SevenZ,
        });
        c.cancel();
        let evs: Vec<JobEvent> = s.collect().await;
        let Some(JobEvent::Finished { result, undo, .. }) = evs.last() else {
            panic!()
        };
        // a tiny job may finish before the cancel lands; either way no junk
        if result.is_err() {
            assert_eq!(result.clone().unwrap_err(), "Cancelled");
            assert!(undo.is_none());
            assert!(!t.join("a.7z").exists());
        }
        let _ = fs::remove_dir_all(&t);
    }

    #[test]
    fn labels_names_and_password_retry() {
        let j = ArchiveJob::ExtractTo {
            archive: "/x/a.zip".into(),
            dest: "/x/a".into(),
            password: None,
        };
        assert_eq!(j.label(), "Extracting a.zip");
        assert!(j.is_extract());
        assert_eq!(j.out_dir(), PathBuf::from("/x"));
        let j = j.with_password("pw".into());
        assert!(matches!(j, ArchiveJob::ExtractTo { password: Some(ref p), .. } if p == "pw"));
        let c = ArchiveJob::Compress {
            sources: vec!["/a/b".into(), "/a/c".into()],
            dest: "/a/Archive.7z".into(),
            format: Format::SevenZ,
        };
        assert_eq!(c.label(), "Compressing 2 items to Archive.7z");
        assert_eq!(c.clone().with_password("x".into()), c);
        assert_eq!(default_name(&["/no/such/report.pdf".into()]), "report");
        assert_eq!(default_name(&["/a".into(), "/b".into()]), "Archive");
    }
}
