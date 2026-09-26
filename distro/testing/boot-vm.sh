#!/usr/bin/env bash
# Boots an og-os ISO in qemu, standardized for automated click-through
# testing (see gui_test.py). Prints a shell-sourceable block of env vars
# (VNC_PORT, MONITOR_PORT, SSH_PORT, QEMU_PID) other tooling can read.
#
# Usage:
#   ./boot-vm.sh [iso_path] [disk_size]
#
# Defaults to the newest built ISO in ../out/ and a fresh 20G scratch disk.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ISO="${1:-$(ls -t "$SCRIPT_DIR"/../out/*.iso | head -1)}"
DISK_SIZE="${2:-20G}"

WORK_DIR="$(mktemp -d /tmp/ogos-vm-XXXXXX)"
DISK="$WORK_DIR/disk.qcow2"
OVMF_CODE="$WORK_DIR/OVMF_CODE.4m.fd"
OVMF_VARS="$WORK_DIR/OVMF_VARS.4m.fd"

# Pick free-ish ports by hashing the work dir path — good enough to avoid
# collisions between concurrent test runs without a port-allocator.
PORT_BASE=$(( 20000 + (RANDOM % 5000) ))
VNC_DISPLAY=$(( PORT_BASE / 100 % 100 ))
MONITOR_PORT=$(( PORT_BASE + 1 ))
SSH_PORT=$(( PORT_BASE + 2 ))

qemu-img create -f qcow2 "$DISK" "$DISK_SIZE" > /dev/null
cp /usr/share/edk2/x64/OVMF_CODE.4m.fd "$OVMF_CODE"
cp /usr/share/edk2/x64/OVMF_VARS.4m.fd "$OVMF_VARS"

qemu-system-x86_64 \
  -enable-kvm -m 4096 -smp 4 \
  -machine q35 \
  -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
  -drive if=pflash,format=raw,file="$OVMF_VARS" \
  -drive if=virtio,format=qcow2,file="$DISK" \
  -cdrom "$ISO" \
  -boot d \
  -display none -vnc "127.0.0.1:$VNC_DISPLAY" \
  -monitor "telnet:127.0.0.1:$MONITOR_PORT,server,nowait" \
  -pidfile "$WORK_DIR/qemu.pid" \
  -netdev "user,id=net0,hostfwd=tcp::${SSH_PORT}-:22" -device virtio-net-pci,netdev=net0 \
  > "$WORK_DIR/qemu.log" 2>&1 &
disown
sleep 2

cat > "$WORK_DIR/env.sh" <<EOF
export OGOS_WORK_DIR="$WORK_DIR"
export OGOS_VNC_PORT=$(( 5900 + VNC_DISPLAY ))
export OGOS_MONITOR_PORT=$MONITOR_PORT
export OGOS_SSH_PORT=$SSH_PORT
export OGOS_QEMU_PID=$(cat "$WORK_DIR/qemu.pid")
EOF

echo "Booted. Work dir: $WORK_DIR"
echo "Source this to get env vars for gui_test.py:"
echo "  source $WORK_DIR/env.sh"
cat "$WORK_DIR/env.sh"
