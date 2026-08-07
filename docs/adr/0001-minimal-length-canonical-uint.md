# 0001: minimal-length canonical uint encoding

## Status

Accepted.
The decision was taken in the shipped WIT at the value-flow freeze gate.
This record was written after the fact (tracker issue #20).

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

## Consequences

For accepted values, byte equality and integer equality coincide, so golden files can pin amounts byte-exact.
`videre_sdk::value_flow` owns both directions: `encode_uint` emits the minimal form, and `decode_uint` rejects a leading zero byte and an encoding past 32 bytes.
`videre-test` publishes `crates/videre-test/vectors/uint.json`, and its reject vectors fail any decoder that tolerates padding, so the MUST has enforcement rather than prose only.
The encoding is part of the 1.0 freeze surface; a change to it is a new major version of `videre:value-flow`.
