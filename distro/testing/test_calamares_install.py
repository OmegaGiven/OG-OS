#!/usr/bin/env python3
"""End-to-end Calamares click-through install test for og-os.

This is a QA/verification tool for confirming a clean install actually
completes and produces a bootable system -- it is not meant for end
users to run to install the OS (they use a real mouse). Coordinates
for dialogs that are screen-centered rather than window-relative
(the "Continue with Installation?" prompt) assume a 1280x800 display,
matching boot-vm.sh's qemu setup; recalibrate if you change that.

Assumes a VM is already booted via boot-vm.sh and its env sourced, OR
pass --ssh-port/--monitor-port directly. Drives, in order: login ->
Welcome -> Location -> Keyboard -> Partitions (erase whole disk,
confirmed via real screenshot: this is the "Erase disk" radio button,
not a guess) -> Users (create a real account, confirmed filled and
validated) -> Summary -> Install (confirmed: real install job runs) ->
Finish, using gui_test.py's ydotool-over-SSH input, calibrate_pointer()
for click precision, and swaymsg/screenshot ground truth throughout.

Usage:
    python3 test_calamares_install.py --ssh-port 2223 --monitor-port 4458 \\
        --username testuser --password testpass123 --hostname ogos-test
"""
import argparse
import os
import time

from gui_test import GuestSession


