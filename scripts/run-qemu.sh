#!/usr/bin/env bash
set -euo pipefail

ARTIFACT=${1:-target/x86_64-mfk/debug/bootimage-mfk-kernel.bin}
: "${QEMU:=qemu-system-x86_64}"

if [[ ! -f "${ARTIFACT}" ]]; then
  echo "Artifact ${ARTIFACT} not found. Run 'cargo bootimage -p mfk-kernel' first." >&2
  exit 1
fi

${QEMU} \
  -drive format=raw,file="${ARTIFACT}" \
  -serial stdio \
  -display none \
  -m 256M \
  -cpu qemu64 \
  -smp 2 \
  -no-reboot \
  -d int
