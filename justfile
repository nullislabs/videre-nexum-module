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
        -p echo-venue -p echo-client -p echo-keeper -p flaky-venue

# Run the test suite.
test:
    cargo nextest run --workspace --all-features

# Rustfmt check.
fmt:
    cargo fmt --all -- --check

# Clippy over everything, warnings denied.
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Re-resolve the WIT deps and fail on any drift against the vendored
# wit/deps/ tree. Mirrors the wit-sync CI job. Needs the wit-deps
# binary, which is not in the dev shell: install the version CI pins,
# `cargo install --locked wit-deps-cli@0.6.0`, or reuse the pinned
# release binary the CI job downloads. An older or newer wit-deps can
# write a different deps.lock and turn CI red.
wit-sync:
    #!/usr/bin/env bash
    set -euo pipefail
    wit-deps update
    git add -N wit
    git --no-pager diff --exit-code -- wit

# Run the full CI series locally before pushing. Mirrors
# .github/workflows/ci.yml: rustfmt, clippy, rustdoc, the
# module wasms the integration tests need, and the workspace test
# suite via nextest plus the doctests, all under the `-D warnings` the
# CI workflow sets globally. The wit-sync CI job is not in this
# recipe because wit-deps is not in the dev shell; run `just wit-sync`
# when you touch wit/deps.toml or the vendored tree.
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
        -p echo-venue -p echo-client -p echo-keeper -p flaky-venue
    # nextest for the suite (as CI does); doctests run separately since nextest
    # does not cover them.
    cargo nextest run --workspace --all-features --no-fail-fast
    cargo test --doc --workspace --all-features
