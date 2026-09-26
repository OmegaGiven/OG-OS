#!/usr/bin/env python3
"""
Minimal local speech-to-text server for og-voice.

POST /transcribe with a raw WAV file in the request body -> {"text": "..."}.
GET  /health                                              -> {"ok": true}

Uses faster-whisper (CTranslate2-based Whisper reimplementation, much
faster than openai-whisper on CPU) with a small/tiny model by default so
a 1.5s audio chunk transcribes well under the chunk interval on modest
hardware. Model size/device are overridable via env vars so this can be
pointed at a GPU or a bigger model without editing code.

Kept to stdlib http.server + faster-whisper (no FastAPI/uvicorn) — one
endpoint, one method, no routing/validation complexity that would
justify a whole ASGI framework. See scripts/install-stt.sh for the venv
this runs in.
"""
import io
import json
import os
import sys
import tempfile
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

try:
    from faster_whisper import WhisperModel
except ImportError:
    print(
        "faster-whisper not installed — run scripts/install-stt.sh first "
        "(this script must be run from the venv it creates).",
        file=sys.stderr,
    )
    sys.exit(1)

HOST = os.environ.get("OG_VOICE_STT_HOST", "127.0.0.1")
PORT = int(os.environ.get("OG_VOICE_STT_PORT", "8765"))
# medium.en over small.en: meaningfully more accurate, still
# CTranslate2/faster-whisper under the hood so still CPU-viable (unlike
# e.g. NVIDIA Parakeet, whose big speed numbers are GPU-only — its
# CPU path is explicitly not recommended upstream). Costs some latency
# per chunk versus small.en; if that trade stops being worth it, drop
# back via OG_VOICE_STT_MODEL=small.en.
MODEL_SIZE = os.environ.get("OG_VOICE_STT_MODEL", "medium.en")
DEVICE = os.environ.get("OG_VOICE_STT_DEVICE", "cpu")
COMPUTE_TYPE = os.environ.get("OG_VOICE_STT_COMPUTE", "int8")

print(f"og-voice STT server: loading model '{MODEL_SIZE}' ({DEVICE}/{COMPUTE_TYPE})…", file=sys.stderr)
_model = WhisperModel(MODEL_SIZE, device=DEVICE, compute_type=COMPUTE_TYPE)
_model_lock = threading.Lock()
print(f"og-voice STT server: ready on http://{HOST}:{PORT}", file=sys.stderr)


class Handler(BaseHTTPRequestHandler):
    # Quiet by default — one line per chunk at info level would spam
    # stderr every ~1.5s while recording; flip this to True while
    # debugging.
    def log_message(self, fmt, *args):
        if os.environ.get("OG_VOICE_STT_VERBOSE"):
            super().log_message(fmt, *args)

    def _send_json(self, status, payload):
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            self._send_json(200, {"ok": True})
        else:
            self._send_json(404, {"error": "not found"})

    def do_POST(self):
        split = urllib.parse.urlsplit(self.path)
        if split.path != "/transcribe":
            self._send_json(404, {"error": "not found"})
            return

        length = int(self.headers.get("Content-Length", "0"))
        if length <= 0:
            self._send_json(400, {"error": "empty body"})
            return
        wav_bytes = self.rfile.read(length)

        # Tail of the transcript so far this recording (see
        # pipeline.rs::run_recording) — each chunk is otherwise
        # transcribed cold with no idea what was just said, which hurts
        # continuity/spelling at chunk boundaries. Absent on the first
        # chunk of a recording.
        prompt = urllib.parse.parse_qs(split.query).get("prompt", [None])[0]

        # faster-whisper wants a path or file-like object it can seek on;
        # a real temp file is the simplest thing that's definitely
        # seekable (an in-memory BytesIO works too, but a temp file also
        # makes a failed chunk trivially inspectable during debugging).
        try:
            with tempfile.NamedTemporaryFile(suffix=".wav") as tmp:
                tmp.write(wav_bytes)
                tmp.flush()
                # Model isn't documented thread-safe for concurrent
                # .transcribe() calls; og-voice only ever has one chunk
                # in flight at a time anyway (chunks are sent
                # sequentially, not pipelined), so this lock is just
                # cheap insurance against a future caller that isn't.
                with _model_lock:
                    # vad_filter: each ~1.5s chunk often ends in a sliver
                    # of trailing silence (mic latency, natural pause
                    # before the next chunk), and Whisper is well known
                    # to hallucinate filler words — "you", "you.",
                    # "Thank you." — off of silence-only audio rather
                    # than emitting nothing. Silero VAD (bundled via
                    # onnxruntime, already a faster-whisper dependency)
                    # strips non-speech regions before they ever reach
                    # the model, which is the actual fix — trimming
                    # known hallucination strings after the fact would
                    # also eat a real "you" the user said.
                    segments, _info = _model.transcribe(
                        tmp.name,
                        beam_size=1,
                        vad_filter=True,
                        vad_parameters=dict(min_silence_duration_ms=300),
                        initial_prompt=prompt,
                    )
                    text = "".join(seg.text for seg in segments).strip()
            self._send_json(200, {"text": text})
        except Exception as e:  # noqa: BLE001 - want any failure reported, not crash the server
            self._send_json(500, {"error": str(e)})


def main():
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
