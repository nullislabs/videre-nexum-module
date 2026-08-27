//! # echo-venue (reference videre venue adapter)
//!
//! Minimal reference venue adapter: accepts any body, echoes it back as
//! the receipt, and settles instantly (every receipt reports
//! `fulfilled`). It is the smallest demonstration of
//! [`VenueInvoker`] and the `videre-test`
//! conformance target (see the tests below).
//!
//! This was a guest wasm component of kind `venue-adapter` until the
//! runtime deleted the extension-installed component path. A venue is now
//! a native Rust adapter the composition root registers, so this crate is
//! a plain library. Two things left with the wasm model: the `init`
//! config hook, which the composition root replaces by constructing the
//! adapter itself, and the `chain::request` read in `submit`, which was
//! there only to justify the manifest's `chain` capability. A native
//! adapter reaches a chain through its own client, and the echo venue
//! needs none.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use std::collections::BTreeSet;

use futures::FutureExt;
use futures::future::BoxFuture;
use videre_host::bindings::value_flow::{Asset, AssetAmount};
use videre_host::bindings::{
    AuthScheme, IntentHeader, IntentStatus, Quotation, Settlement, SubmitOutcome, VenueError,
};
use videre_host::{DuplicateVenue, Liveness, VenueId, VenueInvoker, VenueRegistry};

/// The body-schema versions this venue decodes. A keeper declaring
/// `[venue] body_version` boots only when every registered venue lists it.
pub const BODY_VERSIONS: [u32; 1] = [1];

/// The chain the echo venue settles on.
pub const SETTLEMENT_CHAIN: u64 = 1;

/// [`BODY_VERSIONS`] in the set shape [`VenueRegistry::register`] takes.
#[must_use]
pub fn body_versions() -> BTreeSet<u32> {
    BODY_VERSIONS.into_iter().collect()
}

/// The reference adapter. Stateless: every call answers from the body
/// alone.
#[derive(Clone, Copy, Debug, Default)]
pub struct EchoVenue;

impl EchoVenue {
    /// A fresh adapter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Project the body onto its header.
    ///
    /// The echo venue gives back exactly the bytes handed to it, so the
    /// header's `gives` amount is the body length: enough to exercise the
    /// value-flow vocabulary without a real schema. It wants nothing,
    /// spelled as a zero native amount.
    ///
    /// # Errors
    ///
    /// Never fails. The result shape matches the seam and the conformance
    /// kit.
    pub fn derive_header(body: &[u8]) -> Result<IntentHeader, VenueError> {
        Ok(IntentHeader {
            gives: AssetAmount {
                asset: Asset::Native,
                amount: minimal_be(body.len() as u64),
            },
            wants: zero_native(),
            settlement: Settlement {
                chain: SETTLEMENT_CHAIN,
            },
            authorisation: AuthScheme::Eip1271,
        })
    }

    /// Price the body.
    ///
    /// Echo pricing mirrors the header: gives the body length, wants
    /// nothing, charges no fee, and the quote never expires.
    ///
    /// # Errors
    ///
    /// Never fails.
    pub fn quote(body: &[u8]) -> Result<Quotation, VenueError> {
        Ok(Quotation {
            gives: AssetAmount {
                asset: Asset::Native,
                amount: minimal_be(body.len() as u64),
            },
            wants: zero_native(),
            fee: zero_native(),
            valid_until_ms: u64::MAX,
        })
    }

    /// Accept the body, echoing it back as the receipt.
    ///
    /// # Errors
    ///
    /// Never fails.
    pub fn submit(body: &[u8]) -> Result<SubmitOutcome, VenueError> {
        Ok(SubmitOutcome::Accepted(body.to_vec()))
    }

    /// Report the receipt as settled.
    ///
    /// The echo venue settles instantly, so the intent reaches a terminal
    /// state on the first status poll.
    ///
    /// # Errors
    ///
    /// Never fails.
    pub fn status(_receipt: &[u8]) -> Result<IntentStatus, VenueError> {
        Ok(IntentStatus::Fulfilled)
    }

    /// Accept the withdrawal.
    ///
    /// # Errors
    ///
    /// Never fails.
    pub fn cancel(_receipt: &[u8]) -> Result<(), VenueError> {
        Ok(())
    }
}

impl VenueInvoker for EchoVenue {
    fn derive_header<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<IntentHeader, VenueError>> {
        async move { Self::derive_header(body) }.boxed()
    }

    fn quote<'a>(&'a mut self, body: &'a [u8]) -> BoxFuture<'a, Result<Quotation, VenueError>> {
        async move { Self::quote(body) }.boxed()
    }

    fn submit<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<SubmitOutcome, VenueError>> {
        async move { Self::submit(body) }.boxed()
    }

    fn status(&mut self, receipt: Vec<u8>) -> BoxFuture<'_, Result<IntentStatus, VenueError>> {
        async move { Self::status(&receipt) }.boxed()
    }

    fn cancel(&mut self, receipt: Vec<u8>) -> BoxFuture<'_, Result<(), VenueError>> {
        async move { Self::cancel(&receipt) }.boxed()
    }
}

/// Register a fresh echo venue under `venue`, and return its liveness flag.
///
/// The echo venue never dies, so the flag stays alive. It is returned so a
/// composition root holds one handle per venue.
///
/// # Errors
///
/// Returns [`DuplicateVenue`] when a live venue already claims the id.
pub fn register(registry: &VenueRegistry, venue: VenueId) -> Result<Liveness, DuplicateVenue> {
    let liveness = Liveness::new();
    registry.register(venue, liveness.clone(), body_versions(), EchoVenue::new())?;
    Ok(liveness)
}

/// A zero native amount: the venue's spelling of "nothing".
fn zero_native() -> AssetAmount {
    AssetAmount {
        asset: Asset::Native,
        amount: Vec::new(),
    }
}

