# Design documents

This directory holds design documents and runbooks.
Put architecture decision records in `docs/adr/`, not here.
Write each file in the house documentation style that `AGENTS.md` sets.

## Contents

Every document below is historical and describes the pre-carve monorepo; `AGENTS.md`, the `wit/` tree, and `docs/adr/` are authoritative instead.

- `videre-split-plan.md`: the plan that sequenced the three-repo split, since executed.
- `issue-milestone-plan.md`: the issue and milestone reorganisation, since applied.
- `videre-wit-pinned-0.1.0.md`: the pinned 0.1.0 WIT surface, since grown additively.

Each document carries a provenance header that lists its load-bearing retired claims.
That list is not exhaustive: assume any name in a document body is pre-carve until the live tree confirms it.
Bare issue and PR numbers in these documents refer to the pre-carve nullislabs/shepherd tracker.
All three treat a venue as a guest wasm component of kind `venue-adapter` that the host installs and supervises; a venue is now a native Rust adapter that implements `VenueInvoker` from `videre-host`, the composition root registers it with `VenueRegistry::register`, and the runtime deleted the extension-installed component path.
A keeper stays a guest wasm component, and it still reaches a venue through `videre:venue/client`.

## Landing places

The wit-deps re-vendor and freeze runbook (tracker issue #22) lands here.
Tracker issue #45 is a hard precondition for it: the digests in `wit/deps.toml` are placeholders until #45 records the real ones.
