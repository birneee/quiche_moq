#!/usr/bin/env bash
# What CI checks, run inside `nix develop`.
set -euo pipefail

# the flake toolchain's clippy, cargo would prefer a rustup proxy in ~/.cargo/bin
clippy="$(dirname "$(readlink -f "$(command -v rustc)")")/cargo-clippy"

"$clippy" clippy --workspace --all-targets -- -D warnings
"$clippy" clippy -p moq_web --target wasm32-unknown-unknown -- -D warnings
cargo test --workspace
# moq_core, moq_wire and moq_wire_primitives use only core and alloc: build them for bare metal
cargo build -p moq_core -p moq_wire -p moq_wire_primitives --no-default-features --target thumbv7em-none-eabihf
# doc links and comments of the core crates
RUSTDOCFLAGS="-D warnings" cargo doc -p moq_core -p moq_wire -p moq_wire_primitives --no-deps
