# Windows Setup Verification Checklist

Use this checklist to verify your Windows setup is complete and working.

## Pre-Flight Checks

### Prerequisites

- [ ] Windows 10 or newer (or Windows 8/7 for batch-only)
- [ ] Administrator access (for some VirtualBox operations)
- [ ] Stable internet connection (for installing Rust, VirtualBox)

### Installation Media

- [ ] **Rust installed:** `rustc --version` (in Command Prompt or PowerShell)
- [ ] **VirtualBox installed:** `VBoxManage --version` (in Command Prompt or PowerShell)
- [ ] **Git installed** (optional, for version control): `git --version`

## Choose Your Path

### Path 1: Command Prompt (Simplest) ✅ RECOMMENDED

**System Requirements:**
- [ ] Windows 10/11/older
- [ ] Command Prompt (always available)
- [ ] No special setup needed

**To Verify:**
1. Open Command Prompt (Win + R, type `cmd`)
2. Type: `install.bat`
3. Script should run successfully

### Path 2: PowerShell (More Features)

**System Requirements:**
- [ ] Windows 10/11
- [ ] PowerShell 5.1+ (usually pre-installed)
- [ ] Script execution enabled

**First-Time Setup:**
```powershell
Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser
```

**To Verify:**
1. Open PowerShell (search for "PowerShell")
2. Run: `.\install.ps1`
3. Script should run successfully

### Path 3: Manual/Direct Cargo (Advanced)

**To Verify:**
```batch
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
```

## Step-by-Step Verification

### Step 1: Rust Toolchain

**In Command Prompt or PowerShell:**

```batch
rustc --version
cargo --version
rustup toolchain list
```

**Expected Output:**
```
rustc 1.xx.x (...)
cargo 1.xx.x
installed toolchains:
...
stable-x86_64-pc-windows-msvc (default)
nightly-x86_64-pc-windows-msvc
```

**If not installed:**
```batch
# Windows Command Prompt
rustup update
rustup toolchain install nightly
rustup component add rust-src llvm-tools-preview --toolchain nightly
```

### Step 2: VirtualBox

**Verify Installation:**

```batch
VBoxManage --version
```

**Expected Output:**
```
7.0.x or similar
```

**If not installed:**
1. Download: https://www.virtualbox.org/wiki/Downloads
2. Or: `choco install virtualbox`
3. **Restart** Command Prompt/PowerShell after installation

### Step 3: Project Structure

**Verify you're in the right directory:**

```batch
dir install.bat build.bat run.bat
type build.sh run.sh
```

**Expected:** Files should exist in the current directory

### Step 4: Script Files Exist

**Verify all required scripts:**

```batch
# On Windows:
dir *.ps1 *.bat

# Should show:
#   build.ps1, run.ps1, install.ps1
#   build.bat, run.bat, install.bat
#   setup-vbox-windows.ps1
```

## Run the Installation

### Using Command Prompt (Recommended)

```batch
REM Navigate to project directory
cd C:\path\to\Matzen-Kernel-Framework

REM Run installation
install.bat
```

**Expected:**
- ✓ Rust version shown
- ✓ Nightly toolchain installed
- ✓ Components added
- ✓ VirtualBox version shown (or warning if not installed)
- ✓ Success message

### Using PowerShell (Alternative)

```powershell
cd C:\path\to\Matzen-Kernel-Framework
.\install.ps1
```

**Expected:**
- ✓ Colored output with checkmarks
- ✓ All steps complete
- ✓ Success message

## Build the Project

### Using Command Prompt

```batch
build.bat
```

**Expected:**
```
Building MFK Kernel Framework...

[1/2] Building kernel...
✓ Kernel build complete

[2/2] Building runner...
✓ Runner build complete

======================================
Build Complete!
```

**Time:** First build takes 1-2 minutes

### Using PowerShell

```powershell
.\build.ps1
```

**Expected:** Same output with colored text

## Run the Kernel

### Using Command Prompt

```batch
run.bat
```

**Expected:**
- VirtualBox window opens
- Kernel boots
- Shell prompt appears

### Using PowerShell

```powershell
.\run.ps1
```

**Expected:** Same as command prompt

## Inside the Kernel

Once you see the shell prompt (`mfk> `):

### Test 1: Show Help

```bash
help
```

