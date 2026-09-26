#!/usr/bin/env python3
"""Full end-to-end og-os test: boot ISO -> click through Calamares install
-> power off -> reboot the installed disk (no cdrom) -> log in as the
new user at lightdm -> verify the seed sway session actually came up
(og-bar running, not a blank screen).

This exists to catch exactly the class of bug hit manually: install
completes, bootloader works, greeter login succeeds, but the resulting
session is blank because something in the users/skel/displaymanager
chain didn't wire up correctly. test_calamares_install.py alone only
proves the click-through and install job succeed -- it never reboots
into the result.

Usage:
    ./boot-vm.sh                       # prints a work dir + env vars
    source /tmp/ogos-vm-XXXXXX/env.sh
    python3 full_install_boot_test.py --ssh-port $OGOS_SSH_PORT \\
        --monitor-port $OGOS_MONITOR_PORT --work-dir $OGOS_WORK_DIR \\
        --qemu-pid $OGOS_QEMU_PID

Or, to also boot the ISO itself, just pass --iso and this script will
shell out to boot-vm.sh for you.
"""
import argparse
import os
import re
import signal
import subprocess
import sys
import time

from gui_test import GuestSession
from test_calamares_install import run_install

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))


def boot_iso(iso: str | None, disk_size: str) -> dict:
    cmd = [os.path.join(SCRIPT_DIR, "boot-vm.sh")]
    if iso:
        cmd.append(iso)
        cmd.append(disk_size)
    out = subprocess.run(cmd, capture_output=True, text=True, check=True).stdout
    env = {}
    for m in re.finditer(r'^export (OGOS_\w+)="?([^"\n]+)"?$', out, re.MULTILINE):
        env[m.group(1)] = m.group(2)
    if "OGOS_WORK_DIR" not in env:
        raise RuntimeError(f"couldn't parse boot-vm.sh output:\n{out}")
    return env


def boot_disk(work_dir: str) -> dict:
    cmd = [os.path.join(SCRIPT_DIR, "boot-vm-from-disk.sh"), work_dir]
    out = subprocess.run(cmd, capture_output=True, text=True, check=True).stdout
    env = {}
    for m in re.finditer(r'^export (OGOS_\w+)="?([^"\n]+)"?$', out, re.MULTILINE):
        env[m.group(1)] = m.group(2)
    if "OGOS_SSH_PORT" not in env:
        raise RuntimeError(f"couldn't parse boot-vm-from-disk.sh output:\n{out}")
    return env


def power_off_and_wait(qemu_pid: int, timeout: int = 60) -> None:
    """Kill the live-session qemu process cleanly and wait for it to
    actually exit -- the installed disk is still open/locked until it
    does, and starting a second qemu against the same qcow2 while the
    first still holds it just fails on a write-lock."""
    try:
        os.kill(qemu_pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            os.kill(qemu_pid, 0)
        except ProcessLookupError:
            return
        time.sleep(1)
    # still alive -- escalate
    try:
        os.kill(qemu_pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    time.sleep(2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--iso", help="ISO to boot; omit to reuse an already-booted VM")
    ap.add_argument("--disk-size", default="20G")
    ap.add_argument("--ssh-port", type=int, help="required if --iso omitted")
    ap.add_argument("--monitor-port", type=int, help="required if --iso omitted")
    ap.add_argument("--work-dir", help="required if --iso omitted")
    ap.add_argument("--qemu-pid", type=int, help="required if --iso omitted")
    ap.add_argument("--live-password", default="ogos")
    ap.add_argument("--username", default="testuser")
    ap.add_argument("--password", default="testpass123")
    ap.add_argument("--hostname", default="ogos-test")
    ap.add_argument("--fullname", default="Test User")
    ap.add_argument("--screenshot-dir", default="/tmp/ogos-test-shots")
    ap.add_argument("--install-timeout", type=int, default=900)
    ap.add_argument("--boot-timeout", type=int, default=180,
                     help="seconds to wait for the installed system's SSH to come up")
    args = ap.parse_args()

    if args.iso or not args.ssh_port:
        print("== Booting ISO ==")
        env = boot_iso(args.iso, args.disk_size)
        ssh_port = int(env["OGOS_SSH_PORT"])
        monitor_port = int(env["OGOS_MONITOR_PORT"])
        work_dir = env["OGOS_WORK_DIR"]
        qemu_pid = int(env["OGOS_QEMU_PID"])
    else:
        ssh_port = args.ssh_port
        monitor_port = args.monitor_port
        work_dir = args.work_dir
        qemu_pid = args.qemu_pid
        if not (work_dir and qemu_pid):
            print("need --work-dir and --qemu-pid when --ssh-port is passed "
                  "manually (used to reboot into the installed disk after)",
                  file=sys.stderr)
            sys.exit(1)

    print("== PHASE 1: install ==")
    run_install(
        ssh_port=ssh_port, monitor_port=monitor_port,
        live_password=args.live_password, username=args.username,
        password=args.password, hostname=args.hostname, fullname=args.fullname,
        screenshot_dir=args.screenshot_dir, install_timeout=args.install_timeout,
    )

    print("== PHASE 2: power off live session, reboot from installed disk ==")
    power_off_and_wait(qemu_pid)
    disk_env = boot_disk(work_dir)
    ssh_port2 = int(disk_env["OGOS_SSH_PORT"])
    monitor_port2 = int(disk_env["OGOS_MONITOR_PORT"])

    g = GuestSession(ssh_port=ssh_port2, password=args.password, user=args.username)

    print("== Waiting for installed system's SSH ==")
    g.wait_for_ssh(timeout=args.boot_timeout)

    print("== Installing ydotool in the installed system (test-only dependency) ==")
    g.install_ydotool()

    print("== Logging in as the new user at lightdm ==")
    g.login_greeter(args.password)
    g.wait_for_sway_socket(timeout=60)
    print("  sway session up")

    def shot(name):
        path = f"{args.screenshot_dir}/{name}.png"
        g.screenshot(monitor_port2, path)
        print(f"  [screenshot: {path}]")
        return path

    time.sleep(3)  # let exec_always/exec entries in the seed config finish spawning
    shot("20-post-install-session")

    print("== Verifying seed config actually applied ==")
    checks = {
        "sway config present": "test -f ~/.config/sway/config && echo YES || echo NO",
        "og-bar process running": "pgrep -x og-bar >/dev/null && echo YES || echo NO",
        "mako process running": "pgrep -x mako >/dev/null && echo YES || echo NO",
    }
    results = {}
    for label, cmd in checks.items():
        r = g.ssh(cmd)
        results[label] = r.stdout.strip()
        print(f"  {label}: {results[label]}")

    ok = all(v == "YES" for v in results.values())
    print()
    if ok:
        print("== PASS: installed system boots, logs in, and og-bar's seed session comes up. ==")
    else:
        print("== FAIL: one or more checks above came back NO -- see screenshot "
              f"{args.screenshot_dir}/20-post-install-session.png and re-run the "
              "diagnostic commands manually over SSH (same port) to dig further. ==")
        sys.exit(1)


if __name__ == "__main__":
    main()
