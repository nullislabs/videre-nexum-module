# videre

The videre venue platform for the [nexum runtime](https://github.com/nullislabs/nexum-runtime): the venue registry service, the `VenueInvoker` adapter seam, the guest-side keeper SDK, and the `videre:venue` WIT surface. Carved out of the nullislabs monorepo with full history; the engine composition root lives in [shepherd](https://github.com/nullislabs/shepherd).

A venue is a native Rust adapter. It implements `videre_host::VenueInvoker`, and the composition root registers it with `VenueRegistry::register`. A venue was a guest wasm component until the runtime deleted the extension-installed component path, so an extension can no longer install or supervise a guest. A keeper stays a guest wasm component and reaches a venue through `videre:venue/client`.

## Layout

| Path | What it is |
|---|---|
| `crates/videre-host` | The videre platform as one nexum-runtime extension: the venue registry service, the `VenueInvoker` adapter seam, the venue-routing policy, the advisory egress-guard seam, and the `videre:venue/client` interface. |
| `crates/videre-sdk` | Guest-side SDK: the borsh-versioned `IntentBody` codec, the typed venue client, and the keeper run assembler. |
| `crates/videre-macros` | Proc-macro glue for the guest authoring path, plus `derive(IntentBody)`. |
| `crates/videre-status-body` | Versioned codec for the opaque status body the host trigger stream carries. |
| `crates/videre-test` | Conformance kit for venue adapters: codec round-trip vectors, header-derivation golden fixtures, and mock transports. |
| `crates/no-std-probe` | Compile-only `#![no_std]` probe for the `IntentBody` derive. |
| `echo-client`, `echo-keeper` | The reference keeper guest modules. Each one drives a venue through `videre:venue/client`. |
| `echo-venue`, `flaky-venue` | The reference venue adapter and the evil-by-design venue fixture for the recovery tests. Both are native Rust; the workspace `members` list names the directory of each. |
| `wit/` | The `videre:venue`, `videre:types`, and `videre:value-flow` WIT packages, plus vendored cross-repo deps under `wit/deps/`. |
| `extensions.toml` | Client-capability registry: the per-namespace rows the component world synthesis emits beyond the core `nexum:host` table. |

## Development

The devshell pins the toolchain to match CI:

```sh
nix develop          # or `direnv allow` once
just build           # host-side workspace build
just build-modules   # the guest module wasms (wasm32-wasip2, release)
just test            # nextest suite
just ci              # full local CI mirror
```

Cross-repo dependencies on the nexum runtime are pinned by git rev in the crate manifests; bump them in lock-step with the vendored `wit/deps/` tree (`wit/deps.toml`).

## Licence

AGPL-3.0. See [LICENSE](LICENSE).
