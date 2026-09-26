# og-voice

Push-to-talk AI voice assistant overlay for the OG-toolkit suite. Hold a
hotkey, speak; a themed chat-bubble popup in the corner of the screen
shows live (chunked) transcription of what you're saying. Release the
hotkey and it sends the transcript to the `claude` CLI and shows the
answer in the same bubble. Text only in this version — no TTS output yet.

## How it works

- **Press** (`og-voice-launch`): starts `og-voice`, which immediately
  starts recording via `parec`, opens the popup in "Listening…" state,
  and writes its PID to a lock file.
- **Release** (`og-voice --stop`): sends `SIGUSR1` to the already-running
  instance and exits immediately. It does **not** start a second window.
  The running instance catches the signal, stops recording, moves to
  "Processing…", sends the accumulated transcript to `claude -p`, and
  shows the answer.
- Audio is captured in ~1.5s chunks, each chunk is WAV-wrapped and POSTed
  to a local faster-whisper HTTP server; the returned text is appended to
  a running transcript shown live in the bubble. This is chunked, not
  true streaming — see Limitations.
- The window closes on Escape, on click-away (losing focus), or 25s after
  the answer is shown, whichever comes first.

## Install

### 1. The Rust binary + launch wrapper

```sh
cd ~/.local/src/og-voice
scripts/install.sh
```

Installs `~/.local/bin/og-voice` and `~/.local/bin/og-voice-launch`
(atomic cp-then-mv, same as every other app in the suite — never a raw
`cp` over a possibly-running binary).

### 2. The STT server

```sh
scripts/install-stt.sh
```

Creates a Python venv at `~/.local/share/og-voice/stt-venv`, installs
`faster-whisper`, and copies `stt-server/stt_server.py` to
`~/.local/share/og-voice/stt_server.py` (that copy, not the one in this
repo, is what actually runs — re-run this script after editing the
server to pick up changes).

**The Rust app starts this server on demand** — the first time you hold
the hotkey and the server isn't already answering `/health`, `og-voice`
spawns it in the background and waits (up to ~15s) for it to come up
before it starts sending chunks. You don't need to run it manually. The
very first launch after installing will be slow (Whisper downloads its
model — `small.en` by default, a few hundred MB — cached under
`~/.cache/huggingface`); every launch after that just loads the cached
model, still a few seconds.

If you'd rather run it as a standalone process (e.g. to keep the model
warm across every push-to-talk, not just after the first one per
session):

```sh
~/.local/share/og-voice/stt-venv/bin/python3 ~/.local/share/og-voice/stt_server.py
```

Tested manually via `curl`:

```sh
curl -s http://127.0.0.1:8765/health
# {"ok": true}
curl -s -X POST -H "Content-Type: audio/wav" --data-binary @some.wav \
  http://127.0.0.1:8765/transcribe
# {"text": "..."}
```

## sway config — add these yourself

`og-voice` deliberately never edits your live `~/.config/sway/config`.
Add these two lines by hand:

```
bindsym $mod+v exec ~/.local/bin/og-voice-launch
bindsym --release $mod+v exec ~/.local/bin/og-voice --stop
```

(`$mod+v` is just a suggestion — pick whatever's free. If you change it,
also update `hotkey_hint` in `voice-config.json`, purely for
documentation purposes; nothing reads it back into sway.)

Optional but recommended, so the popup floats compactly instead of
tiling:

```
for_window [app_id="og-voice"] floating enable, border none
```

(og-voice sizes and positions its own window — see Config below — so no
`resize`/`move position` needed in this rule, unlike og-search/
og-hotkeys which hardcode their size in the `for_window` rule itself.)

## Config

### Shared theme (`~/.config/sway-power/config.json`)

Read once at startup via `og_config::Config::load()`, same as every other
app — colors, corner radius, gradient toggle, and `default_ai_cli` (which
CLI binary to shell out to; empty defaults to `claude`). Written by
og-settings; restart `og-voice` to pick up a change (load-once, no live
reload, matches the rest of the suite).