/// Big-endian bytes with leading zeros trimmed: the canonical `uint`
/// spelling of ADR 0001, where an empty list is zero.
///
/// A native venue does not link the guest SDK, so it does not reach
/// `videre_sdk::value_flow::encode_uint`. The test below holds this local
/// encoder to the same published vectors that the SDK codec answers to.
fn minimal_be(value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|byte| *byte != 0);
    first.map_or(Vec::new(), |index| bytes[index..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::{EchoVenue, VenueInvoker, body_versions, register};
    use std::collections::BTreeSet;
    use videre_host::bindings::{IntentStatus, SubmitOutcome};
    use videre_host::{SubmitQuota, VenueId, VenueRegistryBuilder};

    #[test]
    fn the_venue_declares_body_version_one() {
        // Pinned to the literal the keeper handshake reads, so a change to
        // the constant is a deliberate one.
        assert_eq!(body_versions(), BTreeSet::from([1]));
    }

    #[tokio::test]
    async fn submit_echoes_the_body_back_as_the_receipt() {
        let mut venue = EchoVenue::new();
        let outcome = venue.submit(b"body").await.expect("echo accepts any body");
        assert_eq!(outcome, SubmitOutcome::Accepted(b"body".to_vec()));
    }

    #[tokio::test]
    async fn a_receipt_reports_fulfilled_on_the_first_poll() {
        let mut venue = EchoVenue::new();
        let status = venue.status(b"receipt".to_vec()).await.expect("poll");
        assert_eq!(status, IntentStatus::Fulfilled);
        venue.cancel(b"receipt".to_vec()).await.expect("cancel");
    }

    #[tokio::test]
    async fn a_registered_echo_venue_is_routable() {
        let registry = VenueRegistryBuilder::new(SubmitQuota::default()).build();
        let venue = VenueId::new("echo-venue").expect("a valid venue id");
        let liveness = register(&registry, venue.clone()).expect("first registration");
        assert!(liveness.is_alive());

        let outcome = registry
            .submit("mod-a", &venue, b"body".to_vec())
            .await
            .expect("the registry routes to the echo venue");
        assert_eq!(outcome, SubmitOutcome::Accepted(b"body".to_vec()));
        assert_eq!(
            registry.body_versions().get(&venue),
            Some(&body_versions()),
            "the registry publishes the declared body versions",
        );
    }
}

/// echo-venue as the `videre-test` conformance target: the pure header
/// derivation is held to a hand-written golden.
#[cfg(test)]
mod conformance {
    use super::EchoVenue;
    use videre_host::bindings::IntentHeader;
    use videre_test::{
        FormatVersion, GoldenAsset, GoldenAssetAmount, GoldenAuthScheme, GoldenHeader,
        GoldenSettlement, HeaderGolden, HeaderGoldens, UINT_VECTORS_JSON, UintExpectation,
        UintVectors,
    };

    fn zero_native() -> GoldenAssetAmount {
        GoldenAssetAmount {
            asset: GoldenAsset::Native,
            amount: Vec::new(),
        }
    }

    /// Lower a host-side header onto the kit's mirror types. The kit's own
    /// `From` impl covers the SDK header of an out-of-process adapter, so a
    /// native adapter maps its own header here.
    fn as_golden(header: IntentHeader) -> GoldenHeader {
        use videre_host::bindings::AuthScheme;
        use videre_host::bindings::value_flow::Asset;

        let amount = |amount: videre_host::bindings::value_flow::AssetAmount| GoldenAssetAmount {
            asset: match amount.asset {
                Asset::Native => GoldenAsset::Native,
                Asset::Erc20(erc20) => GoldenAsset::Erc20 { token: erc20.token },
            },
            amount: amount.amount,
        };
        GoldenHeader {
            gives: amount(header.gives),
            wants: amount(header.wants),
            settlement: GoldenSettlement {
                chain: header.settlement.chain,
            },
            authorisation: match header.authorisation {
                AuthScheme::Eip1271 => GoldenAuthScheme::Eip1271,
                AuthScheme::Eip712 => GoldenAuthScheme::Eip712,
            },
        }
    }

    /// The adapter's derivation in the shape the kit's check takes.
    fn derive(body: Vec<u8>) -> Result<GoldenHeader, videre_host::bindings::VenueError> {
        EchoVenue::derive_header(&body).map(as_golden)
    }

    #[test]
    fn derive_header_conforms_to_the_published_golden() {
        // The echo contract: gives chain-1 native token whose amount is the
        // body length in minimal big-endian bytes, wants zero native, and
        // authorises via EIP-1271. A conforming adapter reproduces this
        // exactly.
        let golden = HeaderGolden {
            name: "four-byte-body".to_owned(),
            body: vec![1, 2, 3, 4],
            header: GoldenHeader {
                gives: GoldenAssetAmount {
                    asset: GoldenAsset::Native,
                    amount: vec![4],
                },
                wants: zero_native(),
                settlement: GoldenSettlement { chain: 1 },
                authorisation: GoldenAuthScheme::Eip1271,
            },
            notes: Some("amount is the minimal big-endian body length".to_owned()),
        };
        let goldens = HeaderGoldens {
            version: FormatVersion,
            venue: "echo-venue".to_owned(),
            goldens: vec![golden],
        };
        goldens.assert_conforms(derive);
    }

    #[test]
    fn divergent_derivation_is_caught_by_the_golden() {
        // A non-minimal amount is the classic uint bug; the golden must
        // reject it, proving the check has teeth on echo-venue.
        let goldens = HeaderGoldens {
            version: FormatVersion,
            venue: "echo-venue".to_owned(),
            goldens: vec![HeaderGolden {
                name: "four-byte-body".to_owned(),
                body: vec![1, 2, 3, 4],
                header: GoldenHeader {
                    gives: GoldenAssetAmount {
                        asset: GoldenAsset::Native,
                        amount: 4u64.to_be_bytes().to_vec(),
                    },
                    wants: zero_native(),
                    settlement: GoldenSettlement { chain: 1 },
                    authorisation: GoldenAuthScheme::Eip1271,
                },
                notes: None,
            }],
        };
        assert!(goldens.check(derive).is_err());
    }

    #[test]
    fn the_amount_encoder_matches_the_published_uint_vectors() {
        // The native venue keeps its own minimal encoder, so the published
        // vectors are what pins it to ADR 0001. Every value vector that
        // fits a u64 must come back byte for byte.
        let vectors = UintVectors::from_json(UINT_VECTORS_JSON).expect("the vectors parse");
        let mut checked = 0;
        for vector in &vectors.vectors {
            let UintExpectation::Value(published) = &vector.expect else {
                continue;
            };
            let Ok(value) = published.parse::<u64>() else {
                continue;
            };
            assert_eq!(
                super::minimal_be(value),
                vector.bytes,
                "vector {}",
                vector.name,
            );
            checked += 1;
        }
        assert!(checked >= 4, "only {checked} u64 value vectors ran");
    }

    #[test]
    fn the_amount_encoder_never_emits_a_leading_zero() {
        for value in [0u64, 1, 255, 256, 1 << 32, u64::MAX] {
            let bytes = super::minimal_be(value);
            assert_ne!(bytes.first(), Some(&0), "value {value} encoded {bytes:?}");
        }
    }
}
