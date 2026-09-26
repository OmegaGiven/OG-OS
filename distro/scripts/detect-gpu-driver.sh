#!/usr/bin/env bash
# Detects installed GPU(s) via PCI ID and recommends (optionally installs)
# the right driver package — the actual gap this fills: BASE-PACKAGES.md's
# "GPU / hardware breadth" section ships mesa/vulkan-radeon/vulkan-intel/
# vulkan-nouveau for everyone, which is correct and sufficient for AMD and
# Intel (mesa's open stack is *the* driver there, nothing to choose) but
# leaves Nvidia on `nouveau` only — the open Nvidia driver, which is a
# genuinely worse experience (no power management, capped clocks, no
# CUDA/NVENC) than the proprietary driver most Nvidia users actually want.
# This script is the "first boot, tell a new user which package to grab"
# step referenced in OS-PLAN.md phase 4 (Install script) — usable
# standalone today, not only inside an ISO.
#
# Usage:
#   detect-gpu-driver.sh            # detect + print recommendation only
#   detect-gpu-driver.sh --install  # detect + pacman -S the recommended package (asks to confirm)
set -euo pipefail

DO_INSTALL=false
if [[ "${1:-}" == "--install" ]]; then
    DO_INSTALL=true
fi

if ! command -v lspci >/dev/null 2>&1; then
    echo "lspci not found (pciutils not installed) — can't detect GPU hardware." >&2
    exit 1
fi

# One line per display controller, e.g.:
#   25:00.0 VGA compatible controller [0300]: Advanced Micro Devices, Inc. [AMD/ATI] Navi 23 [Radeon RX 6600/6600 XT/6600M] [1002:73ff] (rev c1)
mapfile -t GPU_LINES < <(lspci -nn -d ::0300; lspci -nn -d ::0302)

if [[ ${#GPU_LINES[@]} -eq 0 ]]; then
    echo "No display controller found via lspci — nothing to recommend."
    exit 0
fi

# Nvidia chip-codename prefix -> architecture generation, per the current
# Arch wiki NVIDIA driver support matrix. lspci prints the codename itself
# (e.g. "GA104") ahead of the marketing name in brackets, which is a far
# more reliable signal than parsing "GeForce RTX ..." strings by hand.
nvidia_recommend() {
    local codename="$1"
    case "$codename" in
        NV*|G7*|G8*|G9*|GT2*)
            echo "nouveau-only:Tesla/pre-Fermi — no supported proprietary driver left; nouveau is the only option."
            ;;
        GF*)
            echo "aur:nvidia-390xx-dkms:Fermi"
            ;;
        GK*)
            echo "aur:nvidia-470xx-dkms:Kepler"
            ;;
        GM*|GP*|GV*)
            echo "repo:nvidia-dkms:Maxwell/Pascal/Volta (proprietary only — nvidia-open doesn't cover this generation yet)"
            ;;
        TU*|GA*|AD*|GB*)
            echo "repo:nvidia-open-dkms:Turing or newer (open kernel module driver, Nvidia's own recommended default now)"
            ;;
        *)
            echo "unknown:Couldn't map codename '$codename' to a generation — check https://wiki.archlinux.org/title/NVIDIA for this card."
            ;;
    esac
}

RECOMMENDATIONS=()

for line in "${GPU_LINES[@]}"; do
    vendor_id=$(grep -oP '\[\K[0-9a-f]{4}(?=:[0-9a-f]{4}\])' <<<"$line" | head -1)
    desc=$(sed -E 's/^[0-9a-f:.]+ [^:]+: //' <<<"$line")

    case "$vendor_id" in
        1002) # AMD/ATI
            echo "AMD GPU detected: $desc"
            echo "  -> mesa + vulkan-radeon (already in base) covers this — no extra driver needed."
            ;;
        8086) # Intel
            echo "Intel GPU detected: $desc"
            echo "  -> mesa + vulkan-intel + intel-media-driver (already in base) covers this — no extra driver needed."
            ;;
        10de) # Nvidia
            codename=$(grep -oP '(?<=\[AMD/ATI\]|\[Nvidia\]|NVIDIA Corporation )\S+' <<<"$desc" | head -1)
            # Fallback: first bare alnum token right after the vendor name,
            # since lspci's exact bracket layout varies by pciutils version.
            if [[ -z "$codename" ]]; then
                codename=$(sed -E 's/.*NVIDIA Corporation //' <<<"$desc" | awk '{print $1}')
            fi
            echo "Nvidia GPU detected: $desc"
            echo "  chip codename guess: ${codename:-unknown}"
            rec=$(nvidia_recommend "${codename:-unknown}")
            kind="${rec%%:*}"
            rest="${rec#*:}"
            case "$kind" in
                repo)
                    pkg="${rest%%:*}"
                    note="${rest#*:}"
                    echo "  -> recommended: $pkg ($note)"
                    echo "     install: sudo pacman -S $pkg"
                    RECOMMENDATIONS+=("pacman:$pkg")
                    ;;
                aur)
                    pkg="${rest%%:*}"
                    note="${rest#*:}"
                    echo "  -> recommended: $pkg (AUR, legacy — $note)"
                    echo "     install: yay -S $pkg"
                    RECOMMENDATIONS+=("aur:$pkg")
                    ;;
                nouveau-only)
                    echo "  -> $rest"
                    ;;
                unknown)
                    echo "  -> $rest"
                    ;;
            esac
            ;;
        *)
            echo "Unrecognized GPU vendor ($vendor_id): $desc"
            ;;
    esac
done

if ! $DO_INSTALL || [[ ${#RECOMMENDATIONS[@]} -eq 0 ]]; then
    exit 0
fi

echo
for rec in "${RECOMMENDATIONS[@]}"; do
    method="${rec%%:*}"
    pkg="${rec#*:}"
    read -rp "Install $pkg now via $method? [y/N] " reply
    if [[ "$reply" =~ ^[Yy]$ ]]; then
        if [[ "$method" == "pacman" ]]; then
            sudo pacman -S --needed "$pkg"
        else
            yay -S --needed "$pkg"
        fi
    fi
done
