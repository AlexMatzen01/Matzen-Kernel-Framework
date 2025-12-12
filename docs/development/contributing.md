# Contributing Guide

How to contribute to MFK.

## Code of Conduct

Be respectful, inclusive, and constructive in all interactions.

## Getting Started

1. **Fork the repository** on GitHub
2. **Clone your fork**: `git clone https://github.com/YOUR_USERNAME/Matzen-Kernel-Framework`
3. **Create a branch**: `git checkout -b feature/my-feature`
4. **Set up environment**: See [Development Setup](setup.md)
5. **Make changes** and test thoroughly
6. **Commit with clear messages**: See Commit Guidelines below
7. **Push to your fork**: `git push origin feature/my-feature`
8. **Open a Pull Request** on the main repository

## Development Process

### Before Starting

1. Check [GitHub Issues](https://github.com/matzen-kernel/mfk/issues) for existing work
2. Create an issue for your feature (or claim existing one)
3. Wait for feedback before starting large work
4. Discuss approach if it's a significant change

### During Development

1. **Keep commits small and focused** — Each commit should be one logical change
2. **Write tests as you go** — See [Testing Guide](testing.md)
3. **Update documentation** — Docs are as important as code
4. **Run regression tests**: `./regression_tests.sh`
5. **Test on multiple systems** if possible

### Code Style

#### Rust Style

Follow Rust conventions:

```rust
// Good: Clear, idiomatic Rust
pub fn process_packet(packet: &[u8]) -> Result<(), &'static str> {
    if packet.is_empty() {
        return Err("Empty packet");
    }
    
    for byte in packet {
        handle_byte(*byte)?;
    }
    
    Ok(())
}

// Avoid: C-style code in Rust
pub unsafe fn process_packet(packet: *const u8, len: usize) {
    for i in 0..len {
        let byte = *packet.add(i);
        // ...
    }
}
```

#### Naming Conventions

```rust
// Types: PascalCase
pub struct NetworkDriver { }

// Functions: snake_case
pub fn initialize_driver() { }

// Constants: UPPER_SNAKE_CASE
pub const MAX_PACKET_SIZE: usize = 65535;

// Variables: snake_case
let current_state = DeviceState::Ready;
```

#### Comments

```rust
// Single line: Use // for code explanation
let packet_size = 1500;  // Ethernet MTU

/// Doc comments: Use /// for public items
/// 
/// Initializes the network driver.
pub fn init() -> Result<(), &'static str> {
    // Implementation notes use //
    // ...
}

//! Module documentation at top of file
//! 
//! This module handles network communication
```

#### Formatting

Use `rustfmt`:

```bash
# Format your code
cargo fmt

# Format just kernel
cargo fmt -p mfk-kernel

# Check formatting without changing
cargo fmt --check
```

#### Linting

Use `clippy`:

```bash
# Check for common mistakes
cargo clippy -p mfk-kernel

# Fix automatically
cargo clippy -p mfk-kernel --fix
```

## Commit Guidelines

### Commit Messages

```
First line: Brief summary (50 chars max)
            
Longer explanation if needed, wrapped at 72
characters. Explain WHAT and WHY, not HOW.

Fixes: #123  # Reference related issues
```

**Good commits:**
```
Add static DMA buffers to E1000 driver

Previously, the E1000 driver used heap-allocated
buffers that had invalid physical addresses. This
caused packets to be sent to wrong memory locations.

Switch to static buffers with proper alignment to
ensure physical addresses match kernel's identity
mapping.

Fixes: #45
```

**Bad commits:**
```
Fix network

Bug fix
```

### Commit Organization

Each commit should:
- ✅ Be focused on one thing
- ✅ Have a clear message
- ✅ Pass tests
- ✅ Not break existing functionality

### Atomic Commits

Good: Each commit is independently testable
```
Commit 1: Add IP address configuration function
Commit 2: Call IP configuration at boot
Commit 3: Add test for IP configuration
```

Avoid: Breaking changes across commits
```
Commit 1: Refactor IP module (breaks tests)
Commit 2: Update shell commands
Commit 3: Fix tests
```

## Pull Request Process

### Before Opening PR

- [ ] Code compiles: `./build.sh`
- [ ] No errors: `cargo check -p mfk-kernel`
- [ ] No warnings: `cargo clippy -p mfk-kernel`
- [ ] Formatted: `cargo fmt -p mfk-kernel`
- [ ] Tests pass: `cargo test -p mfk-kernel`
- [ ] Integration works: `./regression_tests.sh`
- [ ] Documentation updated
- [ ] Commits are clean and focused

### PR Description Template

```markdown
## Description
Brief description of changes.

## Type of Change
- [ ] Bug fix
- [ ] New feature
- [ ] Documentation
- [ ] Performance improvement

## Motivation
Why are these changes needed?

## Testing
How was this tested?

## Checklist
- [ ] Tests pass
- [ ] Documentation updated
- [ ] No breaking changes
- [ ] Commits are clean
- [ ] Code follows style guidelines

## Related Issues
Fixes #123
Related to #456
```

### Review Process

**What reviewers look for:**
- Code correctness
- Performance implications
- Memory safety
- Test coverage
- Documentation quality
- Adherence to style guide

**Addressing feedback:**
- Respond to all comments
- Push new commits for changes
- Don't rewrite history (no force push)
- Request re-review after changes

### Merging

Maintainers will merge when:
- ✅ All tests pass
- ✅ At least one approval
- ✅ No requested changes
- ✅ Commits are clean

## Types of Contributions

### Bug Fixes

1. **Identify the issue** — What's wrong?
2. **Write a test** — Reproduce the bug
3. **Fix the code** — Minimal change
4. **Verify** — Test passes

**Example:**
```rust
#[test]
fn test_packet_checksum_overflow() {
    // This test would fail before the fix
    let packet = create_packet_with_large_payload();
    let checksum = calculate_checksum(&packet);
    assert!(checksum > 0);
}
```

### Features

1. **Discuss design** — Is this the right approach?
2. **Implement gradually** — Start small
3. **Test thoroughly** — Especially edge cases
4. **Document** — Add examples and explanation

### Documentation

1. **Identify gaps** — What's not documented?
2. **Write clearly** — Assume reader knows less than you
3. **Include examples** — Show real usage
4. **Update index** — Link from navigation

### Performance Improvements

1. **Measure current** — Baseline first
2. **Identify bottleneck** — Use profiling tools
3. **Implement carefully** — Don't sacrifice clarity
4. **Benchmark improvement** — Show performance gain

## Review Criteria

### What Gets Approved

✅ **Code Quality**
- Clear and readable
- Follows conventions
- Properly commented
- No dead code

✅ **Functionality**
- Solves stated problem
- Doesn't break anything
- Has adequate tests
- Handles errors

✅ **Performance**
- No regressions
- Efficient algorithms
- Minimal allocations
- No busy-waiting

✅ **Documentation**
- Code is commented
- Features documented
- README updated
- Examples included

### What Gets Rejected

❌ **Incomplete**
- Missing tests
- No documentation
- Doesn't compile
- Breaks existing tests

❌ **Poor Quality**
- Unreadable code
- No error handling
- Excessive complexity
- Performance issues

❌ **Controversial**
- Large refactors without discussion
- Breaking API changes
- Significant scope creep

## Issue Labels

- `bug` — Something is broken
- `enhancement` — New feature request
- `documentation` — Docs improvement
- `good first issue` — For new contributors
- `help wanted` — Looking for volunteers
- `discussion` — Seeking input
- `performance` — Speed improvement

## Release Process

Current version: 0.1.0

Release checklist:
- [ ] All issues resolved
- [ ] Tests pass
- [ ] Documentation updated
- [ ] Version bumped (semver)
- [ ] CHANGELOG updated
- [ ] Tag created

## Becoming a Maintainer

Active contributors may be invited to maintain MFK. Maintainers:
- Review and merge PRs
- Triage issues
- Make design decisions
- Release new versions

## Questions?

- Check [Documentation](../index.md)
- Read existing code for examples
- Open an issue with `question` label
- Start a Discussion on GitHub

## Thank You!

Your contributions make MFK better. We appreciate:
- Bug reports with details
- Feature proposals with use cases
- Documentation improvements
- Code contributions
- Testing on different systems
- Helping other contributors

---

**Quick Links:**
- [Main Documentation](../index.md)
- [Building Guide](building.md)
- [Testing Guide](testing.md)
- [Extending Guide](extending.md)
