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

# Re-resolve the WIT deps and fail on drift against the vendored tree.
# wit-deps is not in the dev shell: `cargo install --locked wit-deps-cli@0.6.0`.
wit-sync:
    #!/usr/bin/env bash
    set -euo pipefail
    want=0.6.0
    got="$(wit-deps --version | awk '{print $NF}')"
    if [ "$got" != "$want" ]; then
        echo "wit-deps $got found, CI pins $want: install the pinned version" >&2
        exit 1
    fi
    # A failed resolve deletes wit/deps/<pkg>: re-run it once the pins are right.
    wit-deps update
    if [ -n "$(git status --porcelain -- wit)" ]; then
        echo "wit/ drifted from wit/deps.toml:" >&2
        git status --short -- wit >&2
        git --no-pager diff -- wit >&2
        exit 1
    fi

# Run the full CI series locally before pushing. Mirrors
# .github/workflows/ci.yml: rustfmt, clippy, rustdoc, the module wasms
# the integration tests need, and the workspace test suite via nextest
# plus the doctests, all under the `-D warnings` the CI workflow sets
# globally. The wit-sync job is not in this recipe: run `just wit-sync`
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
        -p echo-client -p echo-keeper
    # nextest for the suite (as CI does); doctests run separately since nextest
    # does not cover them.
    cargo nextest run --workspace --all-features --no-fail-fast
    cargo test --doc --workspace --all-features
