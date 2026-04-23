#!/bin/bash

set -euo pipefail

KERNEL_PATH="${1:-target/x86_64-mfk/debug/mfk-kernel}"

cargo run -p mfk-runner --release -- "$KERNEL_PATH"
