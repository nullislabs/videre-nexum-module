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
`erc721`, `erc1155`, `service`, and `offchain` are separate cases, never payloads of `native`.
`service` landed before the freeze because a service-shaped want has no other spelling (tracker issue #29); `erc721`, `erc1155`, and `offchain` stay 0.2+.
The variant is closed at the value-flow 1.0 freeze; case growth is a new major version.
The freeze semantics themselves are recorded separately (tracker issue #47).