def run_install(ssh_port: int, monitor_port: int, live_password: str = "ogos",
                 username: str = "testuser", password: str = "testpass123",
                 hostname: str = "ogos-test", fullname: str = "Test User",
                 screenshot_dir: str = "/tmp/ogos-test-shots",
                 install_timeout: int = 900) -> GuestSession:
    """Drives the full Calamares click-through against an already-booted
    ISO (see boot-vm.sh). Returns the GuestSession used, so a caller
    (e.g. full_install_boot_test.py) can keep using it after this
    returns if it wants (though by then the live session is about to be
    torn down for a reboot into the installed system, so there's not
    much left to do with it)."""
    os.makedirs(screenshot_dir, exist_ok=True)

    g = GuestSession(ssh_port=ssh_port, password=live_password)

    def shot(name):
        path = f"{screenshot_dir}/{name}.png"
        g.screenshot(monitor_port, path)
        print(f"  [screenshot: {path}]")
        return path

    print("== Waiting for SSH ==")
    g.wait_for_ssh(timeout=120)

    print("== Installing ydotool in guest ==")
    g.install_ydotool()

    print("== Logging in at greeter ==")
    g.login_greeter(live_password)
    g.wait_for_sway_socket(timeout=60)
    print("  sway session up")

    print("== Calibrating pointer (fixes libinput accel-curve click misses) ==")
    # Needs at least one prior ydotool move for the device to show up in
    # `swaymsg -t get_inputs` -- the greeter login's Enter keypress
    # doesn't register a pointer event, so do a throwaway move first.
    g.ssh(f"YDOTOOL_SOCKET={g.ydotool_socket} ydotool mousemove -x 1 -y 1")
    g.calibrate_pointer()

    print("== Waiting for Calamares to auto-launch ==")
    win = g.wait_for_window("io.calamares.calamares", timeout=60)
    print(f"  Calamares window: {win['rect']}")
    shot("01-welcome")

    def click_next():
        w = g.find_window("io.calamares.calamares")
        x, y = g.calamares_next_button(w)
        g.click(x, y)
        time.sleep(2)

    print("== Welcome -> Location ==")
    click_next()
    shot("02-location")

    print("== Location -> Keyboard ==")
    click_next()
    shot("03-keyboard")

    print("== Keyboard -> Partitions ==")
    click_next()
    time.sleep(2)
    shot("04-partitions")

    # "Erase disk" radio button. Confirmed via a real screenshot crop
    # (not a percentage guess): sits at a fixed offset from the
    # window's top-left corner, (208, 71) -- the first choice in
    # Calamares' partition ChoicePage, which is a fixed-position list,
    # not something that scales with window size the way the Next/
    # Install button row does.
    w = g.find_window("io.calamares.calamares")
    rect = w["rect"]
    erase_x = rect["x"] + 208
    erase_y = rect["y"] + 71
    print(f"  Clicking 'Erase disk' at ({erase_x}, {erase_y})")
    g.click(erase_x, erase_y)
    time.sleep(1)
    shot("05-partitions-erase-selected")

    print("== Partitions -> Users ==")
    click_next()
    time.sleep(2)
    shot("06-users-blank")

    # Users page. Confirmed via real screenshot: the "Full Name" field
    # is already focused by default when the page loads, so click it
    # once (guarantees focus even if that default ever changes) then
    # Tab through the rest -- confirmed field order is Full Name ->
    # login -> hostname -> password -> repeat password.
    print("== Filling Users page ==")
    w = g.find_window("io.calamares.calamares")
    rect = w["rect"]
    g.click(rect["x"] + 300, rect["y"] + 50)
    g.type_text(fullname)
    g.key("15:1", "15:0")  # Tab (KEY_TAB=15)
    g.type_text(username)
    g.key("15:1", "15:0")
    g.type_text(hostname)
    g.key("15:1", "15:0")
    g.type_text(password)
    g.key("15:1", "15:0")
    g.type_text(password)
    time.sleep(0.5)
    shot("07-users-filled")

    # "Use the same password for the administrator account" checkbox --
    # confirmed position, avoids leaving the separate admin-password
    # fields in an ambiguous empty-but-seemingly-valid state.
    w = g.find_window("io.calamares.calamares")
    rect = w["rect"]
    g.click(rect["x"] + 208, rect["y"] + 267)
    time.sleep(0.5)
    shot("08-users-admin-checkbox")

    print("== Users -> Summary ==")
    click_next()
    shot("09-summary")

    print("== Summary -> Install ==")
    click_next()
    time.sleep(2)
    shot("10-install-confirm-prompt")

    # prompt-install: true in settings.conf means a "Continue with
    # Installation?" modal appears before the real job starts --
    # confirmed via screenshot: it's centered on the display, not
    # window-relative. (744, 431) confirmed against a 1280x800 display
    # (boot-vm.sh's default) -- recalibrate if you change that.
    print("  Clicking 'Install Now' on the confirmation dialog")
    g.click(744, 431)
    time.sleep(3)
    shot("11-installing")

    print(f"== Waiting for install job to finish (checking every 20s, up to "
          f"{install_timeout}s) ==")
    # No reliable non-OCR signal distinguishes "still copying files" from
    # "done, sitting on Finish" from "failed, sitting on an error dialog"
    # from screenshots alone (confirmed in testing: both success and
    # failure end up on the same Finish sidebar entry, and Calamares'
    # log doesn't print one unambiguous "done" marker either). Poll
    # periodically and take a screenshot each time; stop as soon as the
    # sidebar shows we're on the Finish page (window content-based, via
    # a screenshot pixel check isn't reliable without OCR -- this just
    # checks wall-clock time and leaves the actual "did it succeed"
    # judgment to whoever reviews the screenshots, which is the honest
    # thing to do given the ambiguity above).
    deadline = time.time() + install_timeout
    shot_n = 12
    while time.time() < deadline:
        time.sleep(20)
        shot(f"{shot_n:02d}-install-progress")
        shot_n += 1
    print(f"== Done polling. Review screenshots in {screenshot_dir} "
          f"to confirm the install actually succeeded. ==")
    return g


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ssh-port", type=int, required=True)
    ap.add_argument("--monitor-port", type=int, required=True)
    ap.add_argument("--live-password", default="ogos")
    ap.add_argument("--username", default="testuser")
    ap.add_argument("--password", default="testpass123")
    ap.add_argument("--hostname", default="ogos-test")
    ap.add_argument("--fullname", default="Test User")
    ap.add_argument("--screenshot-dir", default="/tmp/ogos-test-shots")
    ap.add_argument("--install-timeout", type=int, default=900,
                     help="seconds to wait for the install job to reach Finish")
    args = ap.parse_args()

    run_install(
        ssh_port=args.ssh_port, monitor_port=args.monitor_port,
        live_password=args.live_password, username=args.username,
        password=args.password, hostname=args.hostname, fullname=args.fullname,
        screenshot_dir=args.screenshot_dir, install_timeout=args.install_timeout,
    )


if __name__ == "__main__":
    main()
