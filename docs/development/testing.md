# Testing Guide

How to validate MFK changes and add tests.

## Overview

MFK testing spans:
1. **Unit tests** — Test individual modules in isolation
2. **Integration tests** — Test kernel features end-to-end in QEMU
3. **Manual testing** — Interactive testing via shell
4. **Regression testing** — Verify fixes don't break existing features

## Unit Testing

### Writing Unit Tests

Tests are defined in the same file as code using `#[test]` attribute:

```rust
// kernel/src/allocator.rs
pub struct BumpAllocator { /* ... */ }

impl BumpAllocator {
    pub fn allocate(&mut self, layout: Layout) -> *mut u8 {
        // implementation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_single_block() {
        let mut alloc = BumpAllocator::new();
        let ptr = alloc.allocate(Layout::new::<u32>());
        assert!(!ptr.is_null(), "Allocation should not be null");
    }

    #[test]
    fn test_allocate_multiple_blocks() {
        let mut alloc = BumpAllocator::new();
        let ptr1 = alloc.allocate(Layout::new::<u32>());
        let ptr2 = alloc.allocate(Layout::new::<u64>());
        assert_ne!(ptr1, ptr2, "Different allocations should have different addresses");
    }
}
```

### Running Unit Tests

```bash
# Run all tests
cargo test -p mfk-kernel

# Run tests for specific module
cargo test -p mfk-kernel allocator::

# Run with output
cargo test -p mfk-kernel -- --nocapture

# Run single test
cargo test -p mfk-kernel test_allocate_single_block

# Release mode tests
cargo test -p mfk-kernel --release
```

### Example: Testing IP Address Parsing

```rust
// kernel/src/net/ip.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ip_address_from_bytes() {
        let addr = IpAddress([192, 168, 1, 1]);
        assert_eq!(addr.octets[0], 192);
        assert_eq!(addr.octets[1], 168);
        assert_eq!(addr.octets[2], 1);
        assert_eq!(addr.octets[3], 1);
    }

    #[test]
    fn test_ip_address_to_u32() {
        let addr = IpAddress([10, 0, 2, 15]);
        let bytes = addr.to_bytes();
        assert_eq!(bytes, 0x0F02000A);  // Little-endian
    }

    #[test]
    fn test_ip_address_display() {
        let addr = IpAddress([192, 168, 0, 1]);
        assert_eq!(format!("{}", addr), "192.168.0.1");
    }
}
```

## Integration Testing

### Testing in QEMU

Integration tests run the full kernel in QEMU and validate behavior.

**Basic integration test:**

```bash
# Build kernel
./build.sh

# Run with test commands
echo -e "help\nhalt" | ./run.sh target/x86_64-mfk/release/mfk-kernel
```

### Automated Test Scripts

Create shell scripts to validate kernel features:

```bash
#!/bin/bash
# test_networking.sh

QEMU="./run.sh target/x86_64-mfk/release/mfk-kernel"
TIMEOUT=10

# Test 1: Network initializes
echo "Test 1: Network initialization"
OUTPUT=$($QEMU <<EOF
ifconfig
halt
EOF
)

if echo "$OUTPUT" | grep -q "IP Address:.*10.0.2.15"; then
    echo "✓ PASS: Network auto-configured"
else
    echo "✗ FAIL: Network not configured"
    exit 1
fi

# Test 2: Ping works
echo "Test 2: Ping gateway"
OUTPUT=$($QEMU <<EOF
ping 10.0.2.2 2
halt
EOF
)

if echo "$OUTPUT" | grep -q "Received.*reply"; then
    echo "✓ PASS: Ping successful"
else
    echo "✗ FAIL: Ping failed"
    exit 1
fi

echo "All tests passed!"
```

Run test script:
```bash
chmod +x test_networking.sh
./test_networking.sh
```

### Testing File System

```bash
#!/bin/bash
# test_filesystem.sh

OUTPUT=$("./run.sh target/x86_64-mfk/release/mfk-kernel" <<EOF
mkfs 1 --yes
mount 1
write test.txt "Hello, World!"
read test.txt
ls
halt
EOF
)

if echo "$OUTPUT" | grep -q "Hello, World!"; then
    echo "✓ File system works"
else
    echo "✗ File system broken"
    exit 1
fi
```

### Testing Shell Commands

```bash
#!/bin/bash
# test_shell.sh

COMMANDS=(
    "help"
    "echo hello"
    "memory"
    "diskinfo"
    "ls"
)

for cmd in "${COMMANDS[@]}"; do
    echo "Testing: $cmd"
    OUTPUT=$("./run.sh target/x86_64-mfk/release/mfk-kernel" <<EOF
$cmd
halt
EOF
)
    
    if [ $? -eq 0 ]; then
        echo "✓ PASS: $cmd"
    else
        echo "✗ FAIL: $cmd"
        exit 1
    fi
done
```

## Test Coverage

### Checking Coverage

Use `tarpaulin` to measure test coverage:

```bash
# Install
cargo install cargo-tarpaulin

# Run with coverage
cargo tarpaulin -p mfk-kernel --out Html --output-dir coverage

# View coverage
open coverage/tarpaulin-report.html  # macOS
firefox coverage/tarpaulin-report.html  # Linux
```

### Target Coverage Areas

**Priority 1 (Essential):**
- Allocator (memory safety critical)
- Interrupt handling
- Network core (IP, ARP)

**Priority 2 (Important):**
- Drivers (VGA, keyboard, serial)
- File system
- Shell parsing

**Priority 3 (Nice-to-have):**
- Utilities (string parsing, formatting)
- Advanced drivers (ATA)

## Regression Testing

### Tracking Regressions

Create a regression test file to validate known-working features after changes:

```bash
#!/bin/bash
# regression_tests.sh
# Run after ANY kernel change to ensure nothing broke

FAILED=0

echo "=== Regression Test Suite ==="

# Test 1: Kernel boots
echo -n "Test: Boot... "
if ./run.sh target/x86_64-mfk/release/mfk-kernel <<EOF 2>&1 | grep -q "Starting shell"; then
    echo "✓"
else
    echo "✗"
    ((FAILED++))
fi

# Test 2: Help command
echo -n "Test: Help command... "
if ./run.sh target/x86_64-mfk/release/mfk-kernel <<EOF 2>&1 | grep -q "Available commands"; then
    echo "✓"
else
    echo "✗"
    ((FAILED++))
fi

# Test 3: Memory command
echo -n "Test: Memory info... "
if ./run.sh target/x86_64-mfk/release/mfk-kernel <<EOF 2>&1 | grep -q "Heap"; then
    echo "✓"
else
    echo "✗"
    ((FAILED++))
fi

# Test 4: File system
echo -n "Test: File system... "
if ./run.sh target/x86_64-mfk/release/mfk-kernel <<EOF 2>&1 | grep -q "mkfs"; then
    echo "✓"
else
    echo "✗"
    ((FAILED++))
fi

# Test 5: Network
echo -n "Test: Network init... "
if ./run.sh target/x86_64-mfk/release/mfk-kernel <<EOF 2>&1 | grep -q "10.0.2.15"; then
    echo "✓"
else
    echo "✗"
    ((FAILED++))
fi

echo
echo "Regression tests: $((5 - FAILED))/5 passed"
exit $FAILED
```

Run before committing:
```bash
chmod +x regression_tests.sh
./regression_tests.sh
```

## Debugging Failed Tests

### Verbose Output

Run tests with output:
```bash
# Unit tests
cargo test -p mfk-kernel -- --nocapture --test-threads=1

# Integration tests
./run.sh target/x86_64-mfk/release/mfk-kernel 2>&1 | head -100
```

### Using GDB

Debug kernel with GDB (requires QEMU with gdb stub):

```bash
# In one terminal: Run QEMU waiting for GDB
qemu-system-x86_64 -gdb tcp::1234 -S -kernel target/x86_64-mfk/release/mfk-kernel ...

# In another terminal: Attach GDB
gdb target/x86_64-mfk/release/mfk-kernel
(gdb) target remote localhost:1234
(gdb) break kernel_main
(gdb) continue
(gdb) step
```

### Adding Debug Asserts

Add debug assertions that help identify issues:

```rust
// In allocator.rs
pub fn allocate(&mut self, layout: Layout) -> *mut u8 {
    // Debug-only assertion
    debug_assert!(layout.size() > 0, "Cannot allocate 0 bytes");
    debug_assert!(layout.align() > 0, "Alignment must be > 0");
    
    // ... allocation logic
}
```

These are removed in release builds but helpful during development.

## Performance Testing

### Benchmarking Allocator

```rust
#[bench]
fn bench_allocation(b: &mut Bencher) {
    let mut alloc = BumpAllocator::new();
    let layout = Layout::new::<u64>();
    b.iter(|| {
        alloc.allocate(layout)
    });
}
```

Run with:
```bash
cargo test --release -- --nocapture --test-threads=1
```

### Measuring Boot Time

```bash
#!/bin/bash
# measure_boot.sh

KERNEL="target/x86_64-mfk/release/mfk-kernel"

echo "Measuring kernel boot time..."
time ./run.sh "$KERNEL" <<EOF
halt
EOF
```

### Testing Load Time

```bash
#!/bin/bash
# load_test.sh - Write many files

./run.sh target/x86_64-mfk/release/mfk-kernel <<EOF
mkfs 1 --yes
mount 1

write file1.txt "content"
write file2.txt "content"
write file3.txt "content"
write file4.txt "content"
write file5.txt "content"

time ls

halt
EOF
```

## Continuous Integration

### GitHub Actions Setup

```yaml
name: Tests

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      
      - uses: actions-rs/toolchain@v1
        with:
          profile: minimal
          toolchain: nightly
      
      - run: |
          rustup component add rust-src llvm-tools-preview
          sudo apt-get update
          sudo apt-get install -y qemu-system-x86
      
      - run: cargo test -p mfk-kernel
      
      - run: ./build.sh
      
      - run: |
          chmod +x regression_tests.sh
          ./regression_tests.sh
```

Commit the workflow to `.github/workflows/tests.yml`.

## Test Checklist

Before submitting a pull request:

- [ ] `cargo test` passes with no errors
- [ ] `./build.sh` completes successfully
- [ ] `./regression_tests.sh` passes
- [ ] New unit tests added for new code
- [ ] No new compiler warnings
- [ ] Integration tested manually in QEMU
- [ ] Boot messages look normal (no panics)
- [ ] Help text updated if commands changed

## Testing Drivers

### Testing E1000 Driver

```bash
# Verify initialization
./run.sh target/x86_64-mfk/release/mfk-kernel <<EOF
ifconfig
halt
EOF

# Check for: MAC address shown, IP configured
```

### Testing Keyboard

```bash
# Manual: Type on QEMU window and verify input appears
./run.sh target/x86_64-mfk/release/mfk-kernel

# (Type commands, they should appear and work)
# Type: "help" <enter>
# Expected: See help text
```

### Testing VGA

```bash
# Verify screen output appears correctly
./run.sh target/x86_64-mfk/release/mfk-kernel

# (Visual inspection: colors, text positioning should be correct)
```

## Next Steps

- **[Extending Guide](extending.md)** — Add new features properly
- **[Building Guide](building.md)** — Understand build system
- **[Development Setup](setup.md)** — Configure your environment
