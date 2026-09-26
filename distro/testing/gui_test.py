#!/usr/bin/env python3
"""Reusable click-through testing library for og-os live-session GUI
testing (Calamares, or any other graphical app running in the live sway
session) via qemu + SSH + ydotool.

Why this exists
----------------
Driving a Wayland compositor's GUI from outside a VM is fragile if you
synthesize input at the QEMU-monitor level (sendkey/mouse_move over the
VNC/QMP connection) — coordinate systems, focus routing, and absolute-
vs-relative pointer semantics all have to line up, and debugging *why*
a click didn't land is mostly guesswork from screenshots.

The reliable alternative: install `ydotool` *inside* the guest and
drive it over SSH. ydotool injects input at the kernel uinput level, so
it looks like real hardware to sway — no VNC/QEMU coordinate
translation involved.

THE REAL BUG, finally found (this took a long time to nail down)
-------------------------------------------------------------------
`ydotoold`'s virtual device, as started by default, has **no absolute
(EV_ABS) capability at all** — confirmed via `/proc/bus/input/devices`,
it's `REL`-only. `ydotoold`'s own `--touch-on`/`-T` flag is supposed to
add `EV_ABS`, but in practice it made the daemon fail to create *any*
device at all (no socket, nothing) — a dead end, not a fix.

So `ydotool mousemove --absolute` was never operating on a real
absolute device to begin with. What actually determines pointer
position is **relative motion run through libinput's pointer
acceleration curve** — and the default `adaptive` accel profile
distorts movement nonlinearly (small moves are close to 1:1, large
jumps land well past where you'd expect). That's what caused every
earlier theory (halved coordinates, 2x scaling, absolute-device output
mapping) to look plausible for a few data points and then fall apart:
the actual distortion isn't a fixed ratio, it's velocity-dependent.

The fix, confirmed live and reproducible:
  1. `swaymsg input <ydotool-device-identifier> accel_profile flat`
  2. `swaymsg input <ydotool-device-identifier> pointer_accel 0`
  3. Always move via a two-step RELATIVE sequence: first slam far
     off-screen (e.g. -5000,-5000) to clamp the cursor to a known
     origin (0,0), then send a plain relative move of exactly
     (target_x, target_y) — this now lands within a couple of pixels
     of the true target, reliably, across repeated calls.

Confirmed by: closing a window via its titlebar × button (proves the
click mechanism itself works once positioning is exact), then
successfully clicking through Calamares' Welcome → Location → Keyboard
pages using the identical mechanism. Before this fix, clicks would
often *look* right in a screenshot (cursor appears to overlap the
button) but still miss — small acceletation-curve error was enough to
miss a tight Qt button hit-box while still being "close enough" to
register at the coarser window-focus level.

Ground truth over pixel-guessing
---------------------------------
Still true and still useful even with clicks now working reliably:
  - sway_tree() / find_window() — real window geometry + focus state
    (via `swaymsg -t get_tree`) instead of guessing positions from a
    screenshot.
  - tail_log() / wait_for_log() — if the app being tested writes a log
    (Calamares does, when launched with stdout redirected to a file),
    poll that instead of diffing screenshots to know when a page
    transition / job actually happened. (In practice, Calamares only
    logs *some* page transitions, not all — screenshots are still the
    most complete ground truth for "what page are we on".)

Usage
-----
    from gui_test import GuestSession

    g = GuestSession(ssh_port=22223, password="ogos")
    g.wait_for_ssh()
    g.install_ydotool()          # one-time per boot, not baked into the ISO
    g.calibrate_pointer()        # sets accel_profile flat + pointer_accel 0
    g.login_greeter("ogos")      # types password + Enter at the lightdm greeter
    g.wait_for_sway_socket()

    win = g.wait_for_window("io.calamares.calamares")
    x, y = g.calamares_next_button(win)
    g.click(x, y)
"""
from __future__ import annotations

import json
import subprocess
import time
from dataclasses import dataclass
from typing import Optional


