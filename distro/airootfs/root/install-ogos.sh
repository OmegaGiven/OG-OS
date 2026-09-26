#!/usr/bin/env bash
# OG-OS installer — phase 4 of OS-PLAN.md. Partitions a disk, pacstraps
# base + the OG-suite (via the [ogos] repo baked onto this ISO at
# /root/ogos-repo — not the build machine's local-repo/, which won't
# exist on the installed target), creates the first sudo user, installs
# systemd-boot, and seeds a default sway session. Run from the live
# session as root (liveuser has NOPASSWD sudo).
#
# Usage:
#   install-ogos.sh                      interactive prompts
#   install-ogos.sh --disk /dev/sda --hostname ogos-box --user alice \
#                    --password hunter2 --yes
#                                         unattended (all flags required
#                                         together, --yes skips the
#                                         final destructive-confirm)
#
# --password/--luks-password on the command line are visible in
# `ps`/shell history — fine for throwaway qemu testing, not for anything
# you'd actually keep.
set -euo pipefail

DISK=""
HOSTNAME_VAL=""
USERNAME=""
PASSWORD=""
ASSUME_YES=0
FS_TYPE="ext4"
KEYMAP=""
ENCRYPT=0
LUKS_PASSWORD=""

usage() {
    cat <<'EOF'
Usage: install-ogos.sh [--disk /dev/sdX] [--hostname NAME] [--user NAME]
                        [--password PASS] [--fs ext4|btrfs]
                        [--keymap NAME] [--encrypt] [--luks-password PASS]
                        [--yes]

Omit any of --disk/--hostname/--user/--password and you'll be prompted
for it interactively. --yes skips the final "this will erase X" confirm.
--keymap takes a console keymap name (`localectl list-keymaps` for the
full list on the live session) — also becomes the seeded sway session's
keyboard layout, and (with --encrypt) the layout active when typing the
disk-unlock passphrase at boot. --encrypt LUKS-encrypts the root
partition (ESP stays unencrypted, as it must to boot at all); combine
with --luks-password for unattended runs, or you'll be prompted.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --disk) DISK="$2"; shift 2 ;;
        --hostname) HOSTNAME_VAL="$2"; shift 2 ;;
        --user) USERNAME="$2"; shift 2 ;;
        --password) PASSWORD="$2"; shift 2 ;;
        --fs) FS_TYPE="$2"; shift 2 ;;
        --keymap) KEYMAP="$2"; shift 2 ;;
        --encrypt) ENCRYPT=1; shift ;;
        --luks-password) LUKS_PASSWORD="$2"; ENCRYPT=1; shift 2 ;;
        --yes) ASSUME_YES=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown argument: $1" >&2; usage; exit 1 ;;
    esac
done

if [ "$(id -u)" -ne 0 ]; then
    echo "Must run as root (try: sudo $0)" >&2
    exit 1
fi

echo "== OG-OS installer =="

# Keyboard layout first, before anything else needs typing (passwords
# included) — and applied to *this* live session immediately via
# loadkeys, not just queued for the target, so a non-US layout actually
# helps entering the password/LUKS-passphrase prompts that follow.
if [ -z "$KEYMAP" ]; then
    echo
    echo "Keyboard layout [us]: (see 'localectl list-keymaps' for other options)"
    read -rp "> " KEYMAP
    KEYMAP="${KEYMAP:-us}"
fi
if ! loadkeys "$KEYMAP" 2>/dev/null; then
    echo "warning: unknown keymap '$KEYMAP', falling back to 'us'" >&2
    KEYMAP="us"
    loadkeys us
fi

if [ -z "$DISK" ]; then
    echo
    echo "Available disks:"
    lsblk -d -o NAME,SIZE,MODEL -n | sed 's#^#  /dev/#'
    echo
    read -rp "Target disk (e.g. /dev/sda): " DISK
