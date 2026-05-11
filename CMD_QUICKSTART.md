# Windows Command Prompt Quick Start — MFK

This guide helps you run the Matzen Kernel Framework on Windows using Command Prompt (cmd.exe).

## Prerequisites

1. **Command Prompt** (built into Windows)
   - Press `Win + R`, type `cmd`, press Enter
   - Or search for "Command Prompt"

2. **Rust** — with nightly toolchain
   - Install from: https://rustup.rs/
   - Or: `choco install rust` (if using Chocolatey)

3. **VirtualBox** (recommended)
   - Install from: https://www.virtualbox.org/wiki/Downloads
   - Or: `choco install virtualbox`

## Quick Start

### 1. Open Command Prompt

Press `Win + R`, type `cmd`, press Enter

### 2. Navigate to Project

```batch
cd C:\path\to\Matzen-Kernel-Framework
```

### 3. Install Rust (First Time Only)

```batch
install.bat
```

### 4. Build Kernel and Runner

```batch
build.bat
```

### 5. Run in VirtualBox

```batch
run.bat
```

That's it! The kernel will boot in VirtualBox.

## Inside the Kernel Shell

### Test Network (Best Feature with VirtualBox)

```bash
dhclient eth0
ping 8.8.8.8
```

### Try Filesystem

```bash
mkfs
mount
touch myfile.txt
write myfile.txt "Hello"
cat myfile.txt
ls
```

## Command Options

### Run with QEMU

```batch
run.bat --qemu
```

### Use Release Build

```batch
run.bat target/x86_64-mfk/release/mfk-kernel
```

### Rebuild Everything

```batch
cargo clean
build.bat
run.bat
```

## Available Batch Files

| File | Purpose |
|------|---------|
| `install.bat` | Verify Rust setup and install components |
| `build.bat` | Build kernel + runner |
| `run.bat` | Run kernel in VirtualBox |
| `setup-vbox-windows.ps1` | Verify VirtualBox setup |

## Comparison: batch vs PowerShell

Both work on Windows. Choose based on preference:

| Feature | Batch (`.bat`) | PowerShell (`.ps1`) |
|---------|---|---|
| Learn to use | Simpler, built-in | Need to enable scripts |
| Features | Basic | More advanced |
| Compatibility | Works everywhere | May need setup |
| Best for | Quick start | Complex workflows |

**Recommendation:** Start with batch files (`.bat`) if unsure, switch to PowerShell later for advanced features.

## Troubleshooting

### "cargo: command not found"

Rust not in PATH. Restart Command Prompt after installing Rust, or:

```batch
REM Manually add Rust to PATH for this session
set PATH=%PATH%;%USERPROFILE%\.cargo\bin
cargo --version
```

### "VBoxManage: command not found"

VirtualBox not installed or not in PATH:

1. Install from https://www.virtualbox.org/wiki/Downloads
2. Or: `choco install virtualbox`
3. **Restart Command Prompt**

### Build Fails

```batch
REM Update Rust
rustup update

REM Clean and rebuild
cargo clean
build.bat
```

## Manual Commands

If the batch files aren't working, run commands manually:

```batch
REM Build kernel
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem

REM Build runner
cargo build -p mfk-runner --release

REM Run
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel
```

## Next Steps

1. ✅ Run `install.bat`
2. ✅ Run `build.bat`
3. ✅ Run `run.bat`
4. Inside kernel: `dhclient eth0` then `ping 8.8.8.8`

Enjoy! 🚀
