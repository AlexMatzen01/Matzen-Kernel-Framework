BOOTIMAGE?=target/x86_64-mfk/debug/bootimage-mfk-kernel.bin
QEMU?=qemu-system-x86_64

.PHONY: all build image run clean fmt clippy

all: run

build:
	cargo build -p mfk-kernel --target targets/x86_64-mfk.json

image:
	cargo bootimage -p mfk-kernel

run: image
	./scripts/run-qemu.sh $(BOOTIMAGE)

clean:
	cargo clean

fmt:
	cargo fmt --all

clippy:
	cargo clippy --all --target targets/x86_64-mfk.json
