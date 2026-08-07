# Design documents

This directory holds design documents and runbooks.
Put architecture decision records in `docs/adr/`, not here.

## What is authoritative

`AGENTS.md` describes the live architecture of this repository.
The `wit/` tree is authoritative for the contract surface.
Every document below is historical and describes the pre-carve monorepo.

## Contents

- `videre-split-plan.md`: the plan that sequenced the three-repo split, since executed.
- `issue-milestone-plan.md`: the issue and milestone reorganisation, since applied.
- `videre-wit-pinned-0.1.0.md`: the pinned 0.1.0 WIT surface, since grown additively.

Each document carries a provenance header.
Read that header first, because crate names, WIT package names, and file paths changed at the carve.
Bare issue and PR numbers in these documents refer to the pre-carve nullislabs/shepherd tracker.

## Landing places

The wit-deps re-vendor and freeze runbook (tracker issue #22) lands here.
