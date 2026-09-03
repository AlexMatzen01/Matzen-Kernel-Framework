#!/usr/bin/env bash
# MFK Installation Script for Debian / Linux
# Sets up Rust nightly, required components and Debian system dependencies
set -euo pipefail

echo "======================================"
echo "MFK Installation Setup (Debian/Linux)"
echo "======================================"
echo ""

# --- Debian system dependencies (best effort, never fail the script) ---
if command -v apt-get >/dev/null 2>&1; then
    echo "[0/4] Checking Debian system dependencies..."
    MISSING_PKGS=()
    for pkg in build-essential pkg-config ovmf; do
        if ! dpkg -s "$pkg" >/dev/null 2>&1; then
            MISSING_PKGS+=("$pkg")
        fi
    done
    # qemu packages: qemu-system-x86 provides qemu-system-x86_64 + qemu-img on Debian 11/12
    if ! command -v qemu-system-x86_64 >/dev/null 2>&1 && ! command -v qemu-img >/dev/null 2>&1; then
        MISSING_PKGS+=("qemu-system-x86")
    fi
    if ! command -v ip >/dev/null 2>&1; then
        MISSING_PKGS+=("iproute2")
    fi

    if [ ${#MISSING_PKGS[@]} -gt 0 ]; then
        echo "  Missing packages: ${MISSING_PKGS[*]}"
        if [ "$(id -u)" -eq 0 ]; then
            echo "  Installing via apt-get (running as root)..."
            apt-get update
            apt-get install -y "${MISSING_PKGS[@]}" || echo "  ⚠ apt-get install failed - please run manually: sudo apt-get install ${MISSING_PKGS[*]}"
        elif sudo -n true 2>/dev/null; then
            echo "  Installing via sudo apt-get..."
            sudo apt-get update
            sudo apt-get install -y "${MISSING_PKGS[@]}" || echo "  ⚠ apt-get install failed - please run: sudo apt-get install ${MISSING_PKGS[*]}"
        else
            echo "  Please install manually:"
            echo "    sudo apt-get update"
            echo "    sudo apt-get install -y ${MISSING_PKGS[*]}"
        fi
    else
        echo "  ✓ System dependencies present"
    fi
    echo ""

    # Optional: VirtualBox hint (don't auto-install - requires contrib/non-free on Debian)
    if ! command -v VBoxManage >/dev/null 2>&1; then
        echo "  ℹ VirtualBox not found (recommended for bridged networking)."
        echo "    Debian 12: sudo apt-get install virtualbox virtualbox-dkms linux-headers-\$(uname -r)"
        echo "    Or download from https://www.virtualbox.org/wiki/Downloads"
        echo "    QEMU will be used as fallback (./run.sh --qemu)."
        echo ""
    fi
fi

# Step 1: Check Rust
echo "[1/4] Checking Rust installation..."
if ! command -v rustc >/dev/null 2>&1; then
    echo "ERROR: Rust not found" >&2
    echo ""
    echo "Install via rustup:"
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    echo "  source \"\$HOME/.cargo/env\""
    echo ""
    exit 1
fi
echo "  ✓ Rust found: $(rustc --version)"
if ! command -v rustup >/dev/null 2>&1; then
    echo "ERROR: rustup not found (needed for nightly toolchain)" >&2
    exit 1
fi
echo ""

# Step 2: Install nightly toolchain (idempotent) - respects rust-toolchain.toml pin
TOOLCHAIN="$(grep -E 'channel.*=' rust-toolchain.toml 2>/dev/null | sed -E 's/.*channel.*\"(nightly[^\"]*)\".*/\1/' || echo nightly)"
if [[ -z "$TOOLCHAIN" ]]; then TOOLCHAIN="nightly"; fi
echo "[2/4] Ensuring toolchain: $TOOLCHAIN ..."
if rustup toolchain list | grep -q "^${TOOLCHAIN}"; then
    echo "  ✓ $TOOLCHAIN already installed"
else
    echo "  Installing $TOOLCHAIN..."
    rustup toolchain install "$TOOLCHAIN"
fi
# Make sure toolchain is usable
if ! rustup run "$TOOLCHAIN" rustc --version >/dev/null 2>&1; then
    echo "ERROR: toolchain $TOOLCHAIN not functional" >&2
    exit 1
fi
echo "  ✓ $TOOLCHAIN: $(rustup run "$TOOLCHAIN" rustc --version)"
echo ""

# Step 3: Required components
echo "[3/4] Ensuring required components (rust-src, llvm-tools-preview) for $TOOLCHAIN..."
for comp in rust-src llvm-tools-preview; do
    if rustup component list --toolchain "$TOOLCHAIN" 2>/dev/null | grep -q "^${comp}.*(installed)"; then
        echo "  ✓ $comp already installed"
    else
        echo "  Installing $comp for $TOOLCHAIN..."
        rustup component add "$comp" --toolchain "$TOOLCHAIN" || {
            echo "ERROR: failed to add $comp for $TOOLCHAIN" >&2
            exit 1
        }
        echo "  ✓ $comp added"
    fi
done
echo ""

# Step 4: Verify hypervisor tools
echo "[4/4] Checking hypervisor tools..."
if command -v VBoxManage >/dev/null 2>&1; then
    echo "  ✓ VirtualBox: $(VBoxManage --version 2>/dev/null | head -n1)"
else
    echo "  ⚠ VirtualBox not found - QEMU fallback available"
fi
if command -v qemu-system-x86_64 >/dev/null 2>&1; then
    echo "  ✓ QEMU: $(qemu-system-x86_64 --version 2>/dev/null | head -n1)"
else
    echo "  ⚠ QEMU not found - install: sudo apt-get install qemu-system-x86"
fi
if command -v qemu-img >/dev/null 2>&1; then
    echo "  ✓ qemu-img: $(qemu-img --version 2>/dev/null | head -n1)"
fi
echo ""

echo "======================================"
echo "Setup Complete!"
echo "======================================"
echo ""
echo "Next steps:"
echo "  ./build.sh          # build kernel + runner"
echo "  ./run.sh            # run in VirtualBox (or ./run.sh --qemu)"
echo "  ./setup-vbox-linux.sh  # optional: diagnose VirtualBox setup"
