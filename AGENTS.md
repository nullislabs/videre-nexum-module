# AGENTS.md

`CLAUDE.md` is a symlink to this file.

## What videre is

Videre is the venue platform for the [nexum runtime](https://github.com/nullislabs/nexum-runtime), and it is layer 2 of a three-repo stack.
The nexum runtime is the generic layer 1, and [shepherd](https://github.com/nullislabs/shepherd) is the top layer that holds the engine composition root.
Videre plugs into the runtime through the `Extension` seam: `videre_host::platform(&config)` returns a `Videre` value that implements `nexum_runtime::host::extension::Extension`.
A composition root wires it with `builder.with_extensions([Arc::new(videre_host::platform(cfg))])`.

A venue in this codebase is a guest wasm component of kind `venue-adapter`.
The adapter implements the `VenueAdapter` trait from `videre-sdk` and exports it through the `#[venue]` macro.
The host registers each adapter in the `VenueRegistry` and invokes it through the venue-adapter provider kind.
A keeper module calls a registered venue through the `videre:venue/client` interface.

## Layout

- `crates/videre-host`: the platform as one runtime extension: the venue-adapter provider kind, the `VenueRegistry` service, the advisory `EgressGuard` seam, and the host side of `videre:venue/client`.
- `crates/videre-sdk`: the guest-side SDK: the `VenueAdapter` trait, the borsh-versioned `IntentBody` codec, the typed venue client, and the keeper run assembler.
- `crates/videre-macros`: the proc macros `#[venue]`, `#[keeper]`, and `derive(IntentBody)`; `crates/no-std-probe` is the compile-only `#![no_std]` probe for that derive.
- `crates/videre-status-body`: the versioned codec for the opaque status body that the host event stream carries.
- `crates/videre-test`: the conformance kit for venue adapters: codec round-trip vectors, header-derivation golden fixtures, and mock transports.
- `modules/`: the echo-venue reference adapter with its paired echo-client and echo-keeper modules, plus two fixtures: the evil-by-design `flaky-venue` for supervisor recovery tests and `logging-venue` for the venue logging-capability tests.
- `wit/`: the `videre:venue`, `videre:types`, and `videre:value-flow` WIT packages, plus the cross-repo deps vendored under `wit/deps/`.

`extensions.toml` is the client-capability registry for this composition root: it declares the per-namespace rows that module world synthesis emits beyond the core `nexum:host` table.
Its `client` row maps a `[capabilities]` declaration to the `videre:venue/client@0.1.0` import, and names the WIT packages that the resolve path needs.

## Build, test, lint

The workspace uses Rust edition 2024, and the toolchain is pinned to 1.94 in both `flake.nix` and `.github/actions/rust-setup`.
Run `nix develop` first, or run `direnv allow` once, to get that toolchain with `cargo-nextest`, `wasm-tools`, `wabt`, `just`, `ripgrep`, and `ast-grep`.

```sh
just build           # cargo build --workspace
just build-modules   # every guest module wasm
just test            # cargo nextest run --workspace --all-features
just fmt             # cargo fmt --all -- --check
just lint            # cargo clippy --workspace --all-targets --all-features -- -D warnings
just ci              # the full local mirror of .github/workflows/ci.yml
```

Nextest does not run doctests, so run `cargo test --doc --workspace --all-features` after `just test`.
The integration tests load the module wasms from `target/wasm32-wasip2/release/`, so run `just build-modules` before the suite.
A missing wasm fails the test instead of skipping it; set `VIDERE_SKIP_MISSING_WASMS=1` to skip instead, which `CI` ignores.
`just fmt` and `just lint` are the pre-commit gate, and CI sets `-D warnings` globally, so fix every warning.

The hooks in `.claude/hooks/` support this loop: `rustfmt-on-edit.sh` formats each edited `.rs` file.
`nextest-on-stop.sh` runs nextest at the end of a turn, for the crates that own the changed `.rs` files.
`content-lint.sh` blocks an edit that adds an em-dash to a `.rs` or `.md` file.
The hooks run only on NixOS: each one exits at once when `/etc/NIXOS` is absent.
Each hook also exits without an effect when its tool is absent, so the hooks do nothing outside the dev shell.

## Repo boundary

This repo holds venue-domain code only.
A generic host seam, the component runtime, the manifest model, and the capability plumbing belong in nexum-runtime at layer 1.
The engine composition root, the binary, and the operator config belong in shepherd at the top layer.
Do not add a venue concept to nexum-runtime, and do not add a composition root here.
Cross-repo dependencies are pinned by git rev in the crate manifests: bump a pin in lock-step with the vendored `wit/deps/` tree that `wit/deps.toml` describes.

## House rules

Do not use em-dashes in source, in docs, in commit messages, or in a PR or issue body.
Use an ASCII hyphen, a colon, or split the sentence, because `.claude/hooks/content-lint.sh` blocks an em-dash in a `.rs` or `.md` file.
Write each commit message as a [Conventional Commit](https://www.conventionalcommits.org/) with an imperative subject.
Disclose AI assistance with one honest line in the commit message and in the PR body: `AI Assistance: <tool> used for <what>`.
Never add a `Co-Authored-By: Claude Code` trailer or a `Generated with Claude Code` line.
Keep one logical line per paragraph in a PR or issue body.

## Documentation

Write all documentation in ASD-STE100 Simplified Technical English.
Use short sentences, the active voice, and one idea per sentence.
In a markdown file, put each sentence on its own line and do not wrap within a sentence, because GitHub reflows the file when it displays it.
In a PR or issue body, keep one line per paragraph, because GitHub renders a single newline in a comment as a line break.
