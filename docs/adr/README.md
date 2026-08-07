# Architecture decision records

This directory is the ADR home for this repository.
Record each accepted architecture decision here as one file.

## Convention

Name each file `NNNN-short-title.md`.
Use a four-digit sequence number and a kebab-case title.
Start this repository's series at `0001`.
Do not renumber and do not reuse a number.
The pre-carve ADR series (`0001` to `0014`) stays on the nullislabs/shepherd `develop` line, which is the pre-carve history; do not continue that series here.
The shepherd `main` tree carries no `docs/`, so do not look for that series there.

Give each ADR these sections: `Status`, `Context`, `Decision`, `Consequences`.
Set `Status` to `Proposed`, `Accepted`, or `Superseded`.
When an ADR supersedes another, name the superseded file in both records.

Write each ADR in the house documentation style that `AGENTS.md` sets.

## Landing places

The value-flow freeze-gate decisions (tracker issue #20) landed as `0001` and `0002`.
Record the WIT variant-growth caveat and the rest of that batch (tracker issue #47) from `0003` on.
Put design documents and runbooks in `docs/design/`, not here.