@dataclass
class GuestSession:
    ssh_port: int
    password: str
    user: str = "liveuser"
    host: str = "localhost"
    ydotool_socket: str = "/tmp/.ydotool_socket"
    _sway_socket: Optional[str] = None

    # --- low-level SSH ---

    def ssh(self, remote_cmd: str, timeout: int = 30) -> subprocess.CompletedProcess:
        cmd = [
            "sshpass", "-p", self.password,
            "ssh", "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-p", str(self.ssh_port),
            f"{self.user}@{self.host}",
            remote_cmd,
        ]
        return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)

    def wait_for_ssh(self, timeout: int = 120) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                r = self.ssh("echo ok", timeout=5)
                if r.returncode == 0 and "ok" in r.stdout:
                    return
            except subprocess.TimeoutExpired:
                pass
            time.sleep(2)
        raise TimeoutError("SSH never came up")

    # --- one-time-per-boot setup ---

    def install_ydotool(self) -> None:
        """ydotool is a testing-session dependency, not something baked
        into the production ISO — install it fresh each boot. Do NOT
        pass -T/--touch-on to ydotoold: it makes the daemon fail to
        create any device at all in this environment (see module
        docstring) — the plain REL-only device plus calibrate_pointer()
        is the combination that actually works."""
        # liveuser has passwordless sudo baked into the ISO; a real
        # installed user doesn't -- sudo -S with the login password
        # piped in works for both, so always use it rather than bare
        # `sudo` (which hangs forever waiting on a tty that isn't there
        # over a non-interactive ssh command).
        sudo = f"echo {self.password} | sudo -S -p ''"
        self.ssh(f"{sudo} pacman -Sy --noconfirm ydotool", timeout=60)
        self.ssh(f"{sudo} usermod -aG input {self.user}")
        self.ssh(
            f"{sudo} setsid ydotoold < /dev/null > /tmp/ydotoold.log 2>&1 & disown; "
            f"sleep 1; {sudo} chmod 666 {self.ydotool_socket}"
        )

    def wait_for_sway_socket(self, timeout: int = 60) -> str:
        deadline = time.time() + timeout
        while time.time() < deadline:
            r = self.ssh("ls /run/user/1000/sway-ipc.* 2>&1")
            if r.returncode == 0 and "sway-ipc" in r.stdout:
                self._sway_socket = r.stdout.strip().split("\n")[0]
                return self._sway_socket
            time.sleep(2)
        raise TimeoutError("sway socket never appeared — did login succeed?")

    @property
    def sway_socket(self) -> str:
        if not self._sway_socket:
            self.wait_for_sway_socket()
        return self._sway_socket  # type: ignore[return-value]

    def calibrate_pointer(self) -> None:
        """THE fix (see module docstring): sway's default libinput
        accel_profile ("adaptive") distorts ydotool's relative pointer
        motion nonlinearly, causing clicks to land close to but not
        exactly on target — close enough to fool a screenshot check,
        not close enough to hit a tight Qt button. Setting the profile
        to "flat" with zero acceleration makes relative motion land
        within a couple of pixels of the requested delta, reliably.
        Call this once, after wait_for_sway_socket(), before any
        click()/move() calls you intend to trust.
        """
        r = self.ssh(f"SWAYSOCK={self.sway_socket} swaymsg -t get_inputs")
        text = r.stdout
        start = text.find("[")
        devices = json.loads(text[start:])
        ydotool_ids = {
            d["identifier"] for d in devices
            if "ydotool" in d.get("identifier", "").lower()
        }
        if not ydotool_ids:
            raise RuntimeError(
                "no ydotool input device found in `swaymsg -t get_inputs` — "
                "is ydotoold actually running and has a move/click happened "
                "yet? (the device may not register until first used)"
            )
        for ident in ydotool_ids:
            self.ssh(f"SWAYSOCK={self.sway_socket} swaymsg input '{ident}' accel_profile flat")
            self.ssh(f"SWAYSOCK={self.sway_socket} swaymsg input '{ident}' pointer_accel 0")

    # --- input injection ---

    def _ydotool(self, args: str) -> None:
        r = self.ssh(f"YDOTOOL_SOCKET={self.ydotool_socket} ydotool {args}")
        if r.returncode != 0:
            raise RuntimeError(f"ydotool {args} failed: {r.stderr}")

    def move(self, x: int, y: int) -> None:
        """Move the pointer to a REAL screen pixel coordinate (origin
        top-left of the whole output).

        Implementation: slam the cursor to (0,0) via a huge relative
        jump (clamps at the screen edge regardless of current
        position), then send one relative move of exactly (x, y).
        Requires calibrate_pointer() to have been called first, or
        this will be distorted by libinput's default acceleration
        curve — see module docstring for why.
        """
        cmd = (
            f"YDOTOOL_SOCKET={self.ydotool_socket} ydotool mousemove -x -5000 -y -5000 && "
            f"sleep 0.3 && "
            f"YDOTOOL_SOCKET={self.ydotool_socket} ydotool mousemove -x {x} -y {y}"
        )
        r = self.ssh(cmd)
        if r.returncode != 0:
            raise RuntimeError(f"move to ({x},{y}) failed: {r.stderr}")

    def click(self, x: int, y: int, button: str = "0xC0") -> None:
        """Move to (x, y) in real screen pixels, then click.
        button: 0xC0 = left click (down+up). See `ydotool click --help`
        for the full bitmask table (right/middle/etc)."""
        cmd = (
            f"YDOTOOL_SOCKET={self.ydotool_socket} ydotool mousemove -x -5000 -y -5000 && "
            f"sleep 0.3 && "
            f"YDOTOOL_SOCKET={self.ydotool_socket} ydotool mousemove -x {x} -y {y} && "
            f"sleep 0.3 && "
            f"YDOTOOL_SOCKET={self.ydotool_socket} ydotool click {button}"
        )
        r = self.ssh(cmd)
        if r.returncode != 0:
            raise RuntimeError(f"click at ({x},{y}) failed: {r.stderr}")

    def click_verified(self, x: int, y: int, monitor_port: int, out_path: str,
                        button: str = "0xC0") -> str:
        """click() then screenshot immediately after — always worth
        doing for anything that matters, since a wrong coordinate
        calibration (e.g. after a window resize) is otherwise silent.
        Returns the screenshot path for the caller (or a human) to
        inspect."""
        self.click(x, y, button)
        time.sleep(1)
        self.screenshot(monitor_port, out_path)
        return out_path

    def type_text(self, text: str) -> None:
        self._ydotool(f"type {text}")

    def key(self, *keycodes_and_state: str) -> None:
        """Raw key event pairs, e.g. key("28:1", "28:0") for Enter
        press+release. Look up keycodes in linux/input-event-codes.h."""
        self._ydotool("key " + " ".join(keycodes_and_state))

    def press_enter(self) -> None:
        self.key("28:1", "28:0")

    def login_greeter(self, password: str) -> None:
        """Types the password at lightdm's greeter (username is already
        selected — single-user live ISO) and hits Enter."""
        self.type_text(password)
        self.press_enter()

    # --- ground truth: window tree ---

    def sway_tree(self) -> dict:
        r = self.ssh(f"SWAYSOCK={self.sway_socket} swaymsg -t get_tree")
        # ssh may prepend a "Warning: Permanently added..." line on first
        # connection — skip to the first '{'.
        text = r.stdout
        start = text.find("{")
        return json.loads(text[start:])

    def find_window(self, app_id: str) -> Optional[dict]:
        """Returns the sway container node for a window by app_id,
        including its real on-screen rect and focus state — ground
        truth, not a guess from a screenshot."""
        def walk(n):
            if n.get("app_id") == app_id:
                return n
            for c in n.get("nodes", []) + n.get("floating_nodes", []):
                found = walk(c)
                if found:
                    return found
            return None
        return walk(self.sway_tree())

    def wait_for_window(self, app_id: str, timeout: int = 30) -> dict:
        deadline = time.time() + timeout
        while time.time() < deadline:
            w = self.find_window(app_id)
            if w:
                return w
            time.sleep(1)
        raise TimeoutError(f"window with app_id={app_id!r} never appeared")

    # --- ground truth: app log, not pixel-diffing ---

    def launch_logged(self, command: str, log_path: str = "/tmp/gui_test_app.log") -> None:
        """Launch a command in the live session with stdout/stderr
        redirected to a file you can poll with wait_for_log()."""
        self.ssh(
            f"sudo setsid sh -c "
            f"'XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 "
            f"QT_QPA_PLATFORM=wayland {command} > {log_path} 2>&1' "
            f"< /dev/null > /dev/null 2>&1 & disown"
        )

    def log_line_count(self, log_path: str) -> int:
        r = self.ssh(f"wc -l < {log_path} 2>&1")
        try:
            return int(r.stdout.strip().split()[0])
        except (ValueError, IndexError):
            return 0

    def tail_log(self, log_path: str, from_line: int = 0) -> str:
        r = self.ssh(f"tail -n +{from_line + 1} {log_path} 2>&1")
        return r.stdout

    def wait_for_log(self, log_path: str, pattern: str, from_line: int = 0,
                      timeout: int = 30) -> str:
        """Poll a log file until `pattern` appears in new lines, or
        raise TimeoutError. Returns the new log content."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            new_text = self.tail_log(log_path, from_line)
            if pattern in new_text:
                return new_text
            time.sleep(1)
        raise TimeoutError(f"{pattern!r} never appeared in {log_path}")

    # --- screenshots (for human review / debugging, not as ground truth) ---

    def screenshot(self, monitor_port: int, out_path: str) -> None:
        """Grab a screenshot via the qemu monitor for visual review.
        Use sway_tree()/wait_for_log() to actually *drive* a test —
        this is for producing evidence a human can look at."""
        import socket
        ppm_path = out_path + ".ppm"
        with socket.create_connection(("127.0.0.1", monitor_port), timeout=5) as s:
            s.recv(4096)  # banner
            s.sendall(f"screendump {ppm_path}\n".encode())
            time.sleep(1.5)
            s.recv(4096)
        subprocess.run(["convert", ppm_path, out_path], capture_output=True)

    # --- known widget-position helpers ---

    @staticmethod
    def calamares_next_button(window: dict) -> tuple[int, int]:
        """Calamares' Back/Next/Cancel row sits at a FIXED pixel offset
        from the window's bottom-right corner — the button bar doesn't
        scale with window width, it's right-anchored with a constant
        margin (confirmed by measuring it in both a 640px-wide tiled
        window and a 1280px-wide full-screen window and finding the
        same ~146px-from-right / ~24px-from-bottom offset in both, not
        a proportional fraction). Use fixed offsets, not percentages,
        if you recalibrate this for a UI chrome change.
        """
        rect = window["rect"]
        x = rect["x"] + rect["width"] - 146
        y = rect["y"] + rect["height"] - 24
        return x, y
