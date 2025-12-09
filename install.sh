#!/bin/bash
rustup toolchain install nightly
rustup component add rust-src llvm-tools-preview --toolchain nightly