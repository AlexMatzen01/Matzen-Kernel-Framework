# Windows Support — Complete

This document summarizes the addition of full Windows support (PowerShell and Batch) to the Matzen Kernel Framework.

## What Changed

Previously, the build system only supported bash (`.sh`) scripts, requiring WSL or native bash on Windows.

Now there are three ways to build and run MFK on any OS:

### Scripts Added

| File | Platform | Shell | Purpose |
|------|----------|-------|---------|
| `install.ps1` | Windows | PowerShell | Install Rust/components |
| `build.ps1` | Windows | PowerShell | Build kernel + runner |
| `run.ps1` | Windows | PowerShell | Run kernel in VirtualBox |
| `install.bat` | Windows | Command Prompt | Install Rust/components |
| `build.bat` | Windows | Command Prompt | Build kernel + runner |
| `run.bat` | Windows | Command Prompt | Run kernel in VirtualBox |
| `setup-vbox-windows.ps1` | Windows | PowerShell | Verify VirtualBox setup |

### Documentation Added

| File | Purpose |
|------|---------|
| `POWERSHELL_QUICKSTART.md` | Complete PowerShell guide for Windows (includes troubleshooting) |
| `CMD_QUICKSTART.md` | Complete batch/Command Prompt guide for Windows |

## Quick Reference

### Windows Command Prompt (Easiest)

```batch
install.bat
build.bat
run.bat
```

### Windows PowerShell (More Features)

```powershell
.\install.ps1
.\build.ps1
.\run.ps1
```

### Linux/macOS Bash (Unchanged)

```bash
./install.sh
./build.sh
./run.sh
```

### Any Platform (Direct Cargo)

```bash
# No scripts needed
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
cargo build -p mfk-runner --release
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel
```

## Features

### Batch Files (`.bat`) — Windows Command Prompt

✅ **Pros:**
- No setup needed
- Built into Windows
- Familiar syntax for Windows users
- Best for beginners
- Works on all Windows versions

❌ **Cons:**
- Limited scripting features
- No colored output

### PowerShell (`.ps1`) — Windows PowerShell 5.1+

✅ **Pros:**
- Modern scripting language
- Colored output for clarity
- Better error handling
- More flexibility

❌ **Cons:**
- Need to enable script execution first
- Requires PowerShell (not on all older Windows versions)

### Bash (`.sh`) — Linux/macOS/WSL

✅ **Pros:**
- Works on all Unix-like systems
- Widely known
- Best for CI/CD

## Getting Started by OS

### Windows 10/11

**Recommended: Command Prompt (Simplest)**

1. Open Command Prompt (Win + R, type `cmd`)
2. `cd C:\path\to\mfk`
3. `install.bat`
4. `build.bat`
5. `run.bat`

See [CMD_QUICKSTART.md](CMD_QUICKSTART.md)

**Alternative: PowerShell (More features)**

1. Open PowerShell as Administrator
2. `cd C:\path\to\mfk`
3. `Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser` (first time only)
4. `.\install.ps1`
5. `.\build.ps1`
6. `.\run.ps1`

See [POWERSHELL_QUICKSTART.md](POWERSHELL_QUICKSTART.md)

### Linux

```bash
cd ~/path/to/mfk
./install.sh
./build.sh
./run.sh
```

### macOS

```bash
cd ~/path/to/mfk
./install.sh
./build.sh
./run.sh
```

### WSL (Windows Subsystem for Linux)

Use bash scripts as if on Linux:

```bash
./install.sh
./build.sh
./run.sh
```

## Implementation Details

### Batch Scripts

- Use `@echo off` and basic batch syntax
- Error checking with `if errorlevel 1`
- Argument parsing with labels and goto
- Color output via `echo` (limited)

### PowerShell Scripts

- Modern PS5.1+ syntax
- Native colored output with `Write-Host -ForegroundColor`
- Parameter validation
- Error handling with `$LASTEXITCODE`
- Better argument parsing

### All Scripts

- Support hypervisor selection (`--vbox`, `--qemu`)
- Support custom kernel paths
- Provide helpful feedback and progress indicators
- Match functionality of bash equivalents
- Compatible with Windows 10/11 and older versions

## Cross-Platform Behavior

All script variants (`run.sh`, `run.ps1`, `run.bat`) accept the same arguments:

```bash
# All these work the same way:
./run.sh                           # Linux/macOS bash
.\run.ps1                          # Windows PowerShell
run.bat                            # Windows Command Prompt
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel

# All these work the same:
./run.sh --qemu                    # Use QEMU
.\run.ps1 --qemu
run.bat --qemu
```

## Troubleshooting

### PowerShell Script Execution Error

```powershell
Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser
```

### Batch File Not Running

Make sure you're in the right directory:

```batch
cd C:\Users\YourName\Documents\mfk
dir install.bat
install.bat
```

### Command Not Found (Rust/VirtualBox)

Restart the terminal after installing these tools. They update PATH.

## Testing

All scripts have been designed to:

1. ✅ Check prerequisites before running
2. ✅ Provide clear error messages if something fails
3. ✅ Support all command-line options
4. ✅ Display progress and completion messages
5. ✅ Exit with appropriate status codes

## Backward Compatibility

- ✅ Bash scripts unchanged (still support `.sh`)
- ✅ Kernel code unchanged
- ✅ Build commands identical
- ✅ Output and behavior consistent across platforms
- ✅ All scripts use the same runner

## Future Improvements

Potential enhancements:

- Shell script for universal shebang-based selection (`.mfk` or `mfk`)
- GUI launcher (optional)
- Better progress indicators
- Parallel build option
- Profile/optimization options

## Documentation

Full guides are available:

- **[README.md](README.md)** — Updated with Windows guidance
- **[POWERSHELL_QUICKSTART.md](POWERSHELL_QUICKSTART.md)** — 200+ lines, covers everything
- **[CMD_QUICKSTART.md](CMD_QUICKSTART.md)** — Complete batch guide
- **[VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md)** — Networking details
- **[RUNNER_REFERENCE.md](RUNNER_REFERENCE.md)** — Command reference
- **[MIGRATION_QEMU_TO_VBOX.md](MIGRATION_QEMU_TO_VBOX.md)** — For QEMU users

## Summary

✅ **Windows users now have a seamless experience** choosing batch or PowerShell

✅ **Linux/macOS users are unaffected** — bash scripts still work perfectly

✅ **All platforms supported** with appropriate native tools

✅ **Comprehensive documentation** for every platform

✅ **Consistent behavior** across all script variants

Get started now:
- Windows: `install.bat` or `.\install.ps1`
- Linux/macOS: `./install.sh`
- Then: `build` (your variant) → `run` (your variant)
