#!/bin/bash

set -euo pipefail

KERNEL_PATH="${1:-target/x86_64-mfk/debug/mfk-kernel}"
HYPERVISOR="${2:---vbox}"

# Support both positional and flag arguments
if [[ "${2:-}" == "--qemu" || "${2:-}" == "--vbox" || "${2:-}" == "--virtualbox" ]]; then
    HYPERVISOR="$2"
fi

cargo run -p mfk-runner --release -- "$KERNEL_PATH" "$HYPERVISOR"
