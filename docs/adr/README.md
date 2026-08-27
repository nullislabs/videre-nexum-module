# Architecture decision records

Record each accepted architecture decision here as one file.
Put design documents and runbooks in `docs/design/`, not here.

## Convention

Name each file `NNNN-short-title.md`, with a four-digit sequence number and a kebab-case title.
This repository's series starts at `0001`; do not renumber and do not reuse a number.
The pre-carve series runs to `0014` and stays on the nullislabs/shepherd `develop` line; do not continue it here.
Give each ADR the sections `Status`, `Context`, `Decision`, `Consequences`, with `Status` set to `Proposed`, `Accepted`, or `Superseded`.
When an ADR supersedes another, name the superseded file in both records.
Write each ADR in the house documentation style that `AGENTS.md` sets.

## Landing places

The value-flow freeze-gate decisions (tracker issue #20) claim `0001` for the minimal-length canonical `uint` encoding and `0002` for the bare `native` asset case.
Record the next accepted decision as `0003`, where the WIT variant-growth caveat and the rest of that batch (tracker issue #47) land.