fi
if [ ! -b "$DISK" ]; then
    echo "error: $DISK is not a block device" >&2
    exit 1
fi

if [ -z "$HOSTNAME_VAL" ]; then
    read -rp "Hostname [ogos]: " HOSTNAME_VAL
    HOSTNAME_VAL="${HOSTNAME_VAL:-ogos}"
fi

if [ -z "$USERNAME" ]; then
    read -rp "Username for the first sudo user: " USERNAME
fi
if [ -z "$USERNAME" ]; then
    echo "error: username can't be empty" >&2
    exit 1
fi

if [ -z "$PASSWORD" ]; then
    while true; do
        read -rsp "Password for $USERNAME: " PASSWORD
        echo
        read -rsp "Confirm password: " PASSWORD_CONFIRM
        echo
        [ "$PASSWORD" = "$PASSWORD_CONFIRM" ] && break
        echo "Passwords didn't match, try again."
    done
fi

case "$FS_TYPE" in
    ext4|btrfs) ;;
    *) echo "error: --fs must be ext4 or btrfs" >&2; exit 1 ;;
esac

if [ "$ENCRYPT" -eq 0 ] && [ -z "$LUKS_PASSWORD" ] && [ -t 0 ]; then
    echo
    read -rp "Encrypt the root partition with LUKS? [y/N] " ENCRYPT_IN
    case "$ENCRYPT_IN" in
        y|Y|yes|YES) ENCRYPT=1 ;;
    esac
fi
if [ "$ENCRYPT" -eq 1 ] && [ -z "$LUKS_PASSWORD" ]; then
    while true; do
        read -rsp "LUKS passphrase (separate from $USERNAME's login password): " LUKS_PASSWORD
        echo
        read -rsp "Confirm LUKS passphrase: " LUKS_PASSWORD_CONFIRM
        echo
        [ -n "$LUKS_PASSWORD" ] && [ "$LUKS_PASSWORD" = "$LUKS_PASSWORD_CONFIRM" ] && break
        echo "Empty, or didn't match — try again."
    done
fi

if [ "$ASSUME_YES" -ne 1 ]; then
    echo
    echo "!! This will ERASE ALL DATA on $DISK !!"
    echo "   Filesystem: $FS_TYPE   Encryption: $([ "$ENCRYPT" -eq 1 ] && echo "LUKS2" || echo "none")   Keyboard: $KEYMAP"
    read -rp "Type 'yes' to continue: " CONFIRM
    [ "$CONFIRM" = "yes" ] || { echo "Aborted."; exit 1; }
fi

# --- Partitioning ---
# GPT: 512MiB ESP (ef00) + rest as root.
echo "==> Partitioning $DISK"
sgdisk --zap-all "$DISK"
sgdisk -n1:0:+512MiB -t1:ef00 -c1:ESP "$DISK"
sgdisk -n2:0:0       -t2:8300 -c2:root "$DISK"
partprobe "$DISK"
sleep 2

# NVMe-style disks (/dev/nvme0n1) need a 'p' before the partition number;
# plain sdX/vdX disks don't.
if [[ "$DISK" =~ [0-9]$ ]]; then
    ESP_PART="${DISK}p1"
    ROOT_PART="${DISK}p2"
else
    ESP_PART="${DISK}1"
    ROOT_PART="${DISK}2"
fi

echo "==> Formatting"
mkfs.fat -F32 -n ESP "$ESP_PART"

