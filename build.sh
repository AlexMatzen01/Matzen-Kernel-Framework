#!/usr/bin/env bash
# MFK Build Script for Debian / Linux
# Builds the kernel (nightly + custom target) and the runner
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "Building MFK Kernel Framework..."
echo ""

# Respect rust-toolchain.toml (pinned nightly-2025-09-08). Use plain cargo; rustup picks correct nightly.
# Only use explicit +nightly if toolchain file missing and nightly is available.
CARGO_CMD="cargo"
if [[ ! -f "rust-toolchain.toml" ]] && command -v rustup >/dev/null 2>&1 && rustup toolchain list 2>/dev/null | grep -q "^nightly"; then
    # Check if nightly or pinned nightly exists
    if rustup toolchain list 2>/dev/null | grep -q "nightly-2025"; then
        # Use the pinned version if available
        PINNED="$(grep -E 'channel.*nightly' rust-toolchain.toml 2>/dev/null | sed -E 's/.*channel.*\"(nightly[^"]*)\".*/\1/' || echo nightly)"
        CARGO_CMD="cargo +${PINNED:-nightly}"
    else
        CARGO_CMD="cargo +nightly"
    fi
fi

# Parse optional flag
BUILD_MODE="debug"
BUILD_FLAG=""
if [[ "${1:-}" == "--release" ]]; then
    BUILD_MODE="release"
    BUILD_FLAG="--release"
    shift
elif [[ "${1:-}" == "--debug" ]]; then
    BUILD_MODE="debug"
    shift
fi

TARGET_SPEC="targets/x86_64-mfk.json"
if [[ ! -f "$TARGET_SPEC" ]]; then
    echo "ERROR: Target spec not found at $TARGET_SPEC" >&2
    exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "ERROR: cargo not found. Run ./install.sh first." >&2
    exit 1
fi

# Step 1: Build the kernel
# Note: -Zjson-target-spec was removed in cargo 1.91+ (stabilized); plain --target with path now works.
# We use -Zbuild-std only. If old cargo needs json flag, retry automatically.
echo "[1/2] Building kernel ($BUILD_MODE)..."
KERNEL_BUILD_CMD=("$CARGO_CMD" build -p mfk-kernel --target "$TARGET_SPEC" -Zbuild-std=core,alloc "-Zbuild-std-features=compiler-builtins-mem")
if [[ -n "$BUILD_FLAG" ]]; then KERNEL_BUILD_CMD+=("$BUILD_FLAG"); fi
echo "  ${KERNEL_BUILD_CMD[*]}"
if ! "${KERNEL_BUILD_CMD[@]}"; then
    # Fallback for very old cargo that still needs -Zjson-target-spec
    echo "  Retrying with -Zjson-target-spec for older cargo..." >&2
    if ! $CARGO_CMD build -p mfk-kernel --target "$TARGET_SPEC" -Zjson-target-spec -Zbuild-std=core,alloc "-Zbuild-std-features=compiler-builtins-mem" $BUILD_FLAG; then
        echo "" >&2
        echo "ERROR: Kernel build failed" >&2
        echo "Hints:" >&2
        # Extract pinned toolchain from file if present
        TOOLCHAIN_HINT="$(grep -E 'channel.*nightly' rust-toolchain.toml 2>/dev/null | sed -E 's/.*\"(nightly[^"]*)\".*/\1/' || echo nightly)"
        echo "  - Ensure pinned nightly is installed: rustup toolchain install $TOOLCHAIN_HINT" >&2
        echo "  - Ensure components: rustup component add rust-src llvm-tools-preview --toolchain $TOOLCHAIN_HINT" >&2
        echo "  - Try: cargo clean && ./build.sh" >&2
        exit 1
    fi
fi

if [[ "$BUILD_MODE" == "release" ]]; then
    KERNEL_BIN="target/x86_64-mfk/release/mfk-kernel"
else
    KERNEL_BIN="target/x86_64-mfk/debug/mfk-kernel"
fi

if [[ ! -f "$KERNEL_BIN" ]]; then
    echo "ERROR: Expected kernel binary not found at $KERNEL_BIN" >&2
    exit 1
fi
echo "✓ Kernel build complete: $KERNEL_BIN ($(du -h "$KERNEL_BIN" | cut -f1))"
echo ""

# Step 2: Build the runner (host toolchain, no nightly needed)
echo "[2/2] Building runner..."
if ! cargo build -p mfk-runner --release; then
    echo "ERROR: Runner build failed" >&2
    exit 1
fi
echo "✓ Runner build complete: target/release/mfk-runner"
echo ""

echo "======================================"
echo "Build Complete! ($BUILD_MODE)"
echo "======================================"
echo ""
echo "Next, run the kernel:"
echo "  ./run.sh                        # VirtualBox (default)"
echo "  ./run.sh --qemu                 # QEMU fallback (no VirtualBox needed)"
echo "  ./run.sh $KERNEL_BIN --vbox"
echo ""
