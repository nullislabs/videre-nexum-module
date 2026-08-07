# Architecture decision records

This directory is the ADR home for this repository.
Record each accepted architecture decision here as one file.

## Convention

Name each file `NNNN-short-title.md`.
Use a four-digit sequence number and a kebab-case title.
Start this repository's series at `0001`.
Do not renumber and do not reuse a number.
The pre-carve ADR series (`0001` to `0014`) stays in the nullislabs/shepherd tree; do not continue that series here.

Give each ADR these sections: `Status`, `Context`, `Decision`, `Consequences`.
Set `Status` to `Proposed`, `Accepted`, or `Superseded`.
When an ADR supersedes another, name the superseded file in both records.

Write each ADR in ASD-STE100 Simplified Technical English.
Use short sentences and the active voice.
Put one idea in each sentence.
Put each sentence on its own line.

## Landing places

Record the value-flow freeze-gate decisions (tracker issue #20) here.
Record the WIT variant-growth caveat with them (tracker issue #47).
Put design documents and runbooks in `docs/design/`, not here.
The wit-deps re-vendor and freeze runbook (tracker issue #22) lands in `docs/design/`.