# LUKS: the ESP itself is never encrypted (has to stay readable by UEFI
# firmware before any of this exists) — only the root partition. Its
# filesystem (ext4 or btrfs, subvolumes and all) lands on the *mapped*
# /dev/mapper/cryptroot device, not the raw partition, from here on.
FS_TARGET="$ROOT_PART"
CRYPTDEVICE_CMDLINE=""
if [ "$ENCRYPT" -eq 1 ]; then
    echo "==> Encrypting $ROOT_PART (LUKS2)"
    printf '%s' "$LUKS_PASSWORD" | cryptsetup luksFormat --type luks2 --batch-mode "$ROOT_PART" -
    printf '%s' "$LUKS_PASSWORD" | cryptsetup open "$ROOT_PART" cryptroot -
    FS_TARGET="/dev/mapper/cryptroot"
    LUKS_UUID="$(blkid -s UUID -o value "$ROOT_PART")"
    CRYPTDEVICE_CMDLINE="cryptdevice=UUID=$LUKS_UUID:cryptroot "
fi

if [ "$FS_TYPE" = "btrfs" ]; then
    mkfs.btrfs -f -L root "$FS_TARGET"

    # Subvolume layout (@ / @home / snapper's own .snapshots nested under
    # @, created later by `snapper create-config`) — a flat single-subvolume
    # btrfs filesystem works but gets none of the actual point of choosing
    # btrfs here: per-subvolume snapshots, and a root rollback that doesn't
    # also roll back /home's more recent, more valuable data.
    mount "$FS_TARGET" /mnt
    btrfs subvolume create /mnt/@
    btrfs subvolume create /mnt/@home
    umount /mnt

    MOUNT_OPTS="noatime,compress=zstd,space_cache=v2"
    mount -o "subvol=@,$MOUNT_OPTS" "$FS_TARGET" /mnt
    mkdir -p /mnt/home
    mount -o "subvol=@home,$MOUNT_OPTS" "$FS_TARGET" /mnt/home
else
    mkfs.ext4 -F -L root "$FS_TARGET"
    echo "==> Mounting"
    mount "$FS_TARGET" /mnt
fi

mkdir -p /mnt/boot
mount "$ESP_PART" /mnt/boot

# --- pacstrap ---
# Temporary install-time pacman.conf: the live system's own /etc/pacman.conf
# (stock core/extra) plus the [ogos] repo baked onto this ISO at
# /root/ogos-repo. This conf is only used to drive pacstrap itself — the
# target's /etc/pacman.conf comes from whatever the 'pacman' package ships
# by default, so there's nothing to strip afterward: /root/ogos-repo
# wouldn't exist on the installed disk anyway.
INSTALL_PACMAN_CONF="/root/pacman-install.conf"
cp /etc/pacman.conf "$INSTALL_PACMAN_CONF"
cat >> "$INSTALL_PACMAN_CONF" <<'EOF'

[ogos]
SigLevel = Optional TrustAll
Server = file:///root/ogos-repo
EOF

echo "==> pacstrap (this takes a while)"
pacstrap -K -C "$INSTALL_PACMAN_CONF" /mnt \
    base base-devel linux linux-firmware amd-ucode intel-ucode sudo efibootmgr \
    cryptsetup snapper \
    sway sway-contrib swaybg swayidle swaylock lightdm lightdm-gtk-greeter \
    polkit xorg-xwayland xdg-desktop-portal-wlr xdg-desktop-portal-gtk xdg-utils \
    og-bar og-settings og-search og-clip og-notif-center og-notify og-files \
    og-links og-apps og-scripts \
    ttf-nerd-fonts-symbols ttf-nerd-fonts-symbols-common wl-clipboard grim slurp \
    noto-fonts noto-fonts-emoji ttf-liberation \
    alacritty mako \
    pipewire-alsa pipewire-jack pipewire-pulse alsa-utils alsa-firmware sof-firmware \
    bluez bluez-utils iwd wireless_tools openssh tailscale wireguard-tools \
    mesa-utils vulkan-tools vulkan-intel vulkan-radeon vulkan-nouveau \
    libva-intel-driver intel-media-driver libva-utils \
    xf86-video-amdgpu xf86-video-ati xf86-video-nouveau \
    git nano vim htop btop unzip wget less brightnessctl smartmontools \
    zram-generator flatpak mpv \
    cups cups-pdf ghostscript gsfonts gutenprint \
    yay zsh

