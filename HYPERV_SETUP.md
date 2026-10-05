# Hyper-V Setup for Matzen Kernel Framework

Run MFK in **Hyper-V Generation 2 (UEFI-only)** on Windows. Hyper-V stays **opt-in** — VirtualBox remains the default (`--vbox`).

## Requirements

* Windows 10/11 Pro, Enterprise, or Education (Hyper-V role) or Windows Server.
* Elevated shell (Run as administrator).
* A virtual switch — `Default Switch` is auto-detected; otherwise pass `--hyperv-switch="<name>"`.
* `qemu-img` recommended for raw→VHDX conversion (fallback: Hyper-V `Convert-VHD`).

```powershell
# Check everything at once
.\setup-hyperv-windows.ps1
```

## Quick Start

```powershell
.\build.ps1
.\run.ps1 --hyperv --uefi
```

The runner will:

1. Build `*-uefi.img` + `*-bios.img` (same as QEMU/VBox) and `*-uefi.iso` (El Torito UEFI DVD).
2. Create `target/disk.img` (raw) + bundle `--bundle-apps` if requested.
3. Convert to SCSI VHDX: `target/disk.vhdx`, `<extra>.vhdx`, and `<uefi>.img.vhdx` for `--hyperv-boot=disk`.
4. Create VM `MFK-<kernel>-uefi` (Gen2, Secure Boot **off** — the MFK loader is unsigned), attach DVD + SCSI disks, map COM1 to a named pipe, connect the switch, set boot order, start.

## Boot Modes

| Mode | Flag | What boots |
|------|------|------------|
| DVD (default) | `--hyperv-boot=dvd` | UEFI ISO on the DVD drive |
| Disk | `--hyperv-boot=disk` | VHDX converted from the UEFI image (needs `qemu-img` or `Convert-VHD`) |
| Explicit VHDX | `--vhdx=<path>` | With `dvd`: attached as extra data disk; with `disk`: used as the boot disk |

```powershell
.\run.ps1 --hyperv --uefi --hyperv-boot=disk
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel --hyperv --vhdx=custom.vhdx
```

## Disks

* Data disk: `--data-disk-size=10M` sizes `target/disk.img`, converted to `target/disk.vhdx` (SCSI). Existing VHDX is reused when newer than the raw (same policy as VBox VDI); `--force` rebuilds.
* Extra disks: repeatable `--extra-disk=<raw> --extra-disk-size=<size>` (max 8). Each raw becomes `<raw>.vhdx` on SCSI.
* Bundle apps: `--bundle-apps` / `--with-apps` writes `apps/examples` into `target/disk.img` *before* VHDX conversion, so it works on the first run.

Inside the kernel the SCSI disks appear as block devices; use `diskinfo`, `mkfs`, `mount` as usual.

> Gen2 has no IDE controller: everything is SCSI VHDX. The kernel's legacy ATA path does not apply; the UEFI bootloader reads the boot device, and data disks use the block layer.

## Networking

* Default: `Default Switch` (NAT + DHCP) auto-detected, else the first `Get-VMSwitch`.
* Custom: `--hyperv-switch="<name>"` (validated; runner lists switches on error).
* Inside the kernel: `dhclient eth0` (or static `ifconfig` + `route`), then `ping`.

Contrast: VBox bridged gives full Layer-2/ICMP; Default Switch NAT is fine for DHCP/outbound but ICMP behavior follows the host NAT.

## Serial Console

Gen2 has no file-backed serial like `target/mfk-serial.log`. The runner maps COM1 to `\\.\pipe\MFK-<vm>-com1` (override: `--hyperv-com=<pipe|off>`).

```powershell
# Stream serial output (VM keeps running; Ctrl+C detaches)
.\tools\hyperv-serial.ps1
.\tools\hyperv-serial.ps1 -VMName MFK-mfk-kernel-uefi -PipeName MFK-mfk-kernel-com1
```

Log default: `target/mfk-hyperv-serial.log`. GUI console remains available via Hyper-V Manager / `vmconnect.exe`.

## Resources & Switches

```powershell
.\run.ps1 --hyperv --uefi --hyperv-switch="Default Switch" --hyperv-mem=512 --hyperv-cpus=2
```

Reused VMs keep CPU/memory in sync on each run. Recreate from scratch:

```powershell
Stop-VM -Name MFK-mfk-kernel-uefi -TurnOff -Force
Remove-VM -Name MFK-mfk-kernel-uefi -Force
# or: <runner> --hyperv --force
```

## Managing VMs

```powershell
Get-VM MFK-*-uefi | Select Name, State, Generation
Get-VMSwitch | Select Name, SwitchType
Get-VMComPort -VMName MFK-mfk-kernel-uefi
Stop-VM -Name MFK-mfk-kernel-uefi
```

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `NOT_ADMIN` / preflight fails | Run PowerShell as administrator |
| `NO_MODULE` / `NO_CMDLETS` | `OptionalFeatures.exe` → enable Hyper-V → reboot |
| `NO_SWITCH` / no switch | Create one in Hyper-V Manager → Virtual Switch Manager, or pass `--hyperv-switch=` |
| `--hyperv` + `--bios` | Gen2 is UEFI-only: drop `--bios` / use `--uefi` |
| Missing VHDX + no converter | Install `qemu-img` (`choco install qemu`) or pre-convert; `Convert-VHD` is the fallback |
| `--vnc/--web-ui/--kbd/--gpu-*` with `--hyperv` | QEMU-only; runner warns and ignores (GPU in Hyper-V = manual DDA) |
| `--boot-extra-disk` with `--hyperv` | Ignored; use `--hyperv-boot=dvd\|disk` |
| VBox + Hyper-V conflict | Only one hypervisor runs a VM at a time; stop the other VM first |
| COM pipe won't connect | Check `Get-VMComPort`, VM state = Running, correct `-PipeName` |

## Non-Goals

No VMBus/netvsc/storvsc guest drivers, no Gen1/BIOS, no automatic `New-VMSwitch` NAT creation, no VNC/web-UI/GPU-passthrough on Hyper-V. See `RUNNER_REFERENCE.md` for the full flag table.