**Expected:** List of available commands

### Test 2: Configure Network

```bash
dhclient eth0
```

**Expected:** Kernel gets an IP address

### Test 3: Test Connectivity

```bash
ping 8.8.8.8
```

**Expected:** Ping responses (now works perfectly with VirtualBox!)

### Test 4: Filesystem

```bash
mkfs
mount
ls
```

**Expected:** Filesystem commands work

## Troubleshooting Matrix

| Problem | Cause | Solution |
|---------|-------|----------|
| "install.bat not found" | Wrong directory | `cd` to project root |
| "cargo not found" | Rust not in PATH | Restart terminal after installing Rust |
| "VBoxManage not found" | VirtualBox not in PATH | Restart terminal after installing VirtualBox |
| Script won't run (PowerShell) | Execution policy | Run: `Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser` |
| Build fails | Toolchain issue | Run: `rustup update && rustup toolchain install nightly` |
| VirtualBox fails | VM creation issue | Run: `VBoxManage unregistervm MFK-mfk-kernel --delete` |
| No network in kernel | Bridge not configured | See [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md) |

## Quick Diagnostic Commands

Run these if something goes wrong:

```batch
REM Check Rust
rustc --version
cargo --version

REM Check VirtualBox
VBoxManage --version

REM List VMs
VBoxManage list vms

REM Show VM details
VBoxManage showvminfo MFK-mfk-kernel

REM Clean rebuild
cargo clean
build.bat
run.bat

REM Manual build (if script fails)
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
cargo build -p mfk-runner --release
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel
```

## Success Criteria

✅ **Installation successful if:**
- [ ] `install.bat` or `.\install.ps1` completes without errors
- [ ] `rustc --version` shows recent version
- [ ] `VBoxManage --version` shows version number
- [ ] No missing component warnings

✅ **Build successful if:**
- [ ] `build.bat` or `.\build.ps1` completes without errors
- [ ] No cargo build errors in output
- [ ] "Build Complete!" message shown
- [ ] Binary files created in `target/`

✅ **Run successful if:**
- [ ] VirtualBox window opens
- [ ] Kernel starts booting
- [ ] Shell prompt appears
- [ ] `help` command works
- [ ] `ifconfig` shows eth0

✅ **Networking successful if:**
- [ ] `dhclient eth0` gets an IP
- [ ] `ping 8.8.8.8` receives replies
- [ ] Serial output appears in VirtualBox or `target/mfk-serial.log`

## Next Steps After Verification

1. ✅ Read full documentation:
   - [README.md](README.md) — Project overview
   - [WINDOWS_SUPPORT.md](WINDOWS_SUPPORT.md) — Windows-specific info

2. ✅ Choose your platform guide:
   - [CMD_QUICKSTART.md](CMD_QUICKSTART.md) — Command Prompt details
   - [POWERSHELL_QUICKSTART.md](POWERSHELL_QUICKSTART.md) — PowerShell details

3. ✅ Learn about networking:
   - [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md) — Network configuration
   - [NETWORKING.md](NETWORKING.md) — Protocol details

4. ✅ Explore the kernel:
   - [test_commands.txt](test_commands.txt) — Shell test commands
   - [docs/reference/](docs/reference/) — Architecture and reference docs

## Getting Help

If verification fails at any step:

1. **Check the error message** — it usually explains what's wrong
2. **Read the guide for your platform:**
   - [CMD_QUICKSTART.md](CMD_QUICKSTART.md) (Command Prompt)
   - [POWERSHELL_QUICKSTART.md](POWERSHELL_QUICKSTART.md) (PowerShell)
3. **Check troubleshooting:**
   - [docs/development/troubleshooting.md](docs/development/troubleshooting.md)
   - [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md) (for VirtualBox issues)
4. **Review file navigation:**
   - [FILE_INDEX.md](FILE_INDEX.md) — All files and where to find information

## Verification Complete! 🎉

Once you've checked all boxes and can run `ping 8.8.8.8` from the kernel shell, you're all set!

**Ready to develop?** Check [docs/development/contributing.md](docs/development/contributing.md)

**Questions?** See [FILE_INDEX.md](FILE_INDEX.md)

---

**Last Updated:** May 2026  
**Supported:** Windows 7+ (Command Prompt), Windows 10+ (PowerShell)