genfstab -U /mnt >> /mnt/etc/fstab

# pacstrap only pulls package files into /mnt — it does NOT carry over
# anything from this live ISO's own airootfs overlay, including the seed
# sway config at /etc/skel here. Without this, useradd -m on the target
# has nothing to copy and the new user gets a bare home dir (confirmed:
# sway silently falls back to its own stock default session).
mkdir -p /mnt/etc/skel/.config/sway
cp /etc/skel/.config/sway/config /mnt/etc/skel/.config/sway/config

# XKB layout for the graphical session. Console keymap names
# ("us"/"de"/"fr"/"se"/...) and XKB layout codes mostly agree for the
# common single-word ones, which is what this assumes — a compound
# console name like "uk" (XKB: "gb") or "de-latin1" (XKB: "de") won't
# translate cleanly here. Known, bounded limitation, not silently wrong:
# worth fixing properly (a real keymap->XKB table) if this ever needs to
# support those.
if [ "$KEYMAP" != "us" ]; then
    echo "input * xkb_layout \"$KEYMAP\"" >> /mnt/etc/skel/.config/sway/config
fi

# --- Target system config (arch-chroot) ---
echo "==> Configuring installed system"

arch-chroot /mnt /bin/bash <<CHROOT_EOF
set -euo pipefail

ln -sf /usr/share/zoneinfo/UTC /etc/localtime
hwclock --systohc

echo "en_US.UTF-8 UTF-8" >> /etc/locale.gen
locale-gen
echo "LANG=en_US.UTF-8" > /etc/locale.conf

echo "$HOSTNAME_VAL" > /etc/hostname
cat >> /etc/hosts <<HOSTS_EOF
127.0.0.1   localhost
::1         localhost
127.0.1.1   $HOSTNAME_VAL.localdomain $HOSTNAME_VAL
HOSTS_EOF

# Real, persistent [ogos] repo entry — unlike the throwaway
# pacman-install.conf above (file:///root/ogos-repo, an ISO-local path
# that doesn't exist once this is a real installed system), this is the
# one og-settings' Updates tab and any later `pacman -Syu` actually see.
# Without it, an installed system has zero update path for its own
# desktop suite: pacstrap only ever reads pacman-install.conf, and the
# target's own /etc/pacman.conf (pacman's stock default) has no [ogos]
# section at all unless something adds it here.
cat >> /etc/pacman.conf <<PACMAN_EOF

[ogos]
SigLevel = Optional TrustAll
Server = https://omegagiven.github.io/OG-os-repo/x86_64
PACMAN_EOF

# Console keymap — baked into the initramfs by the 'keymap' hook below,
# so it's active at the LUKS passphrase prompt too, not just after boot.
echo "KEYMAP=$KEYMAP" > /etc/vconsole.conf

if [ "$ENCRYPT" -eq 1 ]; then
    # 'encrypt' must sit after 'block' (needs the raw block devices
    # enumerated first) and before 'filesystems' (which is what actually
    # tries to mount the now-decrypted root) — inserting right after
    # 'block' satisfies both. 'keyboard'+'keymap' (stock defaults on this
    # base) are what make the passphrase prompt honor vconsole.conf above
    # instead of always being a raw US layout regardless of KEYMAP.
    sed -i 's/^HOOKS=(\(.*\)block \(.*\))$/HOOKS=(\1block encrypt \2)/' /etc/mkinitcpio.conf
fi
mkinitcpio -P

useradd -m -G wheel -s /usr/bin/zsh "$USERNAME"
echo "$USERNAME:$PASSWORD" | chpasswd
passwd -l root
sed -i 's/^# %wheel ALL=(ALL:ALL) ALL/%wheel ALL=(ALL:ALL) ALL/' /etc/sudoers

