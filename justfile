# Build the host-side workspace (host extension, SDK, macros, test kit).
build:
    cargo build --workspace

# Build the reference venue adapter (echo-venue) for wasm32-wasip2. Its
# per-component world pins the #[videre_sdk::venue] acceptance test.
build-venue:
    cargo build --target wasm32-wasip2 --release -p echo-venue

# Build every guest module wasm in this repo (examples + fixtures).
build-modules:
    cargo build --target wasm32-wasip2 --release \
        -p echo-venue -p echo-client -p echo-keeper -p flaky-venue -p logging-venue

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
        -p echo-venue -p echo-client -p echo-keeper -p flaky-venue -p logging-venue
    # nextest for the suite (as CI does); doctests run separately since nextest
    # does not cover them.
    cargo nextest run --workspace --all-features --no-fail-fast
    cargo test --doc --workspace --all-features
