# Design documents

This directory holds design documents and runbooks.
Put architecture decision records in `docs/adr/`, not here.

## What is authoritative

`AGENTS.md` describes the live architecture of this repository.
The `wit/` tree is authoritative for the contract surface.
The `docs/adr/` tree is authoritative for an accepted decision.
Every document below is historical and describes the pre-carve monorepo.
Read a document below as a design record, not as a description of the code that runs today.

## Contents

- `videre-split-plan.md`: the plan that sequenced the three-repo split, since executed.
- `issue-milestone-plan.md`: the issue and milestone reorganisation, since applied.
- `videre-wit-pinned-0.1.0.md`: the pinned 0.1.0 WIT surface, since grown additively.

Each document carries a provenance header.
Read that header first, because crate names, WIT package names, and file paths changed at the carve.
The header also lists the load-bearing claims that a later change made false.
It is not exhaustive: assume any name in a document body is pre-carve until the live tree confirms it.
Bare issue and PR numbers in these documents refer to the pre-carve nullislabs/shepherd tracker.

## One claim that all three documents get wrong

Each document treats a venue as a guest wasm component of kind `venue-adapter`, installed and supervised by the host.
That model is retired.
A venue is now a native Rust adapter that implements `VenueInvoker` from `videre-host`.
The composition root registers it with `VenueRegistry::register`.
The runtime deleted the extension-installed component path, so an extension can no longer install or supervise a guest.
A keeper stays a guest wasm component, and it still reaches a venue through `videre:venue/client`.

## Landing places

The wit-deps re-vendor and freeze runbook (tracker issue #22) lands here.
Tracker issue #45 is a hard precondition for it: the digests in `wit/deps.toml` are placeholders until #45 records the real ones.
