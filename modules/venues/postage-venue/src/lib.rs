//! # postage-venue (Swarm postage batch purchase)
//!
//! The second genuine venue: buying a Swarm postage batch on Gnosis. The
//! header is policy-legible with the enforceable leg on chain assets:
//! `gives` is the BZZ ERC-20 total (`initial_balance_per_chunk` across
//! `1 << depth` chunk slots), `wants` the display-grade chunk capacity as
//! a service leg. `submit` answers `requires-signing(unsigned-tx)` with
//! the `createBatch` call on the postage stamp contract; only the host
//! can sign and send it.
//!
//! Settlement is two transactions: an ERC-20 `approve` of the BZZ total
//! to the postage contract must precede `createBatch`, and the
//! `requires-signing` wire carries exactly one transaction. This adapter
//! returns the purchase leg only and deliberately models nothing past
//! what the conformance kit proves; approve sequencing, signing,
//! broadcast, and receipt observation belong to the embedding host, and
//! the multi-transaction wire shape stays an open design question.

// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

use alloy_sol_types::{SolCall, sol};
use borsh::{BorshDeserialize, BorshSerialize};
use videre_sdk::nexum_sdk::prelude::{Address, B256, U256, address};
use videre_sdk::value_flow::AssetAmount;
use videre_sdk::{
    AuthScheme, IntentBody, IntentHeader, IntentStatus, Quotation, Settlement, SubmitOutcome,
    UnsignedTx, VenueAdapter, VenueError,
};

/// BZZ ERC-20 on Gnosis, bee's mainnet `bzzToken`.
const BZZ_TOKEN: Address = address!("0xdBF3Ea6F5beE45c02255B2c26a16F300502F68da");

/// The postage stamp contract on Gnosis, bee's mainnet `postageStamp`.
const POSTAGE_STAMP: Address = address!("0x45a1502382541Cd610CC9068e88727426b696293");

/// Gnosis chain id: where Swarm postage settles.
const GNOSIS: u64 = 100;

/// Venue-scoped description of the purchased service.
const POSTAGE_SERVICE: &str = "swarm postage batch: chunk capacity";

sol! {
    /// `PostageStamp.createBatch`: the purchase the host must sign. The
    /// contract pulls `initialBalancePerChunk << depth` BZZ by allowance,
    /// so no native value rides the tx.
    function createBatch(
        address owner,
        uint256 initialBalancePerChunk,
        uint8 depth,
        uint8 bucketDepth,
        bytes32 nonce,
        bool immutableFlag
    ) returns (bytes32 batchId);
}

/// First published version: one batch purchase.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, Eq, PartialEq)]
pub struct PostageV1 {
    /// 20-byte batch owner: the stamp-signing identity; never zero.
    pub owner: [u8; 20],
    /// BZZ per chunk slot in PLUR; the total transfer is this across
    /// `1 << depth` slots.
    pub initial_balance_per_chunk: u64,
    /// Batch depth: capacity is `1 << depth` chunks.
    pub depth: u8,
    /// Uniformity depth of the bucket partition; non-zero, below `depth`.
    pub bucket_depth: u8,
    /// 32-byte batch nonce; the on-chain batch id derives from the
    /// sender and this nonce.
    pub nonce: [u8; 32],
    /// Whether the batch refuses later dilution.
    pub immutable: bool,
}

impl PostageV1 {
    /// Refuse what `createBatch` would revert, answering the BZZ total:
    /// a zero owner, a zero per-chunk balance, a bucket depth of zero or
    /// not below `depth`, or a total past the EVM word. The contract's
    /// depth and balance floors are chain state this adapter cannot
    /// read, so only the unconditional refusals are mirrored.
    fn validate(&self) -> Result<U256, VenueError> {
        if self.owner == [0u8; 20] {
            return Err(VenueError::InvalidBody(
                "batch owner must not be the zero address".to_owned(),
            ));
        }
        // Zero is always below `minimumInitialBalancePerChunk`, and a
        // zero `gives` leg would report the purchase to policy as free.
        if self.initial_balance_per_chunk == 0 {
            return Err(VenueError::InvalidBody(
                "initial balance per chunk must be non-zero".to_owned(),
            ));
        }
        if self.bucket_depth == 0 || self.bucket_depth >= self.depth {
            return Err(VenueError::InvalidBody(format!(
                "bucket depth {} must be non-zero and below depth {}",
                self.bucket_depth, self.depth,
            )));
        }
        U256::from(self.initial_balance_per_chunk)
            .checked_shl(usize::from(self.depth))
            .ok_or_else(|| VenueError::InvalidBody("bzz total overflows the evm word".to_owned()))
    }

