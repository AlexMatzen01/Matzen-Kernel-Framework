# Installation Guide

Detailed setup instructions for the Matzen Kernel Framework.

## System Requirements

### Minimum Requirements
- **OS**: Linux, macOS, or Windows with WSL2
- **CPU**: x86_64 with virtualization support (for QEMU)
- **RAM**: 4GB minimum (2GB for QEMU, 2GB for build tools)
- **Disk**: 2GB free space
- **Network**: Internet for downloading dependencies

### Recommended Setup
- **OS**: Linux (Ubuntu 20.04+, Fedora 35+, Arch)
- **CPU**: Modern x86_64 with hardware virtualization
- **RAM**: 8GB+
- **Disk**: SSD with 5GB+ free space

## Step-by-Step Installation

### 1. Install Rust Nightly

#### On Linux/macOS/WSL:
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
rustup toolchain install nightly
rustup component add rust-src llvm-tools-preview --toolchain nightly
```

#### Verify installation:
```bash
rustc --version
cargo --version
rustup toolchain list | grep nightly
```

### 2. Install Build Dependencies

#### Ubuntu/Debian:
```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential \
  qemu-system-x86 \
  qemu-utils \
  git
```

#### Fedora/RHEL:
```bash
sudo dnf install -y \
  gcc \
  qemu-system-x86 \
  qemu-img \
  git
```

#### macOS (Homebrew):
```bash
brew install qemu git
```

#### Windows (WSL2):
```bash
# In your WSL2 terminal
sudo apt-get update
sudo apt-get install -y qemu-system-x86 qemu-utils git
```

### 3. Clone Repository

```bash
git clone https://github.com/AlexMatzen01/Matzen-Kernel-Framework.git
cd Matzen-Kernel-Framework
```

### 4. Verify Rust Target

MFK uses a custom x86_64 target. Verify the target spec exists:

```bash
ls -la targets/x86_64-mfk.json
```

### 5. Test Build Tools

Build a test to ensure everything works:

```bash
./build.sh
```

Expected output:
```
   Compiling mfk-kernel v0.1.0
   ...
    Finished `dev` profile [unoptimized + debuginfo] target(s) in X.XXs
    Finished `release` profile [optimized] target(s) in X.XXs
```

### 6. Create Disk Image (Optional)

If you want to use existing disk image:

```bash
qemu-img create -f raw target/disk.img 10M
```

## Verification

Run a quick test to confirm installation:

```bash
# Build
./build.sh

# Run kernel
timeout 10 bash << 'EOF'
(
  sleep 3
  echo "help"
  sleep 1
  echo "halt"
) | ./run.sh target/x86_64-mfk/debug/mfk-kernel
EOF
```

Expected output should include:
```
Serial port initialized
VGA initialized
...
E1000 initialized
Network configured: IP 10.0.2.15
mfk> help
Available commands:
...
```

## Troubleshooting Installation

### "qemu-system-x86_64: not found"
- Install QEMU: See above OS-specific instructions
- Or: `which qemu-system-x86_64` to verify installation

### "rustc: command not found"
- Run: `source $HOME/.cargo/env`
- Or: Restart your terminal

### "Permission denied" on build.sh
- Make executable: `chmod +x build.sh run.sh install.sh`

### Build fails with "error: failed to resolve: use of undeclared crate"
- Update Rust: `rustup update`
- Clean build: `cargo clean && ./build.sh`

### QEMU fails to start
- Verify virtualization: `grep -c 'vmx\|svm' /proc/cpuinfo` (should be > 0)
- Try without KVM: Edit `tools/src/main.rs` or use `-no-kvm` flag

## Optional: Install Development Tools

For development and debugging:

### VS Code Extensions
```bash
code --install-extension rust-lang.rust-analyzer
code --install-extension vadimcn.vscode-lldb
```

### Debugging Tools
```bash
# GDB (Linux/macOS)
brew install gdb  # macOS
sudo apt-get install gdb  # Linux

# LLDB (macOS)
xcode-select --install

# Objdump for binary inspection
sudo apt-get install binutils  # Linux
brew install binutils  # macOS
```

## Platform-Specific Notes

### Linux
- Most straightforward setup
- All tools available in official package managers
- Native hardware virtualization support

### macOS
- Use Homebrew for dependencies
- Intel and Apple Silicon Macs supported (via UTM for M1/M2)
- May need to increase QEMU memory: `-m 256M`

### Windows (WSL2)
- Requires Windows 10/11 with WSL2 installed
- Hardware virtualization must be enabled in BIOS
- Recommended: Use WSL2 with Ubuntu 20.04 or newer

### Docker (Alternative)
If you prefer containerized setup:

```dockerfile
FROM ubuntu:22.04
RUN apt-get update && apt-get install -y \
    curl build-essential qemu-system-x86 qemu-utils git
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain nightly
ENV PATH="/root/.cargo/bin:${PATH}"
WORKDIR /mfk
CMD ["/bin/bash"]
```

## Next Steps

1. **[Quick Start Guide](quick-start.md)** — Boot the kernel in 5 minutes
2. **[Building Guide](../development/building.md)** — Understand the build process
3. **[Architecture Overview](../reference/architecture.md)** — Learn system design

## Getting Help

- **Issues**: GitHub Issues
- **Discussions**: GitHub Discussions  
- **Docs**: This documentation site
