#!/usr/bin/env python3
"""MFK QEMU launch TUI - configure extra drives + launch options.

- Interactive terminal menu (rich-enhanced when `rich` is installed,
  plain stdlib fallback otherwise; a `textual` App is used when
  `--textual` is passed and textual is installed).
- Config auto-saves to .mfk-launch.json in the repo root on every change.
- Translates config into mfk-runner flags (repeatable --extra-disk).
- First 2 extras use IDE slots (indices 2-3); extras 3-8 attach as
  virtio-blk-pci devices (guest drive indices 4+, needs kernel virtio driver).

Usage:
    python tools/mfk_launch.py              # interactive menu
    python tools/mfk_launch.py --show-args  # print runner argv, no launch
    python tools/mfk_launch.py --launch     # launch with saved config
    python tools/mfk_launch.py --textual    # force textual App (if installed)
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
CONFIG_PATH = REPO_ROOT / ".mfk-launch.json"
MAX_EXTRA_IDE = 2
MAX_EXTRA_VIRTIO = 6
MAX_EXTRAS = MAX_EXTRA_IDE + MAX_EXTRA_VIRTIO
VALID_BUS = ("auto", "ide", "virtio")

DEFAULTS = {
    "kernel": "target/x86_64-mfk/debug/mfk-kernel",
    "hypervisor": "qemu",       # qemu | vbox | hyperv (hyperv = Windows, Gen2/UEFI)
    "firmware": "bios",         # bios | uefi (hyperv forces uefi)
    "ovmf_code": "OVMF/OVMF_CODE_4M.fd",
    "keyboard": "ps2",          # ps2 | ehci | xhci | uhci | ohci (QEMU only)
    "bundle_apps": False,
    "data_disk_size": "10M",
    "doom": False,                # fully automated Doom setup (WAD disk + attach)
    "doom_wad": "target/doom1.wad",      # shareware WAD cache (auto-downloaded)
    "doom_commercial": "",        # optional user WAD (DOOM.WAD / DOOM64.WAD)
    "doom_disk": "target/doom.img",
    "doom_disk_size": "128M",
    "doom_guest": "/wad/doom1.wad",
    "vnc": False,               # Enable QEMU VNC server (QEMU only)
    "vnc_port": 5900,           # VNC WebSocket port
    "web_ui": False,            # Launch noVNC web UI (implies vnc, QEMU only)
    "web_ui_port": 8084,        # Web UI HTTP port
    "gpu_passthrough": "",
    "gpu_audio": "",
    "gpu_rom": "",
    # Hyper-V Gen2 options (Windows only; empty = runner default).
    "hyperv_switch": "",        # "" = Default Switch (auto-detected)
    "hyperv_mem": 512,
    "hyperv_cpus": 2,
    "hyperv_boot": "dvd",       # dvd | disk
    "hyperv_vhdx": "",          # "" = auto (<uefi>.vhdx for disk boot)
    "hyperv_com": "",           # "" = default pipe, "off" = disable COM1
    "extras": [
        # {"path": "target/extra-disk.img", "size": "64M", "boot": False}
    ],
}

VALID_KBD = ("ps2", "ehci", "xhci", "uhci", "ohci")


def is_valid_pci_bdf(value: str) -> bool:
    value = value.strip().lower()
    if value.startswith("0000:"):
        value = value[5:]
    parts = value.split(":")
    if len(parts) != 2:
        return False
    bus, device_function = parts
    if len(bus) != 2 or "." not in device_function:
        return False
    device, function = device_function.split(".", 1)
    if len(device) != 2 or len(function) != 1:
        return False
    try:
        return (int(bus, 16) <= 0xFF
                and int(device, 16) <= 0x1F
                and int(function, 16) <= 7)
    except ValueError:
        return False


def is_wsl2() -> bool:
    if os.environ.get("WSL_DISTRO_NAME"):
        return True
    try:
        return "microsoft" in Path("/proc/version").read_text().lower()
    except OSError:
        return False


def normalize_gpu_config(cfg: dict) -> None:
    for key in ("gpu_passthrough", "gpu_audio", "gpu_rom"):
        value = str(cfg.get(key, "") or "").strip()
        if any(ord(char) < 32 or ord(char) == 127 for char in value):
            value = ""
        cfg[key] = value
    if cfg["gpu_passthrough"] and not is_valid_pci_bdf(cfg["gpu_passthrough"]):
        cfg["gpu_passthrough"] = ""
    if not cfg["gpu_passthrough"]:
        cfg["gpu_audio"] = ""
        cfg["gpu_rom"] = ""
    if cfg["gpu_audio"] and not is_valid_pci_bdf(cfg["gpu_audio"]):
        cfg["gpu_audio"] = ""
    if cfg["gpu_passthrough"]:
        cfg["hypervisor"] = "qemu"
        cfg["firmware"] = "uefi"
    if cfg["gpu_passthrough"] and is_wsl2():
        out("[yellow]GPU passthrough is configured, but WSL2 does not expose raw PCI passthrough.[/yellow]")
        out("[yellow]Use native Linux/VFIO or a Hyper-V Discrete Device Assignment VM for testing.[/yellow]")
    if cfg["gpu_passthrough"] and os.name == "nt":
        out("[yellow]GPU passthrough is configured, but native Windows does not support Linux vfio-pci passthrough.[/yellow]")
        out("[yellow]Use QEMU's emulated GPU on Windows, or assign the GPU to a Hyper-V VM with DDA.[/yellow]")


def gpu_passthrough_status(cfg: dict) -> str:
    if not cfg.get("gpu_passthrough"):
        return "OFF"
    if is_wsl2():
        return "BLOCKED (WSL2)"
    if os.name == "nt":
        return "BLOCKED (Windows)"
    return "ON"


# ---------------------------------------------------------------------------
# Doom WAD automation: commercial WAD if set, else shareware doom1.wad.
# ---------------------------------------------------------------------------

SHAREWARE_WAD_URLS = (
    # GitHub mirrors first (reliable hotlinking), id mirrors as fallback.
    "https://raw.githubusercontent.com/samrude005/doom1.wad/main/doom1.wad",
    "https://doomwiki.org/mediawiki/images/9/9d/Doom1.wad",
)
SHAREWARE_WAD_SHA256 = ""  # optional pin; empty = accept download as-is.


def resolve_doom_wad(cfg: dict) -> str:
    """Host WAD path for the Doom setup. Prefers commercial, else shareware.

    Downloads the shareware doom1.wad into doom_wad when neither exists.
    Returns "" when automation is off or nothing is available.
    """
    if not cfg.get("doom"):
        return ""
    commercial = str(cfg.get("doom_commercial", "") or "").strip()
    if commercial:
        p = Path(commercial).expanduser()
        if not p.is_absolute():
            p = REPO_ROOT / p
        if p.is_file():
            return str(p)
        out(f"[yellow]Doom commercial WAD not found: {p}; falling back to shareware.[/yellow]")
    wad = Path(str(cfg.get("doom_wad", "target/doom1.wad") or "target/doom1.wad"))
    if not wad.is_absolute():
        wad = REPO_ROOT / wad
    if wad.is_file() and wad.stat().st_size > 1_000_000:
        return str(wad)
    for url in SHAREWARE_WAD_URLS:
        try:
            out(f"[cyan]Downloading shareware doom1.wad from {url}...[/cyan]")
            import hashlib
            import urllib.request
            wad.parent.mkdir(parents=True, exist_ok=True)
            tmp = wad.with_suffix(".wad.download")
            with urllib.request.urlopen(url, timeout=60) as src, open(tmp, "wb") as dst:
                digest = hashlib.sha256()
                while True:
                    chunk = src.read(1 << 20)
                    if not chunk:
                        break
                    digest.update(chunk)
                    dst.write(chunk)
            if tmp.stat().st_size < 1_000_000:
                tmp.unlink(missing_ok=True)
                continue
            if SHAREWARE_WAD_SHA256 and digest.hexdigest() != SHAREWARE_WAD_SHA256:
                tmp.unlink(missing_ok=True)
                continue
            os.replace(tmp, wad)
            out(f"[green]Shareware WAD cached: {wad} ({wad.stat().st_size} bytes)[/green]")
            return str(wad)
        except Exception as e:
            out(f"[yellow]Shareware download failed ({url}): {e}[/yellow]")
    return ""


def doom_guest_path(cfg: dict, host_wad: str) -> str:
    guest = str(cfg.get("doom_guest", "") or "/wad/doom1.wad").strip() or "/wad/doom1.wad"
    commercial = str(cfg.get("doom_commercial", "") or "").strip()
    if (commercial and host_wad and guest == "/wad/doom1.wad"
            and os.path.basename(host_wad).lower() != "doom1.wad"):
        guest = "/wad/" + os.path.basename(host_wad)
    return guest


def find_runner_binary() -> Path | None:
    name = "mfk-runner.exe" if os.name == "nt" else "mfk-runner"
    for profile in ("release", "debug"):
        candidate = REPO_ROOT / "target" / profile / name
        if candidate.is_file():
            return candidate
    return None


def normalize_ovmf_config(cfg: dict) -> None:
    value = str(cfg.get("ovmf_code", "") or "").strip()
    if any(ord(char) < 32 or ord(char) == 127 for char in value):
        value = ""
    cfg["ovmf_code"] = value


def resolve_ovmf_path(value: str) -> Path:
    path = Path(value).expanduser()
    if not path.is_absolute():
        path = REPO_ROOT / path
    return path


def ovmf_status(cfg: dict) -> str:
    value = cfg.get("ovmf_code", "")
    if not value:
        return "AUTO"
    return "FOUND" if resolve_ovmf_path(value).is_file() else "MISSING"


try:
    from rich.console import Console
    from rich.table import Table
    from rich.panel import Panel
    RICH = True
    console = Console()
except ImportError:  # stdlib fallback
    RICH = False
    console = None


def out(msg=""):
    if RICH:
        console.print(msg)
    else:
        # strip rich markup fallback
        print(msg.replace("[bold]", "").replace("[/bold]", "")
                .replace("[cyan]", "").replace("[/cyan]", "")
                .replace("[green]", "").replace("[/green]", "")
                .replace("[red]", "").replace("[/red]", "")
                .replace("[yellow]", "").replace("[/yellow]", ""))


def load_config() -> dict:
    cfg = json.loads(json.dumps(DEFAULTS))  # deep copy
    if CONFIG_PATH.exists():
        try:
            saved = json.loads(CONFIG_PATH.read_text())
            for k in DEFAULTS:
                if k in saved:
                    cfg[k] = saved[k]
        except (json.JSONDecodeError, OSError) as e:
            out(f"[yellow]Warning: could not read {CONFIG_PATH}: {e}; using defaults.[/yellow]")
    # normalize extras (back-compat: entries without "bus" get "auto";    # "auto" = first 2 on IDE, rest on virtio-blk, matching the runner)
    normalize_gpu_config(cfg)
    normalize_ovmf_config(cfg)
    if cfg.get("hypervisor") not in ("qemu", "vbox", "hyperv"):
        cfg["hypervisor"] = "qemu"
    # Normalize the keyboard transport the same way, so a stale or hand-edited
    # value in .mfk-launch.json cannot raise later at VALID_KBD.index().
    if str(cfg.get("keyboard", "")).lower() not in VALID_KBD:
        out("[yellow]Warning: unknown keyboard "
            f"{cfg.get('keyboard')!r}; falling back to ps2.[/yellow]")
        cfg["keyboard"] = "ps2"
    else:
        cfg["keyboard"] = str(cfg["keyboard"]).lower()
    if cfg.get("hypervisor") == "hyperv":
        cfg["firmware"] = "uefi"  # Hyper-V Generation 2 is UEFI-only
    norm = []
    for e in cfg.get("extras", []):
        if isinstance(e, dict) and e.get("path"):
            bus = str(e.get("bus", "auto")).lower()
            if bus not in VALID_BUS:
                bus = "auto"
            norm.append({"path": str(e["path"]),
                         "size": str(e.get("size", "64M")),
                         "boot": bool(e.get("boot", False)),
                         "bus": bus})
    cfg["extras"] = norm[:MAX_EXTRAS]
    return cfg


def save_config(cfg: dict) -> None:
    """Auto-save on every change (atomic write)."""
    tmp = CONFIG_PATH.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(cfg, indent=2) + "\n")
    os.replace(tmp, CONFIG_PATH)


def slot_name(n: int, bus: str = "auto") -> str:
    """Human slot for extra #n (0-based). First 2 default to IDE 2/3,
    the rest (or explicit virtio) are virtio-blk (guest drive 4+n)."""
    bus = (bus or "auto").lower()
    if bus == "virtio" or (bus == "auto" and n >= MAX_EXTRA_IDE):
        return f"virtio-blk #{n - MAX_EXTRA_IDE} (drive {4 + n - MAX_EXTRA_IDE})"
    if bus == "ide" and n >= MAX_EXTRA_IDE:
        return f"IDE ? (only 2 IDE slots; will use virtio)"
    return "Secondary Master (IDE 2)" if n == 0 else "Secondary Slave (IDE 3)"


def bootable_probe(path: str) -> str:
    p = REPO_ROOT / path if not os.path.isabs(path) else Path(path)
    try:
        with open(p, "rb") as f:
            mbr = f.read(512)
        if len(mbr) == 512 and mbr[510] == 0x55 and mbr[511] == 0xAA:
            return "bootable"
        return "blank/missing-sig"
    except OSError:
        return "not-created-yet"


def doom_runner_args(cfg: dict) -> list[str]:
    """Runner flags for the automated Doom setup (uses resolved transient keys)."""
    host = str(cfg.get("_doom_host", "") or "")
    if not host:
        return []
    disk = str(cfg.get("doom_disk", "") or "target/doom.img")
    size = str(cfg.get("doom_disk_size", "") or "128M")
    guest = str(cfg.get("_doom_guest", "") or "/wad/doom1.wad")
    args = [f"--wad={host}", f"--wad-disk={disk}",
            f"--wad-disk-size={size}", f"--wad-guest={guest}"]
    if all(e.get("path") != disk for e in cfg.get("extras", [])):
        args += [f"--extra-disk={disk}", f"--extra-disk-size={size}"]
    return args


def to_runner_args(cfg: dict) -> list[str]:
    args = [cfg.get("kernel", DEFAULTS["kernel"])]
    hv = cfg.get("hypervisor", "qemu")
    if hv == "hyperv":
        args.append("--hyperv")
    else:
        args.append("--qemu" if hv == "qemu" else "--vbox")
    # Hyper-V Generation 2 is UEFI-only.
    args.append("--uefi" if (cfg.get("firmware") == "uefi" or hv == "hyperv") else "--bios")
    if hv == "hyperv":
        # Hyper-V Gen2 options (host-side parity; QEMU devices don't apply).
        boot = str(cfg.get("hyperv_boot", "dvd") or "dvd").lower()
        if boot not in ("dvd", "disk"):
            boot = "dvd"
        args.append(f"--hyperv-boot={boot}")
        if cfg.get("hyperv_switch"):
            args.append(f"--hyperv-switch={cfg['hyperv_switch']}")
        try:
            args.append(f"--hyperv-mem={int(cfg.get('hyperv_mem', 512))}")
        except (TypeError, ValueError):
            pass
        try:
            args.append(f"--hyperv-cpus={int(cfg.get('hyperv_cpus', 2))}")
        except (TypeError, ValueError):
            pass
        if cfg.get("hyperv_vhdx"):
            args.append(f"--vhdx={cfg['hyperv_vhdx']}")
        if cfg.get("hyperv_com"):
            args.append(f"--hyperv-com={cfg['hyperv_com']}")
        if cfg.get("bundle_apps"):
            args.append("--bundle-apps")
        if cfg.get("data_disk_size"):
            args.append(f"--data-disk-size={cfg['data_disk_size']}")
        for e in cfg.get("extras", []):
            args.append(f"--extra-disk={e['path']}")
            args.append(f"--extra-disk-size={e['size']}")
        args += doom_runner_args(cfg)
        for key in ("keyboard", "vnc", "web_ui", "gpu_passthrough"):
            if key == "keyboard":
                if str(cfg.get(key, "ps2")).lower() != "ps2":
                    out("[yellow]Note: --kbd is QEMU-only; ignored for Hyper-V.[/yellow]")
            elif cfg.get(key):
                out(f"[yellow]Note: {key} is QEMU-only; ignored for Hyper-V.[/yellow]")
        return args
    args.append(f"--kbd={cfg.get('keyboard', 'ps2')}")
    if cfg.get("bundle_apps"):
        args.append("--bundle-apps")
    if cfg.get("data_disk_size"):
        args.append(f"--data-disk-size={cfg['data_disk_size']}")
    # VNC / Web UI
    if cfg.get("vnc") or cfg.get("web_ui"):
        args.append("--vnc")
        args.append(f"--vnc-port={cfg.get('vnc_port', 5900)}")
    if cfg.get("web_ui"):
        args.append("--web-ui")
        args.append(f"--web-ui-port={cfg.get('web_ui_port', 8084)}")
    if cfg.get("gpu_passthrough"):
        args.append(f"--gpu-passthrough={cfg['gpu_passthrough']}")
        if cfg.get("gpu_audio"):
            args.append(f"--gpu-audio={cfg['gpu_audio']}")
        if cfg.get("gpu_rom"):
            args.append(f"--gpu-rom={cfg['gpu_rom']}")
    for e in cfg.get("extras", []):
        args.append(f"--extra-disk={e['path']}")
        args.append(f"--extra-disk-size={e['size']}")
    args += doom_runner_args(cfg)
    boots = [i for i, e in enumerate(cfg.get("extras", [])) if e.get("boot")]
    if boots:
        if len(boots) > 1:
            out("[yellow]Warning: multiple boot flags; runner uses explicit index.[/yellow]")
        # bare flag boots first; use =N for others
        args.append("--boot-extra-disk" if boots[0] == 0 else f"--boot-extra-disk={boots[0] + 1}")
    return args


def runner_environment(cfg: dict) -> dict[str, str] | None:
    env = os.environ.copy()
    if cfg.get("firmware") == "uefi" and cfg.get("ovmf_code"):
        path = resolve_ovmf_path(cfg["ovmf_code"])
        if not path.is_file():
            return None
        env["OVMF_CODE"] = str(path)
    return env


def show_config(cfg: dict) -> None:
    if RICH:
        t = Table(title="MFK launch config (auto-saved to .mfk-launch.json)")
        t.add_column("Option", style="cyan")
        t.add_column("Value", style="green")
        t.add_row("kernel", cfg["kernel"])
        t.add_row("hypervisor", cfg["hypervisor"])
        t.add_row("firmware", cfg["firmware"])
        t.add_row("ovmf_code", cfg["ovmf_code"] or "AUTO")
        t.add_row("ovmf_status", ovmf_status(cfg))
        t.add_row("keyboard", cfg["keyboard"])
        t.add_row("bundle_apps", str(cfg["bundle_apps"]))
        t.add_row("data_disk_size", cfg["data_disk_size"])
        t.add_row("vnc", "ON" if cfg["vnc"] else "OFF")
        t.add_row("vnc_port", str(cfg["vnc_port"]))
        t.add_row("web_ui", "ON" if cfg["web_ui"] else "OFF")
        t.add_row("web_ui_port", str(cfg["web_ui_port"]))
        t.add_row("gpu_passthrough", cfg["gpu_passthrough"] or "OFF")
        t.add_row("gpu_audio", cfg["gpu_audio"] or "-")
        t.add_row("gpu_rom", cfg["gpu_rom"] or "-")
        t.add_row("gpu_status", gpu_passthrough_status(cfg))
        t.add_row("hyperv_switch", cfg.get("hyperv_switch") or "(auto: Default Switch)")
        t.add_row("hyperv_mem", str(cfg.get("hyperv_mem", 512)))
        t.add_row("hyperv_cpus", str(cfg.get("hyperv_cpus", 2)))
        t.add_row("hyperv_boot", cfg.get("hyperv_boot", "dvd"))
        t.add_row("hyperv_vhdx", cfg.get("hyperv_vhdx") or "(auto)")
        t.add_row("hyperv_com", cfg.get("hyperv_com") or "(auto pipe)")
        t.add_row("doom", "ON" if cfg.get("doom") else "OFF")
        t.add_row("doom_wad", cfg.get("_doom_host") or cfg.get("doom_wad", ""))
        t.add_row("doom_disk", str(cfg.get("doom_disk", "")) or "-")
        t.add_row("doom_guest", cfg.get("_doom_guest") or cfg.get("doom_guest", ""))
        console.print(t)
        d = Table(title=f"Extra drives ({len(cfg['extras'])}/{MAX_EXTRAS}: "
                        f"{MAX_EXTRA_IDE} IDE + {MAX_EXTRA_VIRTIO} virtio)")
        d.add_column("#")
        d.add_column("Path")
        d.add_column("Size")
        d.add_column("Slot")
        d.add_column("Boot")
        d.add_column("Probe")
        for i, e in enumerate(cfg["extras"]):
            d.add_row(str(i + 1), e["path"], e["size"], slot_name(i, e.get("bus", "auto")),
                      "YES" if e["boot"] else "-",
                      bootable_probe(e["path"]))
        console.print(d)
        console.print(Panel(" ".join(["mfk-runner"] + to_runner_args(cfg)),
                            title="Runner preview"))
    else:
        out("== MFK launch config ==")
        for k in ("kernel", "hypervisor", "firmware", "ovmf_code", "ovmf_status", "keyboard",
                  "bundle_apps", "data_disk_size", "vnc", "vnc_port", "web_ui", "web_ui_port",
                  "gpu_passthrough", "gpu_audio", "gpu_rom", "gpu_status",
                  "hyperv_switch", "hyperv_mem", "hyperv_cpus", "hyperv_boot",
                  "hyperv_vhdx", "hyperv_com", "doom", "doom_wad", "doom_disk", "doom_guest"):
            if k == "ovmf_status":
                value = ovmf_status(cfg)
            elif k == "gpu_status":
                value = gpu_passthrough_status(cfg)
            elif k == "doom_wad":
                value = cfg.get("_doom_host") or cfg.get("doom_wad", "")
            elif k == "doom_guest":
                value = cfg.get("_doom_guest") or cfg.get("doom_guest", "")
            else:
                value = cfg[k]
            out(f"  {k}: {value}")
        out(f"  extras ({len(cfg['extras'])}/{MAX_EXTRAS}: "
            f"{MAX_EXTRA_IDE} IDE + {MAX_EXTRA_VIRTIO} virtio):")
        for i, e in enumerate(cfg["extras"]):
            out(f"    {i+1}. {e['path']} size={e['size']} "
                f"slot={slot_name(i, e.get('bus', 'auto'))} boot={e['boot']} [{bootable_probe(e['path'])}]")
        out("  runner: mfk-runner " + " ".join(to_runner_args(cfg)))


def prompt(text: str, default: str = "") -> str:
    suffix = f" [{default}]" if default else ""
    try:
        v = input(f"{text}{suffix}: ").strip()
    except EOFError:
        return default
    return v if v else default



# ---------------------------------------------------------------------------
# Arrow-key terminal UI
# ---------------------------------------------------------------------------

import shutil
import time

try:
    import msvcrt
except ImportError:
    msvcrt = None

if os.name != "nt":
    import select
    import termios
    import tty


def clear_screen() -> None:
    """Clear and redraw the terminal in-place."""
    if os.name == "nt":
        # Enable ANSI/VT processing when possible.
        try:
            import ctypes
            handle = ctypes.windll.kernel32.GetStdHandle(-11)  # STD_OUTPUT_HANDLE
            mode = ctypes.c_uint32()
            if ctypes.windll.kernel32.GetConsoleMode(handle, ctypes.byref(mode)):
                ctypes.windll.kernel32.SetConsoleMode(handle, mode.value | 0x0004)
        except Exception:
            pass

    sys.stdout.write("\x1b[2J\x1b[H")
    sys.stdout.flush()


def read_key() -> str:
    """Read one key without requiring Enter."""
    if msvcrt is not None:
        ch = msvcrt.getwch()

        # Extended/function keys on Windows come as a prefix + second key.
        if ch in ("\x00", "\xe0"):
            ch2 = msvcrt.getwch()
            return {
                "H": "up",
                "P": "down",
                "K": "left",
                "M": "right",
            }.get(ch2, "")
        if ch == "\r":
            return "enter"
        if ch == "\x1b":
            return "escape"
        if ch == "\x08":
            return "backspace"
        if ch == " ":
            return "space"
        return ch.lower()

    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    try:
        tty.setraw(fd)
        ch = sys.stdin.read(1)

        if ch == "\x1b":
            # Escape sequence for arrows: ESC [ A/B/C/D
            r, _, _ = select.select([sys.stdin], [], [], 0.03)
            if r:
                ch2 = sys.stdin.read(1)
                if ch2 == "[":
                    r, _, _ = select.select([sys.stdin], [], [], 0.03)
                    if r:
                        ch3 = sys.stdin.read(1)
                        return {
                            "A": "up",
                            "B": "down",
                            "C": "right",
                            "D": "left",
                        }.get(ch3, "escape")
            return "escape"
        if ch in ("\r", "\n"):
            return "enter"
        if ch in ("\x7f", "\x08"):
            return "backspace"
        if ch == " ":
            return "space"
        return ch.lower()
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)


def edit_value(label: str, current: str) -> str:
    """Temporarily switch to normal line input for text values."""
    clear_screen()
    out(f"[bold]{label}[/bold]")
    out(f"Current: {current}")
    out("Type a new value and press Enter. Press Escape to cancel.")
    try:
        if msvcrt is not None:
            # Normal input is simpler and reliable on Windows.
            value = input("> ").strip()
            return value if value else current

        value = input("> ").strip()
        return value if value else current
    except (EOFError, KeyboardInterrupt):
        return current


def edit_optional_value(label: str, current: str) -> str:
    clear_screen()
    out(f"[bold]{label}[/bold]")
    out(f"Current: {current or '(empty)'}")
    out("Type a value and press Enter. Press Enter with no value to clear. Escape is not available here.")
    try:
        return input("> ").strip()
    except (EOFError, KeyboardInterrupt):
        return current


def choose_from_list(label: str, values: tuple[str, ...], current: str) -> str:
    idx = values.index(current) if current in values else 0
    while True:
        clear_screen()
        out(f"[bold]{label}[/bold]")
        out("")
        for i, value in enumerate(values):
            marker = "➜" if i == idx else " "
            selected = "[bold cyan]" if i == idx else ""
            reset = "[/bold cyan]" if i == idx else ""
            out(f"{marker} {selected}{value}{reset}")
        out("")
        out("W/S select   Enter confirm   Esc cancel")
        key = read_key()
        if key == "w":
            idx = (idx - 1) % len(values)
        elif key == "s":
            idx = (idx + 1) % len(values)
        elif key == "enter":
            return values[idx]
        elif key in ("escape", "q"):
            return current


def extra_menu(cfg: dict, index: int) -> None:
    """Edit one extra disk using the same arrow-key interaction."""
    e = cfg["extras"][index]
    if "bus" not in e or e["bus"] not in VALID_BUS:
        e["bus"] = "auto"
    selected = 0

    while True:
        clear_screen()
        out("[bold cyan]Extra disk editor[/bold cyan]")
        out(f"Drive #{index + 1} — {slot_name(index, e.get('bus', 'auto'))}")
        out("")

        items = [
            ("Path", e["path"]),
            ("Size", e["size"]),
            ("Bus", e.get("bus", "auto")),
            ("Boot", "YES" if e["boot"] else "NO"),
            ("Done", ""),
        ]

        for i, (name, value) in enumerate(items):
            marker = "➜" if i == selected else " "
            value_text = f" : {value}" if value else ""
            if i == selected:
                out(f"[bold cyan]{marker} {name}{value_text}[/bold cyan]")
            else:
                out(f"{marker} {name}{value_text}")

        out("")
        out("W/S select   Enter edit/toggle   Esc back")

        key = read_key()
        if key == "w":
            selected = (selected - 1) % len(items)
        elif key == "s":
            selected = (selected + 1) % len(items)
        elif key == "escape":
            return
        elif key == "enter":
            if selected == 0:
                e["path"] = edit_value("Drive path", e["path"])
                save_config(cfg)
            elif selected == 1:
                e["size"] = edit_value("Drive size", e["size"])
                save_config(cfg)
            elif selected == 2:
                cur = VALID_BUS.index(e.get("bus", "auto"))
                e["bus"] = VALID_BUS[(cur + 1) % len(VALID_BUS)]
                save_config(cfg)
            elif selected == 3:
                e["boot"] = not e["boot"]
                if e["boot"]:
                    for i, other in enumerate(cfg["extras"]):
                        if i != index:
                            other["boot"] = False
                save_config(cfg)
            elif selected == 4:
                return


def add_extra_menu(cfg: dict) -> None:
    if len(cfg["extras"]) >= MAX_EXTRAS:
        clear_screen()
        out(f"[red]Full: max {MAX_EXTRAS} extras "
            f"({MAX_EXTRA_IDE} IDE + {MAX_EXTRA_VIRTIO} virtio).[/red]")
        out("")
        out("Press any key to continue...")
        read_key()
        return

    n = len(cfg["extras"])
    default_path = f"target/extra-disk{n + 1}.img" if n else "target/extra-disk.img"

    path = edit_value("New drive path", default_path)
    size = edit_value("Creation size", "64M")
    bus = "auto"
    if n >= MAX_EXTRA_IDE:
        bus = "virtio"
        out(f"[cyan]Slot #{n + 1} defaults to virtio-blk "
            f"(guest drive {4 + n - MAX_EXTRA_IDE}).[/cyan]")
    cfg["extras"].append({"path": path, "size": size, "boot": False, "bus": bus})
    save_config(cfg)


def remove_extra_menu(cfg: dict) -> None:
    if not cfg["extras"]:
        clear_screen()
        out("[yellow]No extra drives to remove.[/yellow]")
        out("")
        out("Press any key to continue...")
        read_key()
        return

    selected = 0
    while True:
        clear_screen()
        out("[bold red]Remove extra drive[/bold red]")
        out("")

        for i, e in enumerate(cfg["extras"]):
            marker = "➜" if i == selected else " "
            label = f"{i + 1}. {e['path']} ({e['size']})"
            if i == selected:
                out(f"[bold red]{marker} {label}[/bold red]")
            else:
                out(f"{marker} {label}")

        out("")
        out("W/S select   Enter remove   Esc cancel")

        key = read_key()
        if key == "w":
            selected = (selected - 1) % len(cfg["extras"])
        elif key == "s":
            selected = (selected + 1) % len(cfg["extras"])
        elif key == "escape":
            return
        elif key == "enter":
            cfg["extras"].pop(selected)
            save_config(cfg)
            return


def menu_loop(cfg: dict) -> bool:
    """Arrow-key interactive launcher. Returns True when Launch is selected. Uses W/S navigation and A/D changes."""
    # Each item is a callable action plus a display title.
    menu_names = [
        "Hypervisor",
        "Firmware",
        "Keyboard",
        "Bundle apps",
        "Data disk size",
        "Kernel path",
        "VNC",
        "VNC port",
        "Web UI",
        "Web UI port",
        "GPU passthrough",
        "GPU audio BDF",
        "GPU ROM path",
        "OVMF code path",
        "Doom setup",
        "Doom shareware WAD",
        "Doom commercial WAD",
        "Doom disk size",
        "Add extra drive",
        "Edit extra drive",
        "Remove extra drive",
        "Launch",
        "Save & quit",
        "Quit",
    ]

    selected = 0

    while True:
        clear_screen()

        # Header
        width = shutil.get_terminal_size((100, 30)).columns
        title = " MFK QEMU Launcher "
        out(f"[bold cyan]{'═' * max(10, min(width, 100))}[/bold cyan]")
        out(f"[bold cyan]{title.center(min(width, 100))}[/bold cyan]")
        out(f"[bold cyan]{'═' * max(10, min(width, 100))}[/bold cyan]")
        out("")

        # Current config summary
        out(
            f"[bold]Hypervisor:[/bold] {cfg['hypervisor']}    "
            f"[bold]Firmware:[/bold] {cfg['firmware']}    "
            f"[bold]Keyboard:[/bold] {cfg['keyboard']}"
        )
        out(f"[bold]OVMF code:[/bold] {cfg['ovmf_code'] or 'AUTO'} ({ovmf_status(cfg)})")
        out(
            f"[bold]Bundle apps:[/bold] {'ON' if cfg['bundle_apps'] else 'OFF'}    "
            f"[bold]Data disk:[/bold] {cfg['data_disk_size']}"
        )
        out(
            f"[bold]VNC:[/bold] {'ON' if cfg['vnc'] else 'OFF'}    "
            f"[bold]VNC port:[/bold] {cfg['vnc_port']}"
        )
        out(
            f"[bold]Web UI:[/bold] {'ON' if cfg['web_ui'] else 'OFF'}    "
            f"[bold]Web UI port:[/bold] {cfg['web_ui_port']}"
        )
        out(
            f"[bold]GPU passthrough:[/bold] {gpu_passthrough_status(cfg)}    "
            f"[bold]GPU audio:[/bold] {cfg['gpu_audio'] or '-'}"
        )
        out(f"[bold]GPU ROM:[/bold] {cfg['gpu_rom'] or '-'}")
        out(f"[bold]Kernel:[/bold] {cfg['kernel']}")
        out(f"[bold]Extra drives:[/bold] {len(cfg['extras'])}/{MAX_EXTRAS} "
            f"({MAX_EXTRA_IDE} IDE + {MAX_EXTRA_VIRTIO} virtio)")
        out(f"[bold]Doom:[/bold] {'ON' if cfg.get('doom') else 'OFF'}"
            + (f"  WAD: {cfg.get('_doom_host') or cfg.get('doom_wad', '')}" if cfg.get("doom") else ""))
        out("")

        for i, name in enumerate(menu_names):
            marker = "➜" if i == selected else " "
            if i == 0:
                value = f" : {cfg['hypervisor']}"
            elif i == 1:
                value = f" : {cfg['firmware']}"
            elif i == 2:
                value = f" : {cfg['keyboard']}"
            elif i == 3:
                value = f" : {'ON' if cfg['bundle_apps'] else 'OFF'}"
            elif i == 4:
                value = f" : {cfg['data_disk_size']}"
            elif i == 5:
                value = f" : {cfg['kernel']}"
            elif i == 6:
                value = f" : {'ON' if cfg['vnc'] else 'OFF'}"
            elif i == 7:
                value = f" : {cfg['vnc_port']}"
            elif i == 8:
                value = f" : {'ON' if cfg['web_ui'] else 'OFF'}"
            elif i == 9:
                value = f" : {cfg['web_ui_port']}"
            elif i == 10:
                value = f" : {cfg['gpu_passthrough'] or 'OFF'}"
            elif i == 11:
                value = f" : {cfg['gpu_audio'] or '-'}"
            elif i == 12:
                value = f" : {cfg['gpu_rom'] or '-'}"
            elif i == 13:
                value = f" : {cfg['ovmf_code'] or 'AUTO'}"
            elif i == 14:
                value = f" : {'ON' if cfg.get('doom') else 'OFF'}"
            elif i == 15:
                value = f" : {cfg.get('doom_wad', '')}"
            elif i == 16:
                value = f" : {cfg.get('doom_commercial', '') or '-'}"
            elif i == 17:
                value = f" : {cfg.get('doom_disk_size', '')}"
            else:
                value = ""

            if i == 21:
                prefix_text = "▶ "
            elif i == 22:
                prefix_text = "💾 "
            elif i == 23:
                prefix_text = "✕ "
            else:
                prefix_text = "  "

            if i == selected:
                out(f"[bold cyan]{marker} {prefix_text}{name}{value}[/bold cyan]")
            else:
                out(f"{marker} {prefix_text}{name}{value}")

        out("")
        out("[bold]Runner preview[/bold]")
        preview = " ".join(["mfk-runner"] + to_runner_args(cfg))
        out(preview)
        out("")
        out("W/S move   A/D change   Enter edit/action   Space toggle   Q quit")

        key = read_key()

        if key == "w":
            selected = (selected - 1) % len(menu_names)
            continue

        if key == "s":
            selected = (selected + 1) % len(menu_names)
            continue

        if key == "q" or key == "escape":
            return False

        # Left/right can directly change the cycle-based options.
        if selected == 0 and key in ("a", "d"):
            values = ("qemu", "vbox", "hyperv")
            current = values.index(cfg["hypervisor"]) if cfg["hypervisor"] in values else 0
            cfg["hypervisor"] = values[(current + (1 if key == "d" else -1)) % len(values)]
            if cfg["hypervisor"] == "hyperv":
                cfg["firmware"] = "uefi"
            save_config(cfg)
            continue

        if selected == 1 and key in ("a", "d"):
            values = ("bios", "uefi")
            current = values.index(cfg["firmware"])
            cfg["firmware"] = values[(current + (1 if key == "d" else -1)) % len(values)]
            if cfg["hypervisor"] == "hyperv":
                cfg["firmware"] = "uefi"  # Hyper-V Generation 2 is UEFI-only
            save_config(cfg)
            continue

        if selected == 2 and key in ("a", "d"):
            # load_config() normalizes this, so index() cannot raise here.
            current = VALID_KBD.index(cfg["keyboard"])
            step = 1 if key == "d" else -1
            cfg["keyboard"] = VALID_KBD[(current + step) % len(VALID_KBD)]
            save_config(cfg)
            continue

        if selected == 3 and key in ("space", "a", "d"):
            cfg["bundle_apps"] = not cfg["bundle_apps"]
            save_config(cfg)
            continue

        if selected == 14 and key in ("space", "a", "d"):
            cfg["doom"] = not cfg.get("doom")
            if cfg["doom"]:
                host = resolve_doom_wad(cfg)
                cfg["_doom_host"] = host
                cfg["_doom_guest"] = doom_guest_path(cfg, host) if host else ""
            else:
                cfg["_doom_host"] = ""
                cfg["_doom_guest"] = ""
            save_config(cfg)
            continue

        if selected == 6 and key in ("space", "a", "d"):
            cfg["vnc"] = not cfg["vnc"]
            # Web UI implies VNC
            if cfg["web_ui"] and not cfg["vnc"]:
                cfg["vnc"] = True
            save_config(cfg)
            continue

        if selected == 8 and key in ("space", "a", "d"):
            cfg["web_ui"] = not cfg["web_ui"]
            # Web UI implies VNC
            if cfg["web_ui"] and not cfg["vnc"]:
                cfg["vnc"] = True
            save_config(cfg)
            continue

        if key == "enter" or (selected == 3 and key == "space"):
            if selected == 0:
                cfg["hypervisor"] = choose_from_list(
                    "Hypervisor", ("qemu", "vbox", "hyperv"), cfg["hypervisor"]
                )
                if cfg["hypervisor"] == "hyperv":
                    cfg["firmware"] = "uefi"
                save_config(cfg)

            elif selected == 1:
                cfg["firmware"] = choose_from_list(
                    "Firmware", ("bios", "uefi"), cfg["firmware"]
                )
                if cfg["hypervisor"] == "hyperv":
                    cfg["firmware"] = "uefi"  # Hyper-V Generation 2 is UEFI-only
                save_config(cfg)

            elif selected == 2:
                cfg["keyboard"] = choose_from_list(
                    "Keyboard", VALID_KBD, cfg["keyboard"]
                )
                save_config(cfg)

            elif selected == 3:
                cfg["bundle_apps"] = not cfg["bundle_apps"]
                save_config(cfg)

            elif selected == 4:
                cfg["data_disk_size"] = edit_value(
                    "Data disk size", cfg["data_disk_size"]
                )
                save_config(cfg)

            elif selected == 5:
                cfg["kernel"] = edit_value("Kernel path", cfg["kernel"])
                save_config(cfg)

            elif selected == 6:
                # VNC - already toggled by space/A/D, but allow re-toggle
                cfg["vnc"] = not cfg["vnc"]
                if cfg["web_ui"] and not cfg["vnc"]:
                    cfg["vnc"] = True
                save_config(cfg)

            elif selected == 7:
                cfg["vnc_port"] = edit_value("VNC WebSocket port", str(cfg["vnc_port"]))
                save_config(cfg)

            elif selected == 8:
                # Web UI - already toggled by space/A/D, but allow re-toggle
                cfg["web_ui"] = not cfg["web_ui"]
                if cfg["web_ui"] and not cfg["vnc"]:
                    cfg["vnc"] = True
                save_config(cfg)

            elif selected == 9:
                cfg["web_ui_port"] = edit_value("Web UI HTTP port", str(cfg["web_ui_port"]))
                save_config(cfg)

            elif selected == 10:
                value = edit_optional_value("GPU passthrough BDF", cfg["gpu_passthrough"])
                if value and is_valid_pci_bdf(value):
                    cfg["gpu_passthrough"] = value
                    cfg["hypervisor"] = "qemu"
                    cfg["firmware"] = "uefi"
                elif not value:
                    cfg["gpu_passthrough"] = ""
                    cfg["gpu_audio"] = ""
                    cfg["gpu_rom"] = ""
                else:
                    out("[red]Invalid PCI BDF; previous value was kept.[/red]")
                save_config(cfg)

            elif selected == 11:
                value = edit_optional_value("GPU audio BDF", cfg["gpu_audio"])
                if value and (not cfg["gpu_passthrough"] or not is_valid_pci_bdf(value)):
                    out("[red]GPU audio requires a valid GPU passthrough BDF.[/red]")
                elif value:
                    cfg["gpu_audio"] = value
                else:
                    cfg["gpu_audio"] = ""
                save_config(cfg)

            elif selected == 12:
                cfg["gpu_rom"] = edit_optional_value("GPU ROM path", cfg["gpu_rom"])
                save_config(cfg)

            elif selected == 13:
                value = edit_optional_value("OVMF code path", cfg["ovmf_code"])
                if value and not resolve_ovmf_path(value).is_file():
                    out(f"[yellow]OVMF file not found yet: {value}[/yellow]")
                cfg["ovmf_code"] = value
                save_config(cfg)

            elif selected == 14:
                cfg["doom"] = not cfg.get("doom")
                if cfg["doom"]:
                    host = resolve_doom_wad(cfg)
                    cfg["_doom_host"] = host
                    cfg["_doom_guest"] = doom_guest_path(cfg, host) if host else ""
                else:
                    cfg["_doom_host"] = ""
                    cfg["_doom_guest"] = ""
                save_config(cfg)

            elif selected == 15:
                cfg["doom_wad"] = edit_value("Shareware doom1.wad cache path",
                                             str(cfg.get("doom_wad", "target/doom1.wad")))
                if cfg.get("doom"):
                    host = resolve_doom_wad(cfg)
                    cfg["_doom_host"] = host
                    cfg["_doom_guest"] = doom_guest_path(cfg, host) if host else ""
                save_config(cfg)

            elif selected == 16:
                cfg["doom_commercial"] = edit_optional_value(
                    "Commercial WAD path (DOOM.WAD/DOOM64.WAD, empty = shareware)",
                    str(cfg.get("doom_commercial", "") or ""))
                if cfg.get("doom"):
                    host = resolve_doom_wad(cfg)
                    cfg["_doom_host"] = host
                    cfg["_doom_guest"] = doom_guest_path(cfg, host) if host else ""
                save_config(cfg)

            elif selected == 17:
                cfg["doom_disk_size"] = edit_value("Doom disk size",
                                                   str(cfg.get("doom_disk_size", "128M")))
                save_config(cfg)

            elif selected == 18:
                add_extra_menu(cfg)

            elif selected == 19:
                if not cfg["extras"]:
                    clear_screen()
                    out("[yellow]No extra drives yet.[/yellow]")
                    out("")
                    out("Add one first.")
                    out("Press any key to continue...")
                    read_key()
                else:
                    # Edit the currently selected extra, or the first one.
                    extra_selected = 0
                    while True:
                        clear_screen()
                        out("[bold cyan]Select extra drive to edit[/bold cyan]")
                        out("")
                        for i, e in enumerate(cfg["extras"]):
                            marker = "➜" if i == extra_selected else " "
                            text = f"{i + 1}. {e['path']} ({e['size']})"
                            out(f"[bold cyan]{marker} {text}[/bold cyan]" if i == extra_selected else f"{marker} {text}")
                        out("")
                        out("W/S select   Enter edit   Esc back")
                        k = read_key()
                        if k == "w":
                            extra_selected = (extra_selected - 1) % len(cfg["extras"])
                        elif k == "s":
                            extra_selected = (extra_selected + 1) % len(cfg["extras"])
                        elif k == "enter":
                            extra_menu(cfg, extra_selected)
                            break
                        elif k == "escape":
                            break

            elif selected == 20:
                remove_extra_menu(cfg)

            elif selected == 21:
                if cfg.get("doom") and not cfg.get("_doom_host"):
                    host = resolve_doom_wad(cfg)
                    cfg["_doom_host"] = host
                    cfg["_doom_guest"] = doom_guest_path(cfg, host) if host else ""
                save_config(cfg)
                return True

            elif selected == 22:
                save_config(cfg)
                clear_screen()
                out(f"[green]Saved to {CONFIG_PATH}[/green]")
                out("")
                out("Press any key to continue...")
                read_key()

            elif selected == 23:
                return False


def launch(cfg: dict) -> int:
    if cfg.get("doom"):
        host = resolve_doom_wad(cfg)
        cfg["_doom_host"] = host
        cfg["_doom_guest"] = doom_guest_path(cfg, host) if host else ""
        if not host:
            out("[red]Doom setup is ON but no WAD is available; "
                "set Doom commercial WAD or check network for shareware download.[/red]")
            return 2
        save_config(cfg)
    if cfg.get("gpu_passthrough") and is_wsl2():
        out("[red]GPU passthrough is blocked under WSL2: raw PCI passthrough is unavailable.[/red]")
        out("Use a native Linux host with VFIO, or a Hyper-V VM with Discrete Device Assignment.")
        return 2
    if cfg.get("gpu_passthrough") and os.name == "nt":
        out("[red]GPU passthrough is unavailable on native Windows.[/red]")
        out("Use QEMU's emulated GPU, or assign the GPU to a Hyper-V VM with Discrete Device Assignment.")
        return 2
    runner_env = runner_environment(cfg)
    if runner_env is None:
        out(f"[red]OVMF file not found: {cfg['ovmf_code']}[/red]")
        return 2
    runner_bin = find_runner_binary()
    runner_args = to_runner_args(cfg)
    if runner_bin is not None:
        cmd = [str(runner_bin)] + runner_args
    else:
        cmd = ["cargo", "run", "-p", "mfk-runner", "--bin", "mfk-runner", "--release", "--"] + runner_args
    out(f"[bold]Launching:[/bold] {' '.join(cmd)}")
    return subprocess.call(cmd, cwd=str(REPO_ROOT), env=runner_env)


def textual_app(cfg: dict):
    """Optional textual App (only when textual installed + --textual)."""
    from textual.app import App, ComposeResult
    from textual.widgets import Header, Footer, Button, Static, Input, Select
    from textual.containers import Vertical

    class LaunchApp(App):
        CSS = "Screen { align: center middle; }"
        TITLE = "MFK Launcher"

        def compose(self) -> ComposeResult:
            yield Header()
            yield Vertical(
                Static(f"Config: {CONFIG_PATH.name} (auto-saves on Launch)"),
                Static(f"Extras: {len(cfg['extras'])}/{MAX_EXTRAS} | " +
                       ", ".join(e["path"] for e in cfg["extras"]) or "none"),
                Static("Hypervisor: " + cfg["hypervisor"] + "  Firmware: " +
                       cfg["firmware"] + "  Kbd: " + cfg["keyboard"]),
                Static("OVMF code: " + (cfg["ovmf_code"] or "AUTO") +
                       " (" + ovmf_status(cfg) + ")"),
                Static("GPU passthrough: " + gpu_passthrough_status(cfg) +
                       "  BDF: " + (cfg["gpu_passthrough"] or "-") +
                       "  Audio: " + (cfg["gpu_audio"] or "-")),
                Static("Doom: " + ("ON (" + (cfg.get("_doom_host") or cfg.get("doom_wad", "")) + ")"
                        if cfg.get("doom") else "OFF")),
                Static("Runner: mfk-runner " + " ".join(to_runner_args(cfg))),
                Button("Launch", id="go", variant="success"),
                Button("Quit (saved)", id="quit"),
            )
            yield Footer()

        def on_button_pressed(self, event: Button.Pressed) -> None:
            if event.button.id == "go":
                save_config(cfg)
                self.exit(message="launch")
            else:
                self.exit(message="quit")

    app = LaunchApp()
    result = app.run()
    return result == "launch"


def main() -> int:
    ap = argparse.ArgumentParser(description="MFK launch TUI (multi extra drives)")
    ap.add_argument("--show-args", action="store_true",
                    help="print mfk-runner argv from saved config, no launch")
    ap.add_argument("--launch", action="store_true",
                    help="launch with saved config, no menu")
    ap.add_argument("--textual", action="store_true",
                    help="use textual App if installed")
    args = ap.parse_args()

    cfg = load_config()
    if cfg.get("doom"):
        host = resolve_doom_wad(cfg)
        cfg["_doom_host"] = host
        cfg["_doom_guest"] = doom_guest_path(cfg, host) if host else ""

    if args.show_args:
        print(" ".join(to_runner_args(cfg)))
        return 0
    if args.launch:
        return launch(cfg)
    if args.textual:
        try:
            if textual_app(cfg):
                return launch(cfg)
            return 0
        except ImportError:
            out("[yellow]textual not installed; falling back to menu. "
                "pip install -r requirements-launch.txt[/yellow]")

    if menu_loop(cfg):
        return launch(cfg)
    return 0


if __name__ == "__main__":
    sys.exit(main())
