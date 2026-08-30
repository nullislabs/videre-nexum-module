# 0002: bare native case in the asset variant

## Status

Accepted.
The decision was taken in the shipped WIT at the value-flow freeze gate.
This record was written after the fact (tracker issue #20).

## Context

The `asset` variant in `wit/videre-value-flow/types.wit` names the kinds of value that can move.
An earlier draft named the case `native-token` and nested a token reference inside it.
That shape admitted `native-token(offchain(...))`: a gas token that is located off chain, which names no real asset.
Tracker issue #20 floated ratifying that state as representable but invalid.
Ratification would push the invariant into every consumer as a runtime check.

## Decision

The shipped `asset` variant carries a bare `native` case with no payload.
`native` means the settlement chain's gas token, and the header's settlement record supplies the chain.
The restructuring goes further than the ratify-as-invalid option: the invalid state is unrepresentable, because the case has no payload position.

## Consequences

No consumer validates a native payload, because none exists.
`AssetAmount::native` in `videre_sdk::value_flow` is the SDK spelling of the case, and it takes an amount and nothing else.
`erc721`, `erc1155`, `service`, and `offchain` are planned as separate 0.2+ cases, never as payloads of `native`.
The variant is closed at the `videre:value-flow@0.1.0` freeze; case growth needs a new major version, which is `0.2.0` under 0.x semver.
The freeze semantics themselves are recorded separately (tracker issue #47).