    /// Chunk capacity of the batch.
    fn capacity(&self) -> U256 {
        U256::ONE << usize::from(self.depth)
    }
}

/// The outer version enum; tag order is the schema, so append, never
/// reorder.
#[derive(IntentBody, Clone, Debug, Eq, PartialEq)]
pub enum PostageBody {
    /// Version 1, wire tag 0.
    V1(PostageV1),
}

/// The two value-flow legs every verb shares: the enforceable BZZ total
/// out, the display-grade chunk capacity back.
fn legs(batch: &PostageV1) -> Result<(AssetAmount, AssetAmount), VenueError> {
    let total = batch.validate()?;
    Ok((
        AssetAmount::erc20(BZZ_TOKEN, total),
        AssetAmount::service(POSTAGE_SERVICE, batch.capacity()),
    ))
}

struct PostageVenue;

#[videre_sdk::venue]
impl VenueAdapter for PostageVenue {
    fn init(_config: Config) -> Result<(), Fault> {
        install_tracing();
        tracing::info!("postage-venue facade installed");
        Ok(())
    }

    fn body_versions() -> Vec<u32> {
        // Must equal the manifest `[venue] body_versions`; install
        // asserts it.
        vec![1]
    }

    fn derive_header(body: Vec<u8>) -> Result<IntentHeader, VenueError> {
        let PostageBody::V1(batch) = PostageBody::from_bytes(&body)?;
        let (gives, wants) = legs(&batch)?;
        Ok(IntentHeader {
            gives,
            wants,
            settlement: Settlement { chain: GNOSIS },
            // The 0.1 scheme set has no pre-sign case; eip712 names the
            // host-held-keys path that signs the purchase.
            authorisation: AuthScheme::Eip712,
        })
    }

    fn quote(body: Vec<u8>) -> Result<Quotation, VenueError> {
        let PostageBody::V1(batch) = PostageBody::from_bytes(&body)?;
        let (gives, wants) = legs(&batch)?;
        Ok(Quotation {
            gives,
            wants,
            // The chain charges no venue fee; gas rides the host-signed tx.
            fee: AssetAmount::erc20(BZZ_TOKEN, U256::ZERO),
            // The buyer names the per-chunk balance, so the price cannot
            // go stale.
            valid_until_ms: u64::MAX,
        })
    }

    fn submit(body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        let PostageBody::V1(batch) = PostageBody::from_bytes(&body)?;
        // Refuse before shaping the tx, so the host is never handed a
        // purchase the contract would revert.
        batch.validate()?;
        tracing::info!(
            depth = batch.depth,
            immutable = batch.immutable,
            "postage-venue submit: batch purchase requires signing"
        );
        let call = createBatchCall {
            owner: Address::from(batch.owner),
            initialBalancePerChunk: U256::from(batch.initial_balance_per_chunk),
            depth: batch.depth,
            bucketDepth: batch.bucket_depth,
            nonce: B256::from(batch.nonce),
            immutableFlag: batch.immutable,
        };
        Ok(SubmitOutcome::RequiresSigning(UnsignedTx {
            chain: GNOSIS,
            to: POSTAGE_STAMP.as_slice().to_vec(),
            // BZZ moves by allowance, never native value.
            value: Vec::new(),
            data: call.abi_encode(),
        }))
    }

    fn status(_receipt: Vec<u8>) -> Result<IntentStatus, VenueError> {
        // The batch id derives from the signing key only the host holds,
        // and this adapter declares no chain capability, so the verb is
        // permanently unimplementable here rather than transiently
        // unavailable: `unavailable` would make a keeper poll forever
        // (`retry_action` folds it to `try-next-block`).
        Err(VenueError::Unsupported)
    }

    fn cancel(_receipt: Vec<u8>) -> Result<(), VenueError> {
        // A created batch cannot be revoked on chain.
        Err(VenueError::Unsupported)
    }
}

/// The advisory policy loop proven through the host registry: derive
/// header, guard checkpoint, submit, plus the requires-signing leg.
#[cfg(test)]
mod policy_round_trip;

/// postage-venue as the `videre-test` conformance target: the published
/// header goldens and codec vectors pin the pure derivations, and the
/// signer mock proves the pre-sign leg end to end.
#[cfg(test)]
mod conformance {
    use std::path::Path;

