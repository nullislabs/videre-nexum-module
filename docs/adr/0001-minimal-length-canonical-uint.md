# 0001: minimal-length canonical uint encoding

## Status

Accepted.
The minimality rule was taken in the shipped WIT at the value-flow freeze gate, and this record was written after the fact (tracker issue #20).
The 32-byte bound is taken here: `types.wit` does not state it, and this record is its first statement.

## Context

`videre:value-flow@0.1.0` defines `uint` as `list<u8>` in `wit/videre-value-flow/types.wit`.
A bare byte list admits many encodings of one integer: `0x01`, `0x0001`, and `0x000001` all carry the value one.
Ambiguous encodings make byte comparison unsafe.
Two intent headers can then differ as bytes and agree as amounts, which opens a malleability seam in anything that hashes or compares the wire form.

## Decision

The `uint` encoding is canonical and minimal-length.
The bytes are the big-endian magnitude with no leading zero bytes.
Zero is the empty list.
A decoder rejects a non-minimal encoding; it does not normalise the padding away.
Decoders compare amounts by integer value, not by byte equality, as the `types.wit` prose states.
An encoding longer than 32 bytes is also not a valid `uint`, because `videre:value-flow@0.1.0` is EVM-only and the EVM word bounds the value.
That bound is new with this record, so a `types.wit` update must carry it before an independent implementer can meet it.
A decoder checks the width before the padding, so an over-long encoding reports the overflow whatever its first byte is.

## Consequences

For accepted values, byte equality and integer equality coincide, so golden files can pin amounts byte-exact.
`videre_sdk::value_flow` owns both directions: `encode_uint` emits the minimal form, and `decode_uint` rejects a leading zero byte and an encoding past 32 bytes.
`AssetAmount::native` and `AssetAmount::erc20` build an amount through `encode_uint`, and `AssetAmount::value` reads one back through `decode_uint`, so a guest needs no hand-rolled encoding.
A zero amount is still written as the empty list in many test fixtures, because that literal is the canonical form.
A native venue such as `echo-venue` does not link the guest SDK, so it keeps a local minimal encoder.
The published vectors pin that encoder instead: `crates/echo-venue/src/lib.rs` holds it to the same file the SDK codec answers to.
`videre-host` does not decode the amount a venue returns, so the rule binds the SDK, the native venues, and the vector file, not a host trust boundary.
An `EgressGuard` that thresholds an amount must therefore decode it with `AssetAmount::value`, because the host hands it the raw bytes.
A later chain with a wider amount width reopens the 32-byte bound, not the minimality rule.
`videre-test` publishes `crates/videre-test/vectors/uint.json`, and its reject vectors fail any decoder that tolerates padding, so the MUST has enforcement rather than prose only.
The encoding is part of the frozen `videre:value-flow@0.1.0` surface; a change to it needs a new major version, which is `0.2.0` under 0.x semver.
