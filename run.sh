#!/usr/bin/env bash
# MFK Run Script for Debian / Linux
# Launches the kernel in VirtualBox (default) or QEMU
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

DEFAULT_KERNEL="target/x86_64-mfk/debug/mfk-kernel"
KERNEL_PATH="$DEFAULT_KERNEL"
HYPERVISOR="--vbox"
NO_RUN=""
EXTRA_ARGS=()

# Support all permutations:
#   ./run.sh
#   ./run.sh --qemu
#   ./run.sh target/.../mfk-kernel
#   ./run.sh target/.../mfk-kernel --qemu
#   ./run.sh --qemu target/.../mfk-kernel
#   ./run.sh --no-run
for arg in "$@"; do
    case "$arg" in
        --qemu)
            HYPERVISOR="--qemu"
            ;;
        --vbox|--virtualbox)
            HYPERVISOR="--vbox"
            ;;
        --no-run)
            NO_RUN="--no-run"
            ;;
        --help|-h)
            echo "Usage: $0 [kernel_path] [--vbox|--qemu] [--no-run]"
            echo ""
            echo "Defaults:"
            echo "  kernel: $DEFAULT_KERNEL"
            echo "  hypervisor: --vbox (fallback --qemu if VirtualBox missing)"
            echo ""
            echo "Examples:"
            echo "  $0"
            echo "  $0 --qemu"
            echo "  $0 target/x86_64-mfk/release/mfk-kernel --vbox"
            echo "  $0 --no-run    # only create disk images"
            exit 0
            ;;
        *)
            # Treat as kernel path if it looks like a path/file
            if [[ -e "$arg" ]] || [[ "$arg" == *"/"* ]] || [[ "$arg" == *mfk-kernel* ]]; then
                KERNEL_PATH="$arg"
            else
                echo "WARNING: Unknown argument: $arg" >&2
                EXTRA_ARGS+=("$arg")
            fi
            ;;
    esac
done

# Auto-fallback: if user wants vbox but VBoxManage missing and qemu present, use qemu
# Skip if --no-run (no hypervisor needed to just create images)
if [[ -z "$NO_RUN" ]]; then
    if [[ "$HYPERVISOR" == "--vbox" ]] && ! command -v VBoxManage >/dev/null 2>&1; then
        if command -v qemu-system-x86_64 >/dev/null 2>&1; then
            echo "ℹ VBoxManage not found - falling back to QEMU. Install VirtualBox for bridged networking:"
            echo "    sudo apt-get install virtualbox virtualbox-dkms"
            HYPERVISOR="--qemu"
        fi
    fi

    # Validate hypervisor tools (skip for --no-run)
    if [[ "$HYPERVISOR" == "--qemu" ]]; then
        if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
            echo "ERROR: qemu-system-x86_64 not found" >&2
            echo "Install: sudo apt-get install qemu-system-x86 qemu-utils" >&2
            exit 1
        fi
    else
        if ! command -v VBoxManage >/dev/null 2>&1; then
            echo "ERROR: VBoxManage not found" >&2
            echo "Install VirtualBox:" >&2
            echo "  sudo apt-get install virtualbox virtualbox-dkms linux-headers-\$(uname -r)" >&2
            echo "Or run with QEMU: $0 --qemu" >&2
            exit 1
        fi
    fi
fi

# If --no-run, kernel may not exist yet (runner will create images after build) - warn only
# Otherwise require kernel binary
if [[ -z "$NO_RUN" ]]; then
    if [[ ! -f "$KERNEL_PATH" ]]; then
        echo "ERROR: Kernel binary not found at: $KERNEL_PATH" >&2
        echo "" >&2
        echo "Build it first:" >&2
        echo "  ./build.sh" >&2
        echo "  # or: cargo +nightly build -p mfk-kernel --target targets/x86_64-mfk.json -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem" >&2
        exit 1
    fi
else
    if [[ ! -f "$KERNEL_PATH" ]]; then
        echo "WARNING: Kernel not found at $KERNEL_PATH - will fail if not built" >&2
    fi
fi

HV_DISPLAY="${HYPERVISOR#--}"
echo "Running MFK Kernel..."
echo "  Kernel: $KERNEL_PATH"
echo "  Hypervisor: $HV_DISPLAY"
if [[ -n "$NO_RUN" ]]; then
    echo "  Mode: --no-run (create images only)"
fi
echo ""

# Build runner if missing (helps fresh clones where build.sh wasn't run)
if [[ ! -f "target/release/mfk-runner" ]]; then
    echo "ℹ Runner not found at target/release/mfk-runner - building..."
    if ! cargo build -p mfk-runner --release; then
        echo "ERROR: Failed to build runner" >&2
        exit 1
    fi
fi

RUN_ARGS=("$KERNEL_PATH" "$HYPERVISOR")
if [[ -n "$NO_RUN" ]]; then
    RUN_ARGS+=("$NO_RUN")
fi
if [[ ${#EXTRA_ARGS[@]} -gt 0 ]]; then
    RUN_ARGS+=("${EXTRA_ARGS[@]}")
fi

# Use cargo run for convenience (rebuilds runner if needed) - on Debian host toolchain
exec cargo run -p mfk-runner --release -- "${RUN_ARGS[@]}"
