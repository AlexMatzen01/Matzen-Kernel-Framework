#!/bin/bash

rustup toolchain install nightly

rustup component add rust-src llvm-tools-preview --toolchain nightly

cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem

cargo build -p mfk-runner --release