# Build the host-side workspace (host extension, SDK, macros, test kit).
build:
    cargo build --workspace

# Build every guest module wasm in this repo. The venues are native Rust
# crates now, so only the echo-client and echo-keeper modules build for
# wasm.
build-modules:
    cargo build --target wasm32-wasip2 --release \
        -p echo-client -p echo-keeper

# Run the test suite.
test:
    cargo nextest run --workspace --all-features

# Rustfmt check.
fmt:
    cargo fmt --all -- --check

# Clippy over everything, warnings denied.
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run the full CI series locally before pushing. Mirrors
# .github/workflows/ci.yml one-to-one: rustfmt, clippy, rustdoc, the
# module wasms the integration tests need, and the workspace test
# suite via nextest plus the doctests, all under the `-D warnings` the
# CI workflow sets globally.
ci:
    #!/usr/bin/env bash
    set -euo pipefail
    # Append -D warnings without clobbering the devshell's flags (mold linker,
    # set in flake.nix), so the local run keeps fast native linking. RUSTC_WRAPPER
    # is already sccache from the devshell shellHook.
    export RUSTFLAGS="${RUSTFLAGS:-} -D warnings"
    export RUSTDOCFLAGS="${RUSTDOCFLAGS:-} -D warnings"
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo doc --workspace --no-deps
    cargo build --release --target wasm32-wasip2 \
        -p echo-client -p echo-keeper
    # nextest for the suite (as CI does); doctests run separately since nextest
    # does not cover them.
    cargo nextest run --workspace --all-features --no-fail-fast
    cargo test --doc --workspace --all-features
