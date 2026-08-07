//! # echo-venue (reference Shepherd venue adapter)
//!
//! Minimal reference venue adapter: accepts any body, echoes it back as
//! the receipt, and settles instantly (every receipt reports
//! `fulfilled`). The smallest demonstration of `#[videre_sdk::venue]`
//! and the `videre-test` conformance target (see the tests below).

// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

// `Config` and `Fault` come from the macro's world bindgen at the crate
// root: aliases of the SDK types, so the trait impl lines up.
use nexum::host::chain;
use videre_sdk::nexum_sdk::prelude::U256;
use videre_sdk::value_flow::{Asset, AssetAmount, encode_uint};
use videre_sdk::{
    AuthScheme, IntentHeader, IntentStatus, Quotation, Settlement, SubmitOutcome, VenueAdapter,
    VenueError,
};

struct EchoVenue;

#[videre_sdk::venue]
impl VenueAdapter for EchoVenue {
    fn init(_config: Config) -> Result<(), Fault> {
        Ok(())
    }

    fn body_versions() -> Vec<u32> {
        // Must equal the manifest `[venue] body_versions`; install
        // asserts it.
        vec![1]
    }

    fn derive_header(body: Vec<u8>) -> Result<IntentHeader, VenueError> {
        // The submitter pays one native unit per byte and wants the echo
        // of those bytes back, so both legs scale with the body length:
        // enough to exercise the value-flow vocabulary without a real
        // schema. The want is service-shaped, display-grade by
        // construction; the enforceable `gives` leg stays a chain asset.
        Ok(IntentHeader {
            gives: AssetAmount {
                asset: Asset::Native,
                amount: encode_uint(U256::from(body.len() as u64)),
            },
            wants: echo_service(&body),
            settlement: Settlement { chain: 1 },
            authorisation: AuthScheme::Eip1271,
        })
    }

    fn quote(body: Vec<u8>) -> Result<Quotation, VenueError> {
        // Echo pricing mirrors the header: gives the body length, wants
        // the echo service, charges no fee, and the quote never expires.
        Ok(Quotation {
            gives: AssetAmount {
                asset: Asset::Native,
                amount: encode_uint(U256::from(body.len() as u64)),
            },
            wants: echo_service(&body),
            fee: zero_native(),
            valid_until_ms: u64::MAX,
        })
    }

    fn submit(body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        // Reading chain state on submit is what justifies the declared
        // `chain` capability; the block height is discarded, the point is
        // the scoped transport import the manifest declares.
        let _ = chain::request(1, "eth_blockNumber", "[]")
            .map_err(|_| VenueError::Unavailable("chain read failed".into()))?;
        Ok(SubmitOutcome::Accepted(body))
    }

    fn status(_receipt: Vec<u8>) -> Result<IntentStatus, VenueError> {
        // Settles instantly: the intent reaches a terminal state on the
        // first status poll.
        Ok(IntentStatus::Fulfilled)
    }

    fn cancel(_receipt: Vec<u8>) -> Result<(), VenueError> {
        Ok(())
    }
}

/// A zero native amount: the venue's spelling of "nothing".
fn zero_native() -> AssetAmount {
    AssetAmount {
        asset: Asset::Native,
        amount: Vec::new(),
    }
}

/// The echo want: the service leg for echoing `body`, one venue-defined
/// unit per byte.
fn echo_service(body: &[u8]) -> AssetAmount {
    AssetAmount::service(ECHO_SERVICE, U256::from(body.len() as u64))
}

/// Venue-scoped description of the one service echo-venue provides.
const ECHO_SERVICE: &str = "echo of the submitted body";

/// echo-venue as the `videre-test` conformance target: the pure header
/// derivation is held to a hand-written golden.
#[cfg(test)]
mod conformance {
    use super::*;
    use videre_test::{
        FormatVersion, GoldenAsset, GoldenAssetAmount, GoldenAuthScheme, GoldenHeader,
        GoldenSettlement, HeaderGolden, HeaderGoldens,
    };

    fn echo_service_want(amount: Vec<u8>) -> GoldenAssetAmount {
        GoldenAssetAmount {
            asset: GoldenAsset::Service {
                description: ECHO_SERVICE.to_owned(),
            },
            amount,
        }
    }

    #[test]
    fn derive_header_conforms_to_the_published_golden() {
        // The echo contract: gives chain-1 native token whose amount is the
        // body length in minimal big-endian bytes, wants the echo service
        // at one unit per byte, and authorises via EIP-1271. A conforming
        // adapter reproduces this exactly.
        let golden = HeaderGolden {
            name: "four-byte-body".to_owned(),
            body: vec![1, 2, 3, 4],
            header: GoldenHeader {
                gives: GoldenAssetAmount {
                    asset: GoldenAsset::Native,
                    amount: vec![4],
                },
                wants: echo_service_want(vec![4]),
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
        goldens.assert_conforms(EchoVenue::derive_header);
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
                    wants: echo_service_want(vec![4]),
                    settlement: GoldenSettlement { chain: 1 },
                    authorisation: GoldenAuthScheme::Eip1271,
                },
                notes: None,
            }],
        };
        assert!(goldens.check(EchoVenue::derive_header).is_err());
    }

    #[test]
    fn divergent_service_description_is_caught_by_the_golden() {
        // A service leg is display-grade, but the golden still pins its
        // wire content: a renamed description must be a violation.
        let goldens = HeaderGoldens {
            version: FormatVersion,
            venue: "echo-venue".to_owned(),
            goldens: vec![HeaderGolden {
                name: "four-byte-body".to_owned(),
                body: vec![1, 2, 3, 4],
                header: GoldenHeader {
                    gives: GoldenAssetAmount {
                        asset: GoldenAsset::Native,
                        amount: vec![4],
                    },
                    wants: GoldenAssetAmount {
                        asset: GoldenAsset::Service {
                            description: "some other service".to_owned(),
                        },
                        amount: vec![4],
                    },
                    settlement: GoldenSettlement { chain: 1 },
                    authorisation: GoldenAuthScheme::Eip1271,
                },
                notes: None,
            }],
        };
        assert!(goldens.check(EchoVenue::derive_header).is_err());
    }
}