    use videre_sdk::nexum_sdk::prelude::hex;
    use videre_test::codec::{CodecVectors, Expectation};
    use videre_test::header::{GoldenAsset, HeaderGoldens};
    use videre_test::{MockTransport, SignError};

    use super::*;

    /// The published header golden file, verbatim.
    const HEADER_GOLDENS_JSON: &str = include_str!("../goldens/postage-header.json");

    /// The published codec vector file, verbatim.
    const CODEC_VECTORS_JSON: &str = include_str!("../vectors/postage-body.json");

    /// The postage stamp contract, restated from bee's mainnet chain
    /// config rather than read back off [`POSTAGE_STAMP`].
    const POSTAGE_STAMP_HEX: &str = "45a1502382541cd610cc9068e88727426b696293";

    /// `createBatch` calldata for [`first_batch`], pinned verbatim
    /// against `cast calldata`. Nothing derived from the `sol!` block
    /// can pin it: transposing the two `uint8` parameters leaves both
    /// the signature string and an `abi_decode` round trip unchanged,
    /// so only a literal catches an argument-order or width drift.
    const FIRST_BATCH_CALLDATA: &str = concat!(
        "5239af71",
        "0000000000000000000000000102030405060708090a0b0c0d0e0f1011121314",
        "00000000000000000000000000000000000000000000000000000000000003e8",
        "0000000000000000000000000000000000000000000000000000000000000011",
        "0000000000000000000000000000000000000000000000000000000000000010",
        "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );

    /// `createBatch` calldata for [`deep_immutable_batch`]: the only pin
    /// on `immutableFlag`, which has no footprint in the header.
    const DEEP_IMMUTABLE_CALLDATA: &str = concat!(
        "5239af71",
        "000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "0000000000000000000000000000000000000000000000000000000000000001",
        "0000000000000000000000000000000000000000000000000000000000000018",
        "0000000000000000000000000000000000000000000000000000000000000010",
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        "0000000000000000000000000000000000000000000000000000000000000001",
    );

    fn first_batch() -> PostageV1 {
        PostageV1 {
            owner: std::array::from_fn(|i| i as u8 + 1),
            initial_balance_per_chunk: 1_000,
            depth: 17,
            bucket_depth: 16,
            nonce: std::array::from_fn(|i| i as u8 + 1),
            immutable: false,
        }
    }

    fn deep_immutable_batch() -> PostageV1 {
        PostageV1 {
            owner: [0xAA; 20],
            initial_balance_per_chunk: 1,
            depth: 24,
            bucket_depth: 16,
            nonce: [0xFF; 32],
            immutable: true,
        }
    }

    fn body(batch: PostageV1) -> Vec<u8> {
        PostageBody::V1(batch).to_bytes().expect("body encodes")
    }

    /// Rebuild the published header goldens from the adapter.
    fn build_header_goldens() -> HeaderGoldens {
        let mut goldens = HeaderGoldens::new("postage-venue");
        goldens
            .record(
                "first-batch",
                body(first_batch()),
                PostageVenue::derive_header,
            )
            .unwrap()
            .notes = Some(
            "gives the bzz erc20 total (per-chunk balance shifted left by \
             depth), wants the display-grade chunk capacity"
                .to_owned(),
        );
        goldens
            .record(
                "deep-immutable-batch",
                body(deep_immutable_batch()),
                PostageVenue::derive_header,
            )
            .unwrap()
            .notes = Some(
            "immutability rides only the calldata; the value-flow legs are \
             unchanged"
                .to_owned(),
        );
        goldens
    }

    /// Rebuild the published codec vectors from the body schema.
    fn build_codec_vectors() -> CodecVectors {
        let mut vectors = CodecVectors::new("postage-venue/postage-body");
        vectors
            .push_round_trip("v1-first-batch", &PostageBody::V1(first_batch()))
            .unwrap()
            .notes = Some(
            "tag 0x00; owner 20 raw bytes (fixed array, no length prefix), \
             u64 little-endian, depth and bucket depth one byte each, nonce \
             32 raw bytes, bool one byte"
                .to_owned(),
        );
        vectors
            .push_round_trip(
                "v1-deep-immutable",
                &PostageBody::V1(deep_immutable_batch()),
            )
            .unwrap()
            .notes = Some("bool true is 0x01".to_owned());
        vectors
            .push_failure("empty-body", Vec::new(), Expectation::Empty)
            .notes = Some("no version tag at all".to_owned());
        let mut unknown = body(first_batch());
        unknown[0] = 9;
        vectors
            .push_failure(
                "unknown-version",
                unknown,
                Expectation::UnknownVersion { version: 9 },
            )
            .notes = Some("tag 0x09 names no published version".to_owned());
        let mut truncated = body(first_batch());
        truncated.truncate(truncated.len() - 1);
        vectors
            .push_failure(
                "truncated-payload",
                truncated,
                Expectation::Malformed { version: 0 },
            )
            .notes = Some("known tag, payload cut one byte short".to_owned());
        let mut trailing = body(first_batch());
        trailing.push(0);
        vectors
            .push_failure(
                "trailing-bytes",
                trailing,
                Expectation::Malformed { version: 0 },
            )
            .notes = Some("decoding must consume the payload exactly".to_owned());
        vectors
    }

    #[test]
    fn derive_header_conforms_to_the_published_goldens() {
        HeaderGoldens::from_json(HEADER_GOLDENS_JSON)
            .unwrap()
            .assert_conforms(PostageVenue::derive_header);
    }

    #[test]
    fn body_codec_conforms_to_the_published_vectors() {
        CodecVectors::from_json(CODEC_VECTORS_JSON)
            .unwrap()
            .assert_conforms::<PostageBody>();
    }

    #[test]
    fn published_header_goldens_match_regeneration() {
        assert_eq!(
            HEADER_GOLDENS_JSON,
            build_header_goldens().to_json(),
            "goldens/postage-header.json has drifted; run the ignored \
             regenerate_postage_fixtures test and commit the result",
        );
    }

    #[test]
    fn published_codec_vectors_match_regeneration() {
        assert_eq!(
            CODEC_VECTORS_JSON,
            build_codec_vectors().to_json(),
            "vectors/postage-body.json has drifted; run the ignored \
             regenerate_postage_fixtures test and commit the result",
        );
    }

    /// Rewrite the published files from the adapter. Run with
    /// `cargo test -p postage-venue -- --ignored regenerate` after a
    /// deliberate schema change, then commit the diff.
    #[test]
    #[ignore = "writes the published fixture files in place"]
    fn regenerate_postage_fixtures() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        build_header_goldens()
            .write(root.join("goldens/postage-header.json"))
            .unwrap();
        build_codec_vectors()
            .write(root.join("vectors/postage-body.json"))
            .unwrap();
    }

