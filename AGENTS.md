# AGENTS.md

`CLAUDE.md` is a symlink to this file.

## What videre is

Videre is the venue platform for the [nexum runtime](https://github.com/nullislabs/nexum-runtime), and it is layer 2 of a three-repo stack.
The nexum runtime is the generic layer 1, and [shepherd](https://github.com/nullislabs/shepherd) is the top layer that holds the engine composition root.
Videre plugs into the runtime through the `Extension` seam: `videre_host::platform()` returns a `Videre` value that implements `nexum_runtime::extension::Extension`.
A composition root wires it with `builder.with_extensions([Arc::new(videre_host::platform())])`.

A venue in this codebase is a native Rust adapter that implements the `VenueInvoker` trait from `videre-host`.
The composition root registers each venue on the `VenueRegistry` with `VenueRegistry::register`.
That call takes the venue id, a shared `Liveness` flag, the body-schema versions the venue decodes, and the invoker itself.
A venue was a guest wasm component of kind `venue-adapter` until the runtime deleted the extension-installed component path.
An extension can no longer install or supervise a guest component, so the adapter seam moved in-process.
A keeper is still a guest wasm component, and it calls a registered venue through the `videre:venue/client` interface.

## Layout

- `crates/videre-host`: the platform as one runtime extension: the `VenueRegistry` service, the `VenueInvoker` seam a native venue implements, the venue-routing policy in `videre_host::policy`, the advisory `EgressGuard` seam, and the host side of `videre:venue/client`.
- `crates/videre-sdk`: the guest-side SDK: the borsh-versioned `IntentBody` codec, the typed venue client, and the keeper run assembler.
- `crates/videre-macros`: the proc macros for the guest authoring path, plus `derive(IntentBody)`; `crates/no-std-probe` is the compile-only `#![no_std]` probe for that derive.
- `crates/videre-status-body`: the versioned codec for the opaque status body that the host trigger stream carries.
- `crates/videre-test`: the conformance kit for venue adapters: codec round-trip vectors, header-derivation golden fixtures, and mock transports.
- `echo-client` and `echo-keeper`: the reference keeper guest modules, each one driving a venue through `videre:venue/client`.
- `echo-venue` and `flaky-venue`: the reference venue adapter and the evil-by-design venue fixture for the recovery tests.
- `wit/`: the `videre:venue`, `videre:types`, and `videre:value-flow` WIT packages, plus the cross-repo deps vendored under `wit/deps/`.

`echo-venue` and `flaky-venue` are native Rust, so they no longer build to wasm.
Read the `members` list in the workspace `Cargo.toml` for the directory each crate lives in.

`extensions.toml` is the client-capability registry for this composition root: it declares the per-namespace rows that component world synthesis emits beyond the core `nexum:host` table.
Its `client` row maps a `[dependencies]` declaration to the `videre:venue/client@0.1.0` import, and names the WIT packages that the resolve path needs.

A guest module carries a `component.toml` manifest.
`[component]` holds its identity, `[dependencies]` declares what the runtime links, and each `[[trigger]]` row names with `on =` what wakes it.
The runtime renamed all of these: the file was `module.toml`, the tables were `[module]`, `[capabilities]`, and `[[subscription]] kind =`, the fuel key was `max_fuel_per_event`, and `[component].kind` is retired.

## Build, test, lint

The workspace uses Rust edition 2024, and the toolchain is pinned to 1.94 in both `flake.nix` and `.github/actions/rust-setup`.
Run `nix develop` first, or run `direnv allow` once, to get that toolchain with `cargo-nextest`, `wasm-tools`, `wabt`, `just`, `ripgrep`, and `ast-grep`.

```sh
just build           # cargo build --workspace
just build-modules   # every guest module wasm this repo still ships
just test            # cargo nextest run --workspace --all-features
just fmt             # cargo fmt --all -- --check
just lint            # cargo clippy --workspace --all-targets --all-features -- -D warnings
just ci              # the full local mirror of .github/workflows/ci.yml
just wit-sync        # check wit/deps/ against wit/deps.toml (needs wit-deps)
```

Nextest does not run doctests, so run `cargo test --doc --workspace --all-features` after `just test`.
The integration tests load the guest module wasms from `target/wasm32-wasip2/release/`, so run `just build-modules` before the suite.
A native venue needs no wasm build, so `just build-modules` covers the keeper modules and any guest fixture that is left.
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
An extension contributes four things and no more: a linker import, a capability namespace, the manifest sections it claims, and the trigger sources it opens.
It cannot install a guest component, supervise one, or route a call from one guest to another, because the runtime deleted those paths.
Videre therefore owns `SubmitQuota`, `WatchLimit`, and `Liveness` in `videre_host::policy`: the runtime retired `[limits.quota]` and `[limits.watch]`, because both described venue semantics rather than runtime semantics.
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
