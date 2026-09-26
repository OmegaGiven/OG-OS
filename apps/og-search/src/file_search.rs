use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Filesystem search is real I/O (unlike app/web/AI results, which are all
/// either free or user-typed) — runs via `find` directly (no shell, so the
/// query never risks command injection regardless of what characters it
/// contains — `find`'s own -iname does its own glob matching, not the
/// shell's), bounded by depth so a slow/huge home directory can't make the
/// search bar feel stuck. Streams results one at a time as `find` actually
/// finds them (its stdout is naturally line-buffered per match) rather than
/// waiting for the whole walk to finish, matching how it's meant to keep
/// populating the Files column live while the search stays open.
const MAX_DEPTH: &str = "6";

pub fn stream(query: String) -> impl iced::futures::Stream<Item = PathBuf> {
    iced::stream::channel(32, |mut sender| async move {
        use iced::futures::SinkExt;

        if query.trim().len() < 2 {
            // Single-char queries would match nearly everything under a
            // large home directory — not useful, and the slowest case.
            return;
        }
        let home = std::env::var("HOME").unwrap_or_default();
        let pattern = format!("*{query}*");

        let mut command = tokio::process::Command::new("find");
        command
            .arg(&home)
            .args(["-maxdepth", MAX_DEPTH])
            .args(["-type", "d", "("])
            .args(["-name", ".git", "-o"])
            .args(["-name", "node_modules", "-o"])
            .args(["-name", "target", "-o"])
            .args(["-name", ".cache", "-o"])
            .args(["-name", ".cargo", "-o"])
            .args(["-name", ".rustup"])
            .arg(")")
            .arg("-prune")
            .arg("-o")
            .args(["-iname", &pattern])
            .arg("-print")
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);

        let Ok(mut child) = command.spawn() else { return };
        let Some(stdout) = child.stdout.take() else { return };
        let mut lines = BufReader::new(stdout).lines();

        while let Ok(Some(line)) = lines.next_line().await {
            if line.is_empty() {
                continue;
            }
            if sender.send(PathBuf::from(line)).await.is_err() {
                return;
            }
        }
    })
}
