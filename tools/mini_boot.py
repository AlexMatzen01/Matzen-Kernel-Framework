#!/usr/bin/env python3
"""Minimal exFAT guest repro: boot, mount 3, cat greeting. Serial to file."""
import os
import shutil
import socket
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TARGET = os.path.join(REPO, "target")
PORT = 4446
LOG = "/tmp/mini-serial.log"


def main():
    shutil.copy(os.path.join(TARGET, "fs-test", "exfat.img"),
                os.path.join(TARGET, "boot-mini-exfat.img"))
    uefi = os.path.join(TARGET, "x86_64-mfk", "debug", "mfk-kernel-uefi.img")
    ovmf = os.path.join(REPO, "OVMF", "OVMF_CODE_4M.fd")
    disk = os.path.join(TARGET, "disk.img")
    exfat = os.path.join(TARGET, "boot-mini-exfat.img")
    logf = open(LOG, "wb")

    qemu = ["qemu-system-x86_64",
            "-machine", "pc", "-m", "512",
            "-drive", f"file={uefi},format=raw,if=ide,index=0,media=disk",
            "-drive", f"file={disk},format=raw,if=ide,index=1,media=disk,cache=none,readonly=off",
            "-drive", f"if=pflash,format=raw,readonly=on,file={ovmf}",
            "-drive", f"file={exfat},format=raw,if=ide,index=3,media=disk,cache=none,readonly=off",
            "-serial", f"tcp:127.0.0.1:{PORT},server=on,wait=off",
            "-display", "none", "-monitor", "none",
            "-no-reboot", "-accel", "kvm", "-cpu", "host"]
    proc = subprocess.Popen(qemu, cwd=REPO)
    buf = b""
    try:
        s = None
        for _ in range(100):
            try:
                s = socket.create_connection(("127.0.0.1", PORT), timeout=2)
                break
            except OSError:
                time.sleep(0.5)
        s.settimeout(1.0)

        def drain():
            nonlocal buf
            try:
                while True:
                    c = s.recv(65536)
                    if not c:
                        break
                    buf += c
                    logf.write(c)
                    logf.flush()
            except socket.timeout:
                pass

        def wait_prompt(timeout=90):
            end = time.time() + timeout
            while time.time() < end:
                drain()
                t = buf.decode("utf-8", "replace")
                if t.rstrip().endswith(">") and "mfk" in t.split("\n")[-1]:
                    return t
                time.sleep(0.2)
            return None

        t = wait_prompt(180)
        print("boot prompt:", "YES" if t else "NO")
        if not t:
            return
        for cmd in ["mount 3", "stat /greeting.txt", "df", "cat /greeting.txt"]:
            buf = b""
            s.sendall(cmd.encode() + b"\r")
            t = wait_prompt(45)
            print(f"--- after {cmd!r}: {'PROMPT' if t else 'TIMEOUT'}")
            if t:
                print(t[-600:])
            else:
                print(buf.decode("utf-8", "replace")[-600:])
    finally:
        proc.kill()
        proc.wait()
        logf.close()


if __name__ == "__main__":
    main()
