//! The host's signing role as an in-memory mock: a `requires-signing`
//! submit outcome hands its `unsigned-tx` to [`MockSigner`], which holds
//! it to the wire contract, records it, and answers a deterministic tx
//! hash so a test can drive the pre-sign leg end to end.
//!
//! The mock has teeth: a malformed `to`, a non-canonical `value`, a bare
//! transfer (empty calldata), or a chain outside the scoped grant
//! ([`scope_chains`](MockSigner::scope_chains)) is refused as a typed
//! [`SignError`], as the signing host would refuse it.

use std::cell::RefCell;

use nexum_sdk::prelude::{B256, keccak256};
use videre_sdk::UnsignedTx;
use videre_sdk::value_flow::{UintError, decode_uint};

/// Why the mock refused to sign an unsigned tx.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SignError {
    /// `to` is not a 20-byte address.
    #[error("unsigned-tx `to` must be a 20-byte address, got {len} bytes")]
    MalformedTo {
        /// Length of the rejected `to` field.
        len: usize,
    },
    /// `value` is not a canonical value-flow `uint`.
    #[error("unsigned-tx `value` is not a canonical uint: {0}")]
    NonCanonicalValue(#[from] UintError),
    /// Empty calldata: a bare value transfer, which the wire contract
    /// forbids (an unsigned tx is always a call to existing code).
    #[error("unsigned-tx carries no calldata; a bare transfer is not a call to existing code")]
    BareTransfer,
    /// The tx targets a chain outside the signer's grant.
    #[error("chain {chain} is outside the signer's chain grant")]
    Denied {
        /// The refused chain id.
        chain: u64,
    },
}

/// In-memory stand-in for the host that signs and sends a
/// `requires-signing` transaction; records every signed tx and derives
/// the tx hash deterministically from the tx content.
#[derive(Default)]
pub struct MockSigner {
    signed: RefCell<Vec<UnsignedTx>>,
    scope: RefCell<Option<Vec<u64>>>,
}

impl MockSigner {
    /// Fresh signer with no chain grant configured (every chain admitted).
    pub fn new() -> Self {
        Self::default()
    }

    /// Confine signing to `chains`, mirroring the host's chain grant;
    /// off-grant txs fail [`SignError::Denied`]. An empty grant denies
    /// every chain.
    pub fn scope_chains(&self, chains: impl IntoIterator<Item = u64>) {
        *self.scope.borrow_mut() = Some(chains.into_iter().collect());
    }

    /// Validate, record, and "sign" `tx`, answering its deterministic tx
    /// hash. Equal txs always answer equal hashes, so a test can prove a
    /// re-derived pre-sign leg is byte-identical.
    pub fn sign_and_send(&self, tx: UnsignedTx) -> Result<B256, SignError> {
        if tx.to.len() != 20 {
            return Err(SignError::MalformedTo { len: tx.to.len() });
        }
        decode_uint(&tx.value)?;
        if tx.data.is_empty() {
            return Err(SignError::BareTransfer);
        }
        if let Some(scope) = self.scope.borrow().as_ref()
            && !scope.contains(&tx.chain)
        {
            return Err(SignError::Denied { chain: tx.chain });
        }
        let hash = tx_hash(&tx);
        self.signed.borrow_mut().push(tx);
        Ok(hash)
    }

    /// All signed txs, in signing order; refused txs are never recorded.
    pub fn signed(&self) -> Vec<UnsignedTx> {
        self.signed.borrow().clone()
    }

    /// Last signed tx, if any.
    pub fn last_signed(&self) -> Option<UnsignedTx> {
        self.signed.borrow().last().cloned()
    }

    /// Total signed-tx count.
    pub fn signed_count(&self) -> usize {
        self.signed.borrow().len()
    }
}