    #[test]
    fn divergent_derivation_is_caught_by_the_golden() {
        // A non-minimal bzz amount is the classic uint bug; the golden
        // must reject it, proving the check has teeth on postage-venue.
        let mut goldens = HeaderGoldens::from_json(HEADER_GOLDENS_JSON).unwrap();
        let total =
            U256::from(first_batch().initial_balance_per_chunk) << usize::from(first_batch().depth);
        goldens.goldens[0].header.gives.amount = total.to_be_bytes::<32>().to_vec();
        assert!(goldens.check(PostageVenue::derive_header).is_err());

        // A wrong token address must also be a violation: the golden pins
        // the bzz contract.
        let mut goldens = HeaderGoldens::from_json(HEADER_GOLDENS_JSON).unwrap();
        goldens.goldens[0].header.gives.asset = GoldenAsset::Erc20 {
            token: vec![0xEE; 20],
        };
        assert!(goldens.check(PostageVenue::derive_header).is_err());
    }

    #[test]
    fn submit_requires_signing_and_the_signer_mock_settles_the_pre_sign_leg() {
        let transport = MockTransport::new();
        transport.signer.scope_chains([GNOSIS]);

        let SubmitOutcome::RequiresSigning(tx) = PostageVenue::submit(body(first_batch())).unwrap()
        else {
            panic!("postage submit must answer requires-signing");
        };

        // The purchase targets the postage contract on gnosis, moves no
        // native value, and carries the createBatch calldata verbatim.
        assert_eq!(tx.chain, GNOSIS);
        assert_eq!(tx.to, hex::decode(POSTAGE_STAMP_HEX).unwrap());
        assert!(tx.value.is_empty(), "bzz moves by allowance, never value");
        assert_eq!(hex::encode(&tx.data), FIRST_BATCH_CALLDATA);

        let SubmitOutcome::RequiresSigning(immutable) =
            PostageVenue::submit(body(deep_immutable_batch())).unwrap()
        else {
            panic!("postage submit must answer requires-signing");
        };
        assert_eq!(hex::encode(&immutable.data), DEEP_IMMUTABLE_CALLDATA);

        // The signer mock accepts, records, and answers a tx hash: the
        // pre-sign leg is proven without executing settlement.
        transport.signer.sign_and_send(tx.clone()).unwrap();
        assert_eq!(transport.signer.signed(), vec![tx]);
    }

