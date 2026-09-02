#!/usr/bin/env bash
# MFK Run Script for Debian / Linux
# Fast launcher for BIOS/UEFI using QEMU or VirtualBox
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

DEFAULT_KERNEL="target/x86_64-mfk/debug/mfk-kernel"
BIOS_IMAGE="target/x86_64-mfk/debug/mfk-kernel-bios.img"
UEFI_IMAGE="target/x86_64-mfk/debug/mfk-kernel-uefi.img"

KERNEL_PATH="$DEFAULT_KERNEL"
HYPERVISOR="--vbox"
FIRMWARE="--bios"
NO_RUN=""
EXTRA_ARGS=()

for arg in "$@"; do
    case "$arg" in
        --qemu)
            HYPERVISOR="--qemu"
            ;;
        --vbox|--virtualbox)
            HYPERVISOR="--vbox"
            ;;
        --uefi)
            FIRMWARE="--uefi"
            ;;
        --bios)
            FIRMWARE="--bios"
            ;;
        --no-run)
            NO_RUN="--no-run"
            ;;
        --help|-h)
            echo "Usage: $0 [kernel_path] [--vbox|--qemu] [--bios|--uefi] [--no-run]"
            echo ""
            echo "Defaults:"
            echo "  Hypervisor: --vbox"
            echo "  Firmware:   --bios"
            echo ""
            echo "Examples:"
            echo "  $0"
            echo "  $0 --qemu --bios"
            echo "  $0 --qemu --uefi"
            echo "  $0 --vbox --bios"
            echo "  $0 --vbox --uefi"
            exit 0
            ;;
        *)
            if [[ -e "$arg" || "$arg" == */* || "$arg" == *mfk-kernel* ]]; then
                KERNEL_PATH="$arg"
            else
                EXTRA_ARGS+=("$arg")
            fi
            ;;
    esac
done

# Select firmware image
if [[ "$FIRMWARE" == "--uefi" ]]; then
    FIRMWARE_IMAGE="$UEFI_IMAGE"
else
    FIRMWARE_IMAGE="$BIOS_IMAGE"
fi

# Validate kernel and firmware
if [[ -z "$NO_RUN" ]]; then
    [[ -f "$KERNEL_PATH" ]] || {
        echo "ERROR: Kernel not found: $KERNEL_PATH" >&2
        echo "Run ./build.sh first." >&2
        exit 1
    }

    [[ -f "$FIRMWARE_IMAGE" ]] || {
        echo "ERROR: Firmware image not found: $FIRMWARE_IMAGE" >&2
        exit 1
    }
fi

# Automatically use QEMU if VirtualBox isn't installed
if [[ -z "$NO_RUN" && "$HYPERVISOR" == "--vbox" ]]; then
    if ! command -v VBoxManage >/dev/null 2>&1 &&
       command -v qemu-system-x86_64 >/dev/null 2>&1; then
        echo "ℹ VirtualBox not found, using QEMU."
        HYPERVISOR="--qemu"
    fi
fi

# Validate hypervisor
if [[ -z "$NO_RUN" ]]; then
    if [[ "$HYPERVISOR" == "--qemu" ]]; then
        command -v qemu-system-x86_64 >/dev/null 2>&1 || {
            echo "ERROR: qemu-system-x86_64 not found." >&2
            echo "Install: sudo apt-get install qemu-system-x86 qemu-utils" >&2
            exit 1
        }
    else
        command -v VBoxManage >/dev/null 2>&1 || {
            echo "ERROR: VBoxManage not found." >&2
            exit 1
        }
    fi
fi

echo "Running MFK Kernel..."
echo "  Kernel:   $KERNEL_PATH"
echo "  Hypervisor: ${HYPERVISOR#--}"
echo "  Firmware: ${FIRMWARE#--}"
echo "  Image:    $FIRMWARE_IMAGE"
echo ""

# Build runner only if it doesn't exist.
if [[ ! -x "target/release/mfk-runner" ]]; then
    echo "Building MFK runner..."
    cargo build -p mfk-runner --release
fi

RUN_ARGS=(
    "$KERNEL_PATH"
    "$HYPERVISOR"
    "$FIRMWARE"
    "$FIRMWARE_IMAGE"
)

[[ -n "$NO_RUN" ]] && RUN_ARGS+=("$NO_RUN")

if [[ ${#EXTRA_ARGS[@]} -gt 0 ]]; then
    RUN_ARGS+=("${EXTRA_ARGS[@]}")
fi

# Execute runner directly instead of going through Cargo.
# This avoids Cargo startup/build-check overhead every boot.
exec ./target/release/mfk-runner "${RUN_ARGS[@]}"