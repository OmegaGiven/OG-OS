#!/usr/bin/env bash
# Reboots an already-installed og-os disk (no -cdrom) -- used after
# test_calamares_install.py's install phase to verify the installed
# system actually boots and logs in correctly. Must reuse the SAME
# OVMF_VARS.4m.fd from the original boot-vm.sh work dir: that file is
# where efibootmgr/grub-install wrote the UEFI boot entry during
# install, a fresh OVMF_VARS would have no boot entry at all.
#
# Usage:
#   ./boot-vm-from-disk.sh <work_dir>
#
# <work_dir> is the OGOS_WORK_DIR printed by boot-vm.sh for the install
# run (contains disk.qcow2 + OVMF_VARS.4m.fd already written to).
set -euo pipefail

WORK_DIR="$1"
DISK="$WORK_DIR/disk.qcow2"
OVMF_CODE="$WORK_DIR/OVMF_CODE.4m.fd"
OVMF_VARS="$WORK_DIR/OVMF_VARS.4m.fd"

for f in "$DISK" "$OVMF_CODE" "$OVMF_VARS"; do
  [ -f "$f" ] || { echo "missing $f -- wrong work dir?" >&2; exit 1; }
done

PORT_BASE=$(( 20000 + (RANDOM % 5000) ))
VNC_DISPLAY=$(( PORT_BASE / 100 % 100 ))
MONITOR_PORT=$(( PORT_BASE + 1 ))
SSH_PORT=$(( PORT_BASE + 2 ))

qemu-system-x86_64 \
  -enable-kvm -m 4096 -smp 4 \
  -machine q35 \
  -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
  -drive if=pflash,format=raw,file="$OVMF_VARS" \
  -drive if=virtio,format=qcow2,file="$DISK" \
  -boot c \
  -display none -vnc "127.0.0.1:$VNC_DISPLAY" \
  -monitor "telnet:127.0.0.1:$MONITOR_PORT,server,nowait" \
  -pidfile "$WORK_DIR/qemu-disk.pid" \
  -netdev "user,id=net0,hostfwd=tcp::${SSH_PORT}-:22" -device virtio-net-pci,netdev=net0 \
  > "$WORK_DIR/qemu-disk.log" 2>&1 &
disown
sleep 2

cat > "$WORK_DIR/env-disk.sh" <<EOF
export OGOS_WORK_DIR="$WORK_DIR"
export OGOS_VNC_PORT=$(( 5900 + VNC_DISPLAY ))
export OGOS_MONITOR_PORT=$MONITOR_PORT
export OGOS_SSH_PORT=$SSH_PORT
export OGOS_QEMU_PID=$(cat "$WORK_DIR/qemu-disk.pid")
EOF

echo "Booted installed disk. Work dir: $WORK_DIR"
echo "Source this to get env vars for gui_test.py:"
echo "  source $WORK_DIR/env-disk.sh"
cat "$WORK_DIR/env-disk.sh"