/// Deterministic mock tx hash: keccak over a length-framed encoding of
/// every field, so distinct txs cannot collide by concatenation.
fn tx_hash(tx: &UnsignedTx) -> B256 {
    let mut preimage = Vec::with_capacity(8 + 4 * 2 + tx.to.len() + tx.value.len() + tx.data.len());
    preimage.extend_from_slice(&tx.chain.to_be_bytes());
    preimage.extend_from_slice(&tx.to);
    for field in [&tx.value, &tx.data] {
        preimage.extend_from_slice(&(field.len() as u64).to_be_bytes());
        preimage.extend_from_slice(field);
    }
    keccak256(&preimage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn purchase_tx() -> UnsignedTx {
        UnsignedTx {
            chain: 100,
            to: vec![0xAA; 20],
            value: Vec::new(),
            data: vec![0xde, 0xad, 0xbe, 0xef],
        }
    }

    #[test]
    fn signing_records_the_tx_and_the_hash_is_deterministic() {
        let signer = MockSigner::new();
        let first = signer.sign_and_send(purchase_tx()).unwrap();
        let again = signer.sign_and_send(purchase_tx()).unwrap();
        assert_eq!(first, again, "equal txs must answer equal hashes");
        assert_eq!(signer.signed(), vec![purchase_tx(), purchase_tx()]);
        assert_eq!(signer.signed_count(), 2);
        assert_eq!(signer.last_signed(), Some(purchase_tx()));
    }

    #[test]
    fn distinct_txs_answer_distinct_hashes() {
        let signer = MockSigner::new();
        let base = signer.sign_and_send(purchase_tx()).unwrap();
        for tx in [
            UnsignedTx {
                chain: 1,
                ..purchase_tx()
            },
            UnsignedTx {
                to: vec![0xBB; 20],
                ..purchase_tx()
            },
            UnsignedTx {
                value: vec![0x01],
                ..purchase_tx()
            },
            UnsignedTx {
                data: vec![0xde, 0xad],
                ..purchase_tx()
            },
        ] {
            assert_ne!(signer.sign_and_send(tx).unwrap(), base);
        }
    }

    #[test]
    fn framing_prevents_value_data_boundary_collisions() {
        let signer = MockSigner::new();
        let a = signer
            .sign_and_send(UnsignedTx {
                value: vec![0x01],
                data: vec![0x02, 0x03],
                ..purchase_tx()
            })
            .unwrap();
        let b = signer
            .sign_and_send(UnsignedTx {
                value: vec![0x01, 0x02],
                data: vec![0x03],
                ..purchase_tx()
            })
            .unwrap();
        assert_ne!(
            a, b,
            "shifting bytes across the value/data boundary must change the hash"
        );
    }

    #[test]
    fn malformed_to_is_refused_and_never_recorded() {
        let signer = MockSigner::new();
        for len in [0, 19, 21, 32] {
            let err = signer
                .sign_and_send(UnsignedTx {
                    to: vec![0xAA; len],
                    ..purchase_tx()
                })
                .unwrap_err();
            assert_eq!(err, SignError::MalformedTo { len });
        }
        assert_eq!(signer.signed_count(), 0);
    }

    #[test]
    fn non_canonical_value_is_refused() {
        let signer = MockSigner::new();
        let err = signer
            .sign_and_send(UnsignedTx {
                value: vec![0x00, 0x01],
                ..purchase_tx()
            })
            .unwrap_err();
        assert_eq!(err, SignError::NonCanonicalValue(UintError::LeadingZero));
        assert_eq!(signer.signed_count(), 0);
    }

    #[test]
    fn a_bare_transfer_is_refused() {
        let signer = MockSigner::new();
        let err = signer
            .sign_and_send(UnsignedTx {
                data: Vec::new(),
                ..purchase_tx()
            })
            .unwrap_err();
        assert_eq!(err, SignError::BareTransfer);
        assert_eq!(signer.signed_count(), 0);
    }

    #[test]
    fn scope_matches_the_chain_grant() {
        let signer = MockSigner::new();
        signer.scope_chains([100]);
        signer.sign_and_send(purchase_tx()).unwrap();
        let err = signer
            .sign_and_send(UnsignedTx {
                chain: 1,
                ..purchase_tx()
            })
            .unwrap_err();
        assert_eq!(err, SignError::Denied { chain: 1 });
        assert_eq!(signer.signed_count(), 1);

        // An empty grant denies every chain, the host's posture for an
        // absent grant.
        let sealed = MockSigner::new();
        sealed.scope_chains(Vec::new());
        assert_eq!(
            sealed.sign_and_send(purchase_tx()).unwrap_err(),
            SignError::Denied { chain: 100 },
        );
    }
}
