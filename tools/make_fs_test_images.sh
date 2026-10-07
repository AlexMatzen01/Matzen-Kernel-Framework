#!/usr/bin/env bash
# Build host-side filesystem test images for the MFK drivers.
#
# The MFK implementation itself never runs these tools; they only produce
# images that the native Rust drivers must then parse independently:
#
#   target/fs-test/ext4.img       64 MiB ext4 (no metadata_csum), seeded files
#   target/fs-test/exfat.img      64 MiB exFAT, seeded files
#   target/fs-test/big.bin        5 MiB deterministic pattern (ext4 content)
#   target/fs-test/bigpattern.bin 8 MiB deterministic pattern (exFAT content)
#
# Byte i of each pattern file is (base_offset + i) % 251, so a Rust test can
# regenerate the expectation without storing a second copy.
#
# Requires (Debian/WSL): e2fsprogs, exfatprogs. Loop-mounts need root.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT="$REPO_ROOT/target/fs-test"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$OUT"
cd "$WORK"

echo "--- pattern files ---"
python3 "$SCRIPT_DIR/gen_pattern.py" 5242880 big.bin
python3 "$SCRIPT_DIR/gen_pattern.py" 8388608 bigpattern.bin
cp big.bin bigpattern.bin "$OUT/"

echo "--- ext4.img ---"
dd if=/dev/zero of=ext4.img bs=1M count=64 status=none
mkfs.ext4 -F -q -O ^metadata_csum -b 4096 ext4.img
echo 'hello-mfk-ext4' > hello.txt
printf 'deep-content-123\n' > deep.txt
debugfs -w -R 'mkdir docs' ext4.img >/dev/null < /dev/null
debugfs -w -R 'mkdir docs/nested' ext4.img >/dev/null < /dev/null
debugfs -w -R 'write hello.txt hello.txt' ext4.img >/dev/null < /dev/null
debugfs -w -R 'write deep.txt docs/nested/deep.txt' ext4.img >/dev/null < /dev/null
debugfs -w -R 'write big.bin big.bin' ext4.img >/dev/null < /dev/null
# debugfs arg order is: symlink <link-name> <target>, ln <source> <dest>
# (stdin is /dev/null so batch tools can never block on a prompt)
debugfs -w -R 'symlink link.txt hello.txt' ext4.img >/dev/null < /dev/null
debugfs -w -R 'ln hello.txt hard.txt' ext4.img >/dev/null < /dev/null
# debugfs `ln` forgets to bump i_links_count; repair so the image is clean.
# (e2fsck -y exits 1 when it fixes something, which is expected here.)
e2fsck -y -f ext4.img >/dev/null 2>&1 < /dev/null || true
if ! e2fsck -n -f ext4.img < /dev/null; then
  echo "ext4 test image is not clean" >&2
  exit 1
fi
cp ext4.img "$OUT/ext4.img"

echo "--- exfat.img ---"
dd if=/dev/zero of=exfat.img bs=1M count=64 status=none
mkfs.exfat exfat.img >/dev/null
mkdir -p mnt
mount -o loop exfat.img mnt
bash "$SCRIPT_DIR/seed_exfat.sh" "$WORK/mnt" "$WORK/bigpattern.bin"
umount mnt
fsck.exfat exfat.img
cp exfat.img "$OUT/exfat.img"

echo "--- images in $OUT ---"
ls -la "$OUT"