### og-voice's own config (`~/.config/sway-power/voice-config.json`)

Created with defaults on first run if it doesn't exist yet:

```json
{
  "ai_backend": "claude-cli",
  "stt_server_url": "http://127.0.0.1:8765",
  "hotkey_hint": "$mod+v"
}
```

- `ai_backend` — only `"claude-cli"` is implemented right now. The field
  exists so a future local-LLM backend can be added without another
  schema change.
- `stt_server_url` — where the Rust app looks for (and, if needed,
  spawns) the STT server.
- `hotkey_hint` — display-only, see above.

og-settings is a large (~3500 line) existing app; this prototype
deliberately does **not** add a tab there. This file is a placeholder for
that future integration — its format was chosen so a settings UI could
read/write it directly later without a migration.

## Architecture notes

- `src/main.rs` — arg parsing (`--stop` vs. normal launch), lock-file
  guard, SIGUSR1 registration, spawns the pipeline, then hands off to
  iced.
- `src/lock.rs` — PID file at `$XDG_RUNTIME_DIR/og-voice.pid` (falls back
  to `/tmp`). `--stop` reads it and shells out to `kill -USR1 <pid>`.
- `src/pipeline.rs` — the whole capture → transcribe → ask-Claude flow,
  entirely on background threads, publishing state through a plain
  `Arc<Mutex<Shared>>` that `App` polls on a 150ms timer. See the doc
  comment there for why this was chosen over threading a channel
  `Receiver` through iced's subscription machinery.
- `src/stt.rs` — `ureq`-based HTTP client + `ensure_running` (health
  check, spawn-if-down, poll).
- `src/claude.rs` — `claude -p "<prompt>"`, captures stdout.
- `src/wav.rs` — raw PCM → WAV wrapping for each chunk.
- `src/placement.rs` — corner positioning via `swaymsg move position`
  (adapted from og-notif-center's; Wayland doesn't let a client position
  its own toplevel, so the IPC move is the actual mechanism, the iced
  `Position::Specific` hint is a no-op best-effort fallback).
- `src/app.rs` — iced State/Message/view, themed via
  `og_theme::AppColors::from_config(&cfg, "og-voice")`.

## Push-to-talk signal choice: SIGUSR1, not a socket

Went with a PID file + `SIGUSR1` over a Unix domain socket: there's never
more than one (sender, receiver) pair in flight — one `--stop` invocation
signaling one running instance — so a socket's listener/accept bookkeeping
would just be reinventing what the kernel's signal delivery already
guarantees for free (exactly-once, race-free delivery to a specific PID).
`signal_hook::flag::register` just flips an `AtomicBool` from the handler,
which the capture loop polls between reads — no signal-handler-unsafe code
involved.

## Known limitations (this is a prototype)

- **No TTS / audio output.** Text only — the answer is displayed, not
  spoken. Documented fast-follow, not in scope here.
- **Chunked, not true streaming, transcription.** Audio is captured in
  ~1.5s chunks, each independently transcribed and appended — this reads
  as "live-ish" but has ~1.5s latency per chunk and no cross-chunk context
  (a sentence split across a chunk boundary is transcribed as two
  independent fragments, which can occasionally garble a word at the
  seam). A real streaming ASR pipeline would fix this but is substantially
  more work.
- **No og-settings integration.** `voice-config.json` is hand-edited only
  in this prototype; see Config above.
- **Single STT model, CPU only, by default.** `OG_VOICE_STT_MODEL` /
  `OG_VOICE_STT_DEVICE` / `OG_VOICE_STT_COMPUTE` env vars (set them before
  starting `stt_server.py`, e.g. in a wrapper) can point it at a bigger
  model or a CUDA device, but nothing in `og-voice` itself surfaces that
  as a setting yet.
- **No retry on a dropped chunk.** If a single `/transcribe` POST fails
  (STT server hiccup, network blip on a POST to localhost — rare but
  possible), that chunk's audio is silently lost rather than retried;
  the transcript just has a gap.
- **First launch after install is slow** (whisper model download) — see
  Install above.
