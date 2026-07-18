# Build the 4 videre guest WASM modules: the reference venue adapter
# (echo-venue), its client/keeper pair, and the fault fixture.
build-module:
    cargo build --release --target wasm32-wasip2 \
        -p echo-venue -p echo-client -p echo-keeper -p flaky-venue

# Run the workspace tests (includes videre-host/tests/zero_leak.rs).
test:
    cargo test --workspace --all-features --no-fail-fast

# Check the workspace.
check:
    cargo check --workspace

# Run the full CI series locally before pushing. Mirrors
# .github/workflows/ci.yml one-to-one: rustfmt, clippy, rustdoc, the
# module wasms the integration tests need, and the workspace test
# suite, all under the `-D warnings` the CI workflow sets globally.
ci:
    #!/usr/bin/env bash
    set -euo pipefail
    export RUSTFLAGS="-D warnings"
    export RUSTDOCFLAGS="-D warnings"
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo doc --workspace --no-deps
    cargo build --release --target wasm32-wasip2 \
        -p echo-venue -p echo-client -p echo-keeper -p flaky-venue
    cargo test --workspace --all-features --no-fail-fast