    #[test]
    fn resubmitting_the_pre_sign_body_shapes_the_identical_transaction() {
        // The presign leg re-derives from the body alone: a re-submit is
        // byte-identical, which the signer hash makes observable.
        let signer = videre_test::MockSigner::new();
        let submit_tx = || match PostageVenue::submit(body(first_batch())).unwrap() {
            SubmitOutcome::RequiresSigning(tx) => tx,
            SubmitOutcome::Accepted(_) => panic!("postage submit never accepts directly"),
        };
        let first = submit_tx();
        let again = submit_tx();
        assert_eq!(first, again);
        assert_eq!(
            signer.sign_and_send(first).unwrap(),
            signer.sign_and_send(again).unwrap(),
        );
    }

    #[test]
    fn an_off_grant_signer_refuses_the_purchase() {
        // A signer scoped to mainnet only must refuse the gnosis
        // purchase: the grant model has teeth against this adapter's tx.
        let transport = MockTransport::new();
        transport.signer.scope_chains([1]);
        let SubmitOutcome::RequiresSigning(tx) = PostageVenue::submit(body(first_batch())).unwrap()
        else {
            panic!("postage submit must answer requires-signing");
        };
        assert_eq!(
            transport.signer.sign_and_send(tx).unwrap_err(),
            SignError::Denied { chain: GNOSIS },
        );
    }

    #[test]
    fn invalid_batches_are_refused_as_invalid_body() {
        let cases = [
            PostageV1 {
                owner: [0; 20],
                ..first_batch()
            },
            PostageV1 {
                initial_balance_per_chunk: 0,
                ..first_batch()
            },
            PostageV1 {
                bucket_depth: 0,
                ..first_batch()
            },
            PostageV1 {
                depth: 16,
                bucket_depth: 16,
                ..first_batch()
            },
            PostageV1 {
                initial_balance_per_chunk: u64::MAX,
                depth: 255,
                bucket_depth: 16,
                ..first_batch()
            },
        ];
        for batch in cases {
            let bytes = body(batch.clone());
            assert!(
                matches!(
                    PostageVenue::derive_header(bytes.clone()),
                    Err(VenueError::InvalidBody(_)),
                ),
                "derive_header must refuse {batch:?}",
            );
            assert!(
                matches!(
                    PostageVenue::quote(bytes.clone()),
                    Err(VenueError::InvalidBody(_)),
                ),
                "quote must refuse {batch:?}",
            );
            assert!(
                matches!(PostageVenue::submit(bytes), Err(VenueError::InvalidBody(_)),),
                "submit must refuse {batch:?}",
            );
        }
    }

    #[test]
    fn malformed_and_unknown_bodies_are_invalid_body() {
        for bytes in [
            Vec::new(),
            vec![9, 1, 2],
            body(first_batch())[..10].to_vec(),
        ] {
            assert!(matches!(
                PostageVenue::derive_header(bytes),
                Err(VenueError::InvalidBody(_)),
            ));
        }
    }

    #[test]
    fn quote_mirrors_the_header_legs_with_a_zero_fee() {
        let quote = PostageVenue::quote(body(first_batch())).unwrap();
        let header = PostageVenue::derive_header(body(first_batch())).unwrap();
        assert_eq!(quote.gives, header.gives);
        assert_eq!(quote.wants, header.wants);
        assert_eq!(quote.fee, AssetAmount::erc20(BZZ_TOKEN, U256::ZERO));
        assert_eq!(quote.valid_until_ms, u64::MAX);
    }

    #[test]
    fn status_and_cancel_are_unsupported() {
        // Terminal, not retryable: the adapter can never observe the
        // batch, so a keeper must drop the watch rather than re-poll.
        assert!(matches!(
            PostageVenue::status(vec![1]),
            Err(VenueError::Unsupported),
        ));
        assert!(matches!(
            PostageVenue::cancel(vec![1]),
            Err(VenueError::Unsupported),
        ));
    }
}
