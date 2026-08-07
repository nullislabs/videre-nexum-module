# videre

The videre venue platform for the [nexum runtime](https://github.com/nullislabs/nexum-runtime): a venue-adapter provider kind, the venue registry service, the guest-side venue SDK, and the `videre:venue` WIT surface. Carved out of the nullislabs monorepo with full history; the engine composition root lives in [shepherd](https://github.com/nullislabs/shepherd).

## Layout

| Path | What it is |
|---|---|
| `crates/videre-host` | The videre platform as one nexum-runtime extension: the venue-adapter provider kind, the venue registry service, the advisory egress-guard seam, and the `videre:venue/client` interface. |
| `crates/videre-sdk` | Guest-side SDK: the `VenueAdapter` trait, the borsh-versioned `IntentBody` codec, the typed venue client, and the keeper run assembler. |
| `crates/videre-macros` | Proc-macro glue: `#[venue]`, `#[keeper]`, and `derive(IntentBody)`. |
| `crates/videre-status-body` | Versioned codec for the opaque status body the host event stream carries. |
| `crates/videre-test` | Conformance kit for venue adapters: codec round-trip vectors, header-derivation golden fixtures, and mock transports. |
| `crates/no-std-probe` | Compile-only `#![no_std]` probe for the `IntentBody` derive. |
| `modules/examples/` | The echo-venue reference adapter and its paired echo-client / echo-keeper modules. |
| `modules/fixtures/flaky-venue` | Evil-by-design adapter fixture for supervisor recovery tests. |
| `modules/fixtures/logging-venue` | Adapter fixture declaring the `logging` capability, for the venue logging tests. |
| `modules/venues/postage-venue` | Swarm postage venue adapter: BZZ-for-storage header, `requires-signing` over the `createBatch` purchase. |
| `wit/` | The `videre:venue`, `videre:types`, and `videre:value-flow` WIT packages, plus vendored cross-repo deps under `wit/deps/`. |
| `extensions.toml` | Client-capability registry: the per-namespace rows the module world synthesis emits beyond the core `nexum:host` table. |
| `docs/` | Architecture decision records under `docs/adr/`; design documents and runbooks under `docs/design/`. |

## Development

The devshell pins the toolchain to match CI:

```sh
nix develop          # or `direnv allow` once
just build           # host-side workspace build
just build-modules   # guest wasms (wasm32-wasip2, release)
just test            # nextest suite
just ci              # full local CI mirror
```

Cross-repo dependencies on the nexum runtime are pinned by git rev in the crate manifests; bump them in lock-step with the vendored `wit/deps/` tree (`wit/deps.toml`).

## Licence

AGPL-3.0. See [LICENSE](LICENSE).
