#!/usr/bin/env python3
"""Per-step install timing from a Calamares session log.

Calamares (run as root in the live session) logs to
/root/.cache/calamares/session.log, one line per job start:

    2026-10-19 - 11:54:18 [6]:     Starting job "unpackfs" ( 11 / 32 )

and, once the Finished page is reached, a completion line from the
finished module:

    2026-10-19 - 11:55:59 [6]:     Notification not sent; completion: succeeded

A job's duration is the gap to the next job's start (the last job ends at
the completion line). Timestamps have 1 s resolution, so sub-second jobs
show as 0.

Usage:
    python3 calamares_timing.py session.log      # print the table
Or from Python: parse_log(text) -> InstallTiming, used by
test_calamares_install.py to wait for the install and report timings.
"""
from __future__ import annotations

import re
import sys
from dataclasses import dataclass, field
from datetime import datetime

_TS = r"(\d{4}-\d{2}-\d{2}) - (\d{2}:\d{2}:\d{2})"
JOB_RE = re.compile(_TS + r' \[\d+\]:\s+Starting job "(.*)" \( (\d+) / (\d+) \)')
DONE_RE = re.compile(_TS + r" \[\d+\]:\s+.*completion: (succeeded|failed)")
FAIL_RE = re.compile(_TS + r" \[\d+\]:\s+.*Installation failed", re.IGNORECASE)


def _ts(date: str, time_: str) -> datetime:
    return datetime.strptime(f"{date} {time_}", "%Y-%m-%d %H:%M:%S")


@dataclass
class InstallTiming:
    jobs: list[tuple[str, float]] = field(default_factory=list)  # (name, seconds)
    total_jobs: int = 0
    started: int = 0          # how many jobs have started so far
    finished: bool = False
    succeeded: bool = False
    total_seconds: float = 0.0

    def table(self) -> str:
        width = max([len(n) for n, _ in self.jobs] + [10])
        lines = [f"{'step':<{width}}  seconds", f"{'-' * width}  -------"]
        lines += [f"{n:<{width}}  {s:7.0f}" for n, s in self.jobs]
        lines.append(f"{'-' * width}  -------")
        state = ("succeeded" if self.succeeded else "FAILED") if self.finished \
            else f"running ({self.started}/{self.total_jobs} started)"
        lines.append(f"{'TOTAL (' + state + ')':<{width}}  {self.total_seconds:7.0f}")
        return "\n".join(lines)


def parse_log(text: str) -> InstallTiming:
    starts: list[tuple[datetime, str]] = []
    t = InstallTiming()
    end: datetime | None = None
    for line in text.splitlines():
        m = JOB_RE.match(line)
        if m:
            date, time_, name, idx, total = m.groups()
            if int(idx) == 1:
                # A new run (e.g. Calamares restarted) -- keep only the last.
                starts = []
                end = None
                t.finished = t.succeeded = False
            starts.append((_ts(date, time_), name))
            t.total_jobs = int(total)
            continue
        m = DONE_RE.match(line)
        if m and starts:
            end = _ts(m.group(1), m.group(2))
            t.finished = True
            t.succeeded = m.group(3) == "succeeded"
            continue
        m = FAIL_RE.match(line)
        if m and starts:
            end = _ts(m.group(1), m.group(2))
            t.finished = True
            t.succeeded = False
    t.started = len(starts)
    for i, (ts, name) in enumerate(starts):
        nxt = starts[i + 1][0] if i + 1 < len(starts) else end
        t.jobs.append((name, (nxt - ts).total_seconds() if nxt else 0.0))
    if starts:
        last = end or starts[-1][0]
        t.total_seconds = (last - starts[0][0]).total_seconds()
    return t


def main() -> None:
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(2)
    with open(sys.argv[1], encoding="utf-8", errors="replace") as f:
        print(parse_log(f.read()).table())


if __name__ == "__main__":
    main()
