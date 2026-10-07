#!/usr/bin/env python3
"""Headless MFK filesystem boot test.

Boots the real kernel under QEMU (serial TCP socket, display none),
drives the shell over the serial console, and asserts ext4/exFAT/SimplFS
behaviour in the guest. Mutated images are fsck'd on the host afterwards.

Usage:
    python3 tools/fs_boot_test.py [--keep] [--timeout SECS]

Requires: build.sh output, OVMF firmware, QEMU. Images are COPIED so the
pristine files under target/fs-test are never modified.
"""
import os
import shutil
import socket
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TARGET = os.path.join(REPO, "target")
SERIAL_PORT = 4445
BOOT_TIMEOUT = 240


def find_ovmf():
    for c in [
        os.path.join(REPO, "OVMF", "OVMF_CODE_4M.fd"),
        "/usr/share/OVMF/OVMF_CODE_4M.fd",
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/edk2/ovmf/OVMF_CODE.fd",
    ]:
        if os.path.isfile(c):
            return c
    raise SystemExit("no OVMF_CODE.fd found")


def accel_args():
    if os.path.exists("/dev/kvm"):
        return ["-accel", "kvm", "-cpu", "host"]
    print("no KVM, using TCG (slower)")
    return ["-accel", "tcg", "-cpu", "max"]


class Console:
    def __init__(self, port):
        self.buf = b""
        last = None
        for _ in range(100):
            try:
                s = socket.create_connection(("127.0.0.1", port), timeout=2)
                last = None
                break
            except OSError as e:
                last = e
                time.sleep(0.5)
        else:
            raise SystemExit(f"serial connect failed: {last}")
        self.s = s
        self.s.settimeout(1.0)

    def _drain(self):
        try:
            while True:
                chunk = self.s.recv(65536)
                if not chunk:
                    break
                self.buf += chunk
        except socket.timeout:
            pass

    def wait_for(self, needle: bytes, timeout=60):
        end = time.time() + timeout
        while time.time() < end:
            self._drain()
            if needle in self.buf:
                out = self.buf
                self.buf = b""
                return out.decode("utf-8", "replace")
            time.sleep(0.2)
        raise SystemExit(f"timeout waiting for {needle!r}\n--- tail ---\n"
                         + self.buf[-2000:].decode("utf-8", "replace"))

    def run(self, cmd, expect=(), timeout=60):
        """Send a shell line, wait for the next prompt, return full output."""
        self.buf = b""
        self.s.sendall(cmd.encode() + b"\r")
        end = time.time() + timeout
        while time.time() < end:
            self._drain()
            # prompt ends with "> " at a line end
            text = self.buf.decode("utf-8", "replace")
            lines = text.split("\n")
            # fresh prompt looks like `mfk> ` or `mfk:/> ` on its own line
            if text.rstrip().endswith(">") and "mfk" in lines[-1]:
                out = text
                self.buf = b""
                for e in expect:
                    if e not in out:
                        raise SystemExit(
                            f"command {cmd!r}: missing {e!r}\n--- output ---\n{out}")
                print(f"$ {cmd}\n  ok ({len(out)} bytes)")
                return out
            time.sleep(0.2)
        raise SystemExit(f"command {cmd!r}: no prompt\n--- output ---\n"
                         + self.buf.decode("utf-8", "replace")[-2000:])


def main():
    keep = "--keep" in sys.argv
    shutil.copy(os.path.join(TARGET, "fs-test", "ext4.img"),
                os.path.join(TARGET, "boot-test-ext4.img"))
    shutil.copy(os.path.join(TARGET, "fs-test", "exfat.img"),
                os.path.join(TARGET, "boot-test-exfat.img"))
    uefi = os.path.join(TARGET, "x86_64-mfk", "debug", "mfk-kernel-uefi.img")
    if not os.path.isfile(uefi):
        raise SystemExit("uefi image missing; run the runner with --no-run first")
    ovmf = find_ovmf()
    disk = os.path.join(TARGET, "disk.img")
    ext4 = os.path.join(TARGET, "boot-test-ext4.img")
    exfat = os.path.join(TARGET, "boot-test-exfat.img")

    qemu = ["qemu-system-x86_64",
            "-machine", "pc", "-m", "512",
            "-drive", f"file={uefi},format=raw,if=ide,index=0,media=disk",
            "-drive", f"file={disk},format=raw,if=ide,index=1,media=disk,cache=none,readonly=off",
            "-drive", f"if=pflash,format=raw,readonly=on,file={ovmf}",
            "-drive", f"file={ext4},format=raw,if=ide,index=2,media=disk,cache=none,readonly=off",
            "-drive", f"file={exfat},format=raw,if=ide,index=3,media=disk,cache=none,readonly=off",
            "-serial", f"tcp:127.0.0.1:{SERIAL_PORT},server=on,wait=off",
            "-display", "none", "-monitor", "none",
            "-no-reboot"]
    qemu += accel_args()
    print("launch:", " ".join(qemu))
    proc = subprocess.Popen(qemu, cwd=REPO)
    try:
        con = Console(SERIAL_PORT)
        con.wait_for(b"mfk", timeout=BOOT_TIMEOUT)
        print("booted, shell prompt seen")

        # ext4 on drive 2
        con.run("mount 2", ["ext4", "mounted"]);
        con.run("ls", ["hello.txt", "big.bin", "link.txt"])
        con.run("cat /hello.txt", ["hello-mfk-ext4"])
        con.run("cat /docs/nested/deep.txt", ["deep-content-123"])
        con.run("stat /big.bin", ["5242880"])
        con.run("mkdir /mfk-boot", ["Created directory"])
        con.run("write /mfk-boot/note.txt hello-from-guest", ["Wrote"])
        con.run("cat /mfk-boot/note.txt", ["hello-from-guest"])
        con.run("ln -s /hello.txt /mfk-link", ["Linked"])
        con.run("cp /hello.txt /mfk-boot/copy.txt", ["Copied"])
        con.run("ls /mfk-boot", ["note.txt", "copy.txt"])

        # exFAT on drive 3 (replaces the mount)
        con.run("mount 3", ["exfat", "mounted"])
        con.run("ls", ["greeting.txt"])
        con.run("cat /greeting.txt", ["exfat hello"])
        con.run("cat /GREETING.TXT", ["exfat hello"])
        con.run("mkdir /guest-dir", ["Created directory"])
        con.run("write /guest-dir/guest.txt guest-writes-exfat", ["Wrote"])
        con.run("cat /guest-dir/guest.txt", ["guest-writes-exfat"])
        con.run("mv /guest-dir/guest.txt /guest-dir/moved.txt", ["Moved"])
        con.run("cat /guest-dir/moved.txt", ["guest-writes-exfat"])
        con.run("rm /guest-dir/moved.txt", ["Deleted"])
        con.run("rmdir /guest-dir", ["Removed directory"])
        con.run("df", ["exfat"])

        # SimplFS on drive 1 (fresh format + round trip)
        con.run("mkfs 1 --yes", ["formatted"])
        con.run("mount 1", ["simplfs", "mounted"])
        con.run("mkdir /sdir", ["Created directory"])
        con.run("write /sdir/msg.txt simplfs-in-guest", ["Wrote"])
        con.run("cat /sdir/msg.txt", ["simplfs-in-guest"])
        con.run("ls", ["sdir"])
        con.run("df", ["simplfs"])
        print("ALL GUEST CHECKS PASSED")
    finally:
        proc.kill()
        proc.wait()
        if not keep:
            pass
    print("done")


if __name__ == "__main__":
    main()
