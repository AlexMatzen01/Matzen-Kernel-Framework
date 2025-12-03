KERNEL_LIB?=target/x86_64-mfk/debug/libmfk_kernel.rlib
QEMU?=qemu-system-x86_64

.PHONY: all build run clean fmt clippy

all: build

build:
	cargo build --lib --target targets/x86_64-mfk.json -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem

# Note: bootimage tool doesn't work with lib-only targets in bootloader 0.9.x
# The kernel lib compiles successfully at $(KERNEL_LIB)
# To create a bootable image, you'll need to use bootloader 0.11+ or manually link

run: image
	./scripts/run-qemu.sh $(BOOTIMAGE)

clean:
	cargo clean

fmt:
	cargo fmt --all

clippy:
	cargo clippy --all --target targets/x86_64-mfk.json