systemctl enable lightdm.service
systemctl enable iwd.service
systemctl enable bluetooth.service
systemctl enable systemd-networkd.service
systemctl enable systemd-resolved.service
ln -sf /usr/lib/systemd/system/graphical.target /etc/systemd/system/default.target

if [ "$FS_TYPE" = "btrfs" ]; then
    # Simple layout: snapper's own \`.snapshots\` lands as a subvolume
    # nested under @ (create-config makes it automatically) rather than a
    # separate top-level @snapshots — snapshots and rollback of individual
    # files/timelines both work fine; the one thing this doesn't survive
    # is a full \`btrfs subvolume delete @\` disaster, which a top-level
    # @snapshots would. Documented tradeoff, not an oversight.
    snapper -c root create-config /
    systemctl enable snapper-timeline.timer
    systemctl enable snapper-cleanup.timer

    # snap-pac (the usual answer to "snapshot before/after every pacman
    # transaction") is AUR-only — pulling it via yay from inside a chroot
    # with no build user set up is more fragile than it's worth. Same
    # outcome, hand-rolled: a pacman hook pair that just calls snapper
    # directly. This is the single most valuable piece of the whole
    # snapshot story — "the update that breaks something" is exactly what
    # you want a snapshot positioned right before.
    mkdir -p /etc/pacman.d/hooks
    cat > /etc/pacman.d/hooks/50-snapper-pre.hook <<'SNAPPER_PRE_EOF'
[Trigger]
Operation = Install
Operation = Upgrade
Operation = Remove
Type = Package
Target = *

[Action]
Description = Snapshotting root filesystem before pacman transaction (snapper)
When = PreTransaction
Exec = /usr/bin/snapper -c root create --type pre --print-number --cleanup-algorithm number --description "pacman transaction" --userdata "important=yes"
SNAPPER_PRE_EOF
    cat > /etc/pacman.d/hooks/50-snapper-post.hook <<'SNAPPER_POST_EOF'
[Trigger]
Operation = Install
Operation = Upgrade
Operation = Remove
Type = Package
Target = *

[Action]
Description = Snapshotting root filesystem after pacman transaction (snapper)
When = PostTransaction
Exec = /usr/bin/snapper -c root create --type post --cleanup-algorithm number --description "pacman transaction"
SNAPPER_POST_EOF
fi

bootctl --path=/boot install
cat > /boot/loader/loader.conf <<LOADER_EOF
default arch.conf
timeout 3
console-mode max
editor no
LOADER_EOF

mkdir -p /boot/loader/entries
if [ "$ENCRYPT" -eq 1 ]; then
    # Booting an encrypted root: the kernel needs cryptdevice= to find and
    # unlock the LUKS container before root= means anything, and root=
    # itself has to point at the *mapped* device, never the raw
    # (encrypted, unreadable-as-a-filesystem) partition or its PARTUUID.
    cat > /boot/loader/entries/arch.conf <<ENTRY_EOF
title   OG-OS
linux   /vmlinuz-linux
initrd  /amd-ucode.img
initrd  /intel-ucode.img
initrd  /initramfs-linux.img
options ${CRYPTDEVICE_CMDLINE}root=/dev/mapper/cryptroot rw
ENTRY_EOF
else
    ROOT_PARTUUID=\$(blkid -s PARTUUID -o value "$ROOT_PART")
    cat > /boot/loader/entries/arch.conf <<ENTRY_EOF
title   OG-OS
linux   /vmlinuz-linux
initrd  /amd-ucode.img
initrd  /intel-ucode.img
initrd  /initramfs-linux.img
options root=PARTUUID=\$ROOT_PARTUUID rw
ENTRY_EOF
fi
CHROOT_EOF

echo "==> Done. Unmount and reboot when ready:"
echo "    umount -R /mnt && reboot"
if [ "$ENCRYPT" -eq 1 ]; then
    echo "    (root is LUKS-encrypted — you'll be asked for the passphrase on every boot)"
fi
