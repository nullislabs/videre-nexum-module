# value-flow versioning policy

Status: live policy.
This note governs `videre:value-flow` from the 1.0.0 freeze on (tracker issue #31).
The package source is `wit/videre-value-flow/types.wit`, and that file is authoritative for the frozen surface.

## The frozen surface

`videre:value-flow@1.0.0` freezes the egress-neutral value vocabulary: `address`, `uint`, `erc20`, `service-desc`, the `asset` variant, and `asset-amount`.
The canonical `uint` encoding is fixed by ADR 0001, and the bare `native` case is fixed by ADR 0002.
The package stays EVM-only and dependency-free through 1.x.

## The strict rule

- A record may grow additively within 1.x: a new field is a minor bump, and an existing field never changes type, name, or meaning.
- Every variant is closed: the `asset` case set is exactly `native`, `erc20`, and `service`, and no 1.x release adds, removes, renames, or retypes a case.
- Any variant growth is 2.0.

Non-EVM asset kinds (erc721, erc1155, offchain) therefore wait for 2.0.
The asymmetry is deliberate.
A consumer that matches a WIT variant must handle every case, so a new case breaks every existing consumer at the ABI seam.
A new record field lowers into the canonical ABI without disturbing the existing fields, so record growth is safe for a consumer that ignores it.
Issue #22 records this caveat as the reason #29 (the `service` case) had to land before the freeze.

## The first test of the policy

Issue #32 (bind the quote on the submit wire) is the first additive consumer after the freeze.
It must land as an additive 1.x change under this rule, and it is the acceptance test of the policy: if #32 needs a variant case, the policy forces the 2.0 conversation instead of a silent break.

## How a bump ripples

A 1.x minor bump touches every consumer of the versioned name in lock-step:

1. `wit/videre-value-flow/types.wit`: the `package` line.
2. `wit/videre-types/types.wit`: the `use videre:value-flow/types@<v>` line.
3. `crates/videre-sdk/src/bindings.rs` and `crates/videre-host/src/bindings.rs`: the inline-world `import` lines.
4. `crates/videre-macros/src/lib.rs` and `crates/videre-macros/src/keeper.rs`: the `with` remap keys.
5. `crates/videre-test`: the freeze pin in `tests/value_flow_freeze.rs`, plus the golden mirrors when the surface grew.
6. The guest modules rebuild against the SDK; they carry no version literal of their own.
7. `docs/design/value-flow-versioning-policy.md`: this note, which names the frozen version.

A 2.0 additionally re-opens the variant sets, regenerates every golden fixture, and follows the cross-repo re-vendor runbook that issue #22 specifies (tracker issue #46 for the docs home).

## What a bump costs a consumer

Any change to the version, minor or major, is ABI-visible past this repository.
A `use` of `videre:value-flow/types@<v>` lowers into a component-model import named at that version, so the import list of `videre:types`, of `videre:venue`, and of every guest world moves with it.
An already-built `.wasm` therefore stops instantiating against a host linked at the new version; rebuilding the module is not optional.
`videre:types` and `videre:venue` stay at 0.1.0 across the 1.0.0 freeze even though their resolved import names changed, because both are pre-1.0 and move with this repository rather than on their own contract.
A downstream repository that vendors them by version alone sees no signal, so a value-flow bump goes out with the re-vendor runbook, not on its own.

## Enforcement

`crates/videre-test/tests/value_flow_freeze.rs` pins what a compile cannot see: the `asset` case set closed and in ABI order, and the declaration of every frozen record field.
The rest is compile-time.
The versioned `import` lines in `crates/videre-sdk/src/bindings.rs` and the identifier-hygiene smoke in `crates/videre-host/src/bindings.rs` resolve the package at 1.0.0, so a stale version reference fails the build.
The bindgen struct literals in the same smoke and in `crates/videre-sdk/src/value_flow.rs` fail the build on a dropped or renamed field, and the exhaustive `match` on `Asset` in `crates/videre-test/src/header.rs` fails the build on an added case.
