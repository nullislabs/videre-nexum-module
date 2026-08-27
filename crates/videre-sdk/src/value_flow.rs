//! The value-flow wire types, re-exported from
//! [`bindings`](crate::bindings) with the canonical `uint` codec so
//! callers never hand-roll the encoding.

// `Address` stays fully qualified below: the glob re-export publishes a
// value-flow `Address` alias, and a private import of the nexum type
// would shadow it.
use nexum_sdk::prelude::U256;

pub use crate::bindings::videre::value_flow::types::*;

/// Why bytes are not a canonical value-flow `uint` (ADR 0001).
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum UintError {
    /// A leading zero byte; the canonical form is minimal-length and
    /// zero is the empty list.
    #[error("non-minimal uint: leading zero byte")]
    LeadingZero,
    /// The value cannot fit the 32-byte EVM word.
    #[error("uint of {len} bytes overflows the 32-byte EVM word")]
    Overflow {
        /// Length of the rejected encoding.
        len: usize,
    },
}

/// Encode a value as the canonical `uint`: minimal big-endian bytes,
/// zero as the empty list.
#[must_use]
pub fn encode_uint(value: U256) -> Vec<u8> {
    value.to_be_bytes_trimmed_vec()
}

/// Decode a canonical `uint`, rejecting a non-minimal encoding rather
/// than normalising it.
///
/// The width is checked before the padding, so an over-long encoding
/// reports [`UintError::Overflow`] whatever its first byte is.
///
/// # Errors
///
/// Returns [`UintError::Overflow`] for an encoding past the 32-byte EVM
/// word, and [`UintError::LeadingZero`] for a padded encoding.
pub fn decode_uint(bytes: &[u8]) -> Result<U256, UintError> {
    if bytes.len() > 32 {
        return Err(UintError::Overflow { len: bytes.len() });
    }
    if bytes.first() == Some(&0) {
        return Err(UintError::LeadingZero);
    }
    Ok(U256::from_be_slice(bytes))
}

impl AssetAmount {
    /// An ERC-20 amount, with `amount` in the canonical `uint` encoding.
    #[must_use]
    pub fn erc20(token: nexum_sdk::prelude::Address, amount: U256) -> Self {
        Self {
            asset: Asset::Erc20(Erc20 {
                token: token.as_slice().to_vec(),
            }),
            amount: encode_uint(amount),
        }
    }

    /// A settlement-chain gas-token amount, in the canonical `uint`
    /// encoding.
    #[must_use]
    pub fn native(amount: U256) -> Self {
        Self {
            asset: Asset::Native,
            amount: encode_uint(amount),
        }
    }

    /// The amount as an integer, holding the wire bytes to the canonical
    /// encoding.
    ///
    /// This is the reading side of the rule: a consumer that compares or
    /// thresholds an amount decodes it here rather than comparing bytes.
    ///
    /// # Errors
    ///
    /// Returns the [`UintError`] when the bytes are not a canonical
    /// `uint`.
    pub fn value(&self) -> Result<U256, UintError> {
        decode_uint(&self.amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_is_minimal_and_zero_is_empty() {
        assert_eq!(encode_uint(U256::ZERO), Vec::<u8>::new());
        assert_eq!(encode_uint(U256::from(1u64)), vec![0x01]);
        assert_eq!(encode_uint(U256::from(256u64)), vec![0x01, 0x00]);
        assert_eq!(encode_uint(U256::MAX), vec![0xff; 32]);
    }

    #[test]
    fn decode_round_trips_the_canonical_forms() {
        for value in [U256::ZERO, U256::from(1u64), U256::from(256u64), U256::MAX] {
            assert_eq!(decode_uint(&encode_uint(value)), Ok(value));
        }
    }

    #[test]
    fn decode_rejects_a_leading_zero_byte() {
        assert_eq!(decode_uint(&[0x00]), Err(UintError::LeadingZero));
        assert_eq!(decode_uint(&[0x00, 0x01]), Err(UintError::LeadingZero));
    }

    #[test]
    fn decode_rejects_more_than_a_word() {
        let mut bytes = vec![0x01];
        bytes.extend([0x00; 32]);
        assert_eq!(decode_uint(&bytes), Err(UintError::Overflow { len: 33 }));
        // The width is checked first, so padding does not mask the
        // overflow.
        assert_eq!(
            decode_uint(&[0x00; 40]),
            Err(UintError::Overflow { len: 40 }),
        );
    }

    #[test]
    fn the_constructors_encode_canonically() {
        let native = AssetAmount::native(U256::from(256u64));
        assert_eq!(native.asset, Asset::Native);
        assert_eq!(native.amount, vec![0x01, 0x00]);
        assert_eq!(native.value(), Ok(U256::from(256u64)));

        let zero = AssetAmount::native(U256::ZERO);
        assert_eq!(zero.amount, Vec::<u8>::new());
        assert_eq!(zero.value(), Ok(U256::ZERO));

        let erc20 = AssetAmount::erc20(nexum_sdk::prelude::Address::ZERO, U256::from(1u64));
        assert_eq!(erc20.amount, vec![0x01]);
        assert_eq!(erc20.value(), Ok(U256::from(1u64)));
    }

    #[test]
    fn a_padded_amount_fails_to_read_back() {
        let padded = AssetAmount {
            asset: Asset::Native,
            amount: vec![0x00, 0x01],
        };
        assert_eq!(padded.value(), Err(UintError::LeadingZero));
    }
}
