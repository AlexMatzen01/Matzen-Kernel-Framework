#!/usr/bin/env bash
# Seed the loop-mounted exFAT image.
# Usage: seed_exfat.sh <mnt-dir> <bigpattern-file>
set -euo pipefail
MNT="${1:?mnt dir}"
PAT="${2:?bigpattern file}"
cp "$PAT" "$MNT/frag-big.bin"
mkdir -p "$MNT/docs and nested dirs"
echo 'exfat hello' > "$MNT/greeting.txt"
echo 'deep exfat content 456' > "$MNT/docs and nested dirs/This is a very long filename for exFAT testing.txt"
for i in $(seq 1 40); do
  head -c 262144 /dev/urandom > "$MNT/fill-$i.dat"
done
for i in 2 4 6 8 10 12 14 16 18 20; do
  rm "$MNT/fill-$i.dat"
done
cp "$PAT" "$MNT/fragmented.bin"
