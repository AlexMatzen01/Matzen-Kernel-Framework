#!/usr/bin/env python3
"""MFK QEMU launch TUI - configure extra drives + launch options.

- Interactive terminal menu (rich-enhanced when `rich` is installed,
  plain stdlib fallback otherwise; a `textual` App is used when
  `--textual` is passed and textual is installed).
- Config auto-saves to .mfk-launch.json in the repo root on every change.
- Translates config into mfk-runner flags (repeatable --extra-disk).
- Max 2 extras on IDE (indices 2-3); more need a virtio-blk guest driver.

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

DEFAULTS = {
    "kernel": "target/x86_64-mfk/debug/mfk-kernel",
    "hypervisor": "qemu",       # qemu | vbox
    "firmware": "bios",         # bios | uefi
    "keyboard": "ps2",          # ps2 | ehci | xhci | uhci
    "bundle_apps": False,
    "data_disk_size": "10M",
    "vnc": False,               # Enable QEMU VNC server
    "vnc_port": 5900,           # VNC WebSocket port
    "web_ui": False,            # Launch noVNC web UI (implies vnc)
    "web_ui_port": 8084,        # Web UI HTTP port
    "extras": [
        # {"path": "target/extra-disk.img", "size": "64M", "boot": False}
    ],
}

VALID_KBD = ("ps2", "ehci", "xhci", "uhci")

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
    # normalize extras
    norm = []
    for e in cfg.get("extras", []):
        if isinstance(e, dict) and e.get("path"):
            norm.append({"path": str(e["path"]),
                         "size": str(e.get("size", "64M")),
                         "boot": bool(e.get("boot", False))})
    cfg["extras"] = norm[:MAX_EXTRA_IDE]
    return cfg


def save_config(cfg: dict) -> None:
    """Auto-save on every change (atomic write)."""
    tmp = CONFIG_PATH.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(cfg, indent=2) + "\n")
    os.replace(tmp, CONFIG_PATH)


def slot_name(n: int) -> str:
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


def to_runner_args(cfg: dict) -> list[str]:
    args = [cfg.get("kernel", DEFAULTS["kernel"])]
    args.append("--qemu" if cfg.get("hypervisor", "qemu") == "qemu" else "--vbox")
    args.append("--uefi" if cfg.get("firmware") == "uefi" else "--bios")
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
    for e in cfg.get("extras", []):
        args.append(f"--extra-disk={e['path']}")
        args.append(f"--extra-disk-size={e['size']}")
    boots = [i for i, e in enumerate(cfg.get("extras", [])) if e.get("boot")]
    if boots:
        if len(boots) > 1:
            out("[yellow]Warning: multiple boot flags; runner uses explicit index.[/yellow]")
        # bare flag boots first; use =N for others
        args.append("--boot-extra-disk" if boots[0] == 0 else f"--boot-extra-disk={boots[0] + 1}")
    return args


def show_config(cfg: dict) -> None:
    if RICH:
        t = Table(title="MFK launch config (auto-saved to .mfk-launch.json)")
        t.add_column("Option", style="cyan")
        t.add_column("Value", style="green")
        t.add_row("kernel", cfg["kernel"])
        t.add_row("hypervisor", cfg["hypervisor"])
        t.add_row("firmware", cfg["firmware"])
        t.add_row("keyboard", cfg["keyboard"])
        t.add_row("bundle_apps", str(cfg["bundle_apps"]))
        t.add_row("data_disk_size", cfg["data_disk_size"])
        t.add_row("vnc", "ON" if cfg["vnc"] else "OFF")
        t.add_row("vnc_port", str(cfg["vnc_port"]))
        t.add_row("web_ui", "ON" if cfg["web_ui"] else "OFF")
        t.add_row("web_ui_port", str(cfg["web_ui_port"]))
        console.print(t)
        d = Table(title=f"Extra drives ({len(cfg['extras'])}/{MAX_EXTRA_IDE} IDE slots)")
        d.add_column("#")
        d.add_column("Path")
        d.add_column("Size")
        d.add_column("Slot")
        d.add_column("Boot")
        d.add_column("Probe")
        for i, e in enumerate(cfg["extras"]):
            d.add_row(str(i + 1), e["path"], e["size"], slot_name(i),
                      "YES" if e["boot"] else "-",
                      bootable_probe(e["path"]))
        console.print(d)
        console.print(Panel(" ".join(["mfk-runner"] + to_runner_args(cfg)),
                            title="Runner preview"))
    else:
        out("== MFK launch config ==")
        for k in ("kernel", "hypervisor", "firmware", "keyboard",
                  "bundle_apps", "data_disk_size", "vnc", "vnc_port", "web_ui", "web_ui_port"):
            out(f"  {k}: {cfg[k]}")
        out(f"  extras ({len(cfg['extras'])}/{MAX_EXTRA_IDE}):")
        for i, e in enumerate(cfg["extras"]):
            out(f"    {i+1}. {e['path']} size={e['size']} "
                f"slot={slot_name(i)} boot={e['boot']} [{bootable_probe(e['path'])}]")
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
    selected = 0

    while True:
        clear_screen()
        out("[bold cyan]Extra disk editor[/bold cyan]")
        out(f"Drive #{index + 1} — {slot_name(index)}")
        out("")

        items = [
            ("Path", e["path"]),
            ("Size", e["size"]),
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
                e["boot"] = not e["boot"]
                if e["boot"]:
                    for i, other in enumerate(cfg["extras"]):
                        if i != index:
                            other["boot"] = False
                save_config(cfg)
            elif selected == 3:
                return


def add_extra_menu(cfg: dict) -> None:
    if len(cfg["extras"]) >= MAX_EXTRA_IDE:
        clear_screen()
        out(f"[red]IDE full: max {MAX_EXTRA_IDE} extras (indices 2-3).[/red]")
        out("More drives need a virtio-blk guest driver.")
        out("")
        out("Press any key to continue...")
        read_key()
        return

    default_path = (
        "target/extra-disk.img"
        if not cfg["extras"]
        else "target/extra-disk2.img"
    )

    path = edit_value("New drive path", default_path)
    size = edit_value("Creation size", "64M")
    cfg["extras"].append({"path": path, "size": size, "boot": False})
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
        out(f"[bold]Kernel:[/bold] {cfg['kernel']}")
        out(f"[bold]Extra drives:[/bold] {len(cfg['extras'])}/{MAX_EXTRA_IDE}")
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
                value = ""
            elif i == 11:
                value = ""
            elif i == 12:
                value = ""
            else:
                value = ""

            if i == 13:
                prefix_text = "▶ "
            elif i == 14:
                prefix_text = "💾 "
            elif i == 15:
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
            values = ("qemu", "vbox")
            current = values.index(cfg["hypervisor"])
            cfg["hypervisor"] = values[(current + (1 if key == "d" else -1)) % len(values)]
            save_config(cfg)
            continue

        if selected == 1 and key in ("a", "d"):
            values = ("bios", "uefi")
            current = values.index(cfg["firmware"])
            cfg["firmware"] = values[(current + (1 if key == "d" else -1)) % len(values)]
            save_config(cfg)
            continue

        if selected == 2 and key in ("a", "d"):
            current = VALID_KBD.index(cfg["keyboard"])
            step = 1 if key == "d" else -1
            cfg["keyboard"] = VALID_KBD[(current + step) % len(VALID_KBD)]
            save_config(cfg)
            continue

        if selected == 3 and key in ("space", "a", "d"):
            cfg["bundle_apps"] = not cfg["bundle_apps"]
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
                    "Hypervisor", ("qemu", "vbox"), cfg["hypervisor"]
                )
                save_config(cfg)

            elif selected == 1:
                cfg["firmware"] = choose_from_list(
                    "Firmware", ("bios", "uefi"), cfg["firmware"]
                )
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
                add_extra_menu(cfg)

            elif selected == 7:
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

            elif selected == 11:
                remove_extra_menu(cfg)

            elif selected == 13:
                save_config(cfg)
                return True

            elif selected == 14:
                save_config(cfg)
                clear_screen()
                out(f"[green]Saved to {CONFIG_PATH}[/green]")
                out("")
                out("Press any key to continue...")
                read_key()

            elif selected == 15:
                return False


def launch(cfg: dict) -> int:
    runner_bin = REPO_ROOT / "target" / "release" / "mfk-runner"
    runner_args = to_runner_args(cfg)
    if runner_bin.exists():
        cmd = [str(runner_bin)] + runner_args
    else:
        cmd = ["cargo", "run", "-p", "mfk-runner", "--release", "--"] + runner_args
    out(f"[bold]Launching:[/bold] {' '.join(cmd)}")
    return subprocess.call(cmd, cwd=str(REPO_ROOT))


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
                Static(f"Extras: {len(cfg['extras'])}/{MAX_EXTRA_IDE} | " +
                       ", ".join(e["path"] for e in cfg["extras"]) or "none"),
                Static("Hypervisor: " + cfg["hypervisor"] + "  Firmware: " +
                       cfg["firmware"] + "  Kbd: " + cfg["keyboard"]),
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
