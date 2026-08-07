//! The advisory policy round trip on the postage path: the registry's
//! derive-header, guard, submit sequence run over this adapter's native
//! verbs, following the shape of the host's own
//! `submit_round_trips_through_derive_guard_submit`. The guard reads the
//! policy-legible header (enforceable bzz erc20 `gives`, display-grade
//! service `wants`) and its verdict stays advisory: a deny is logged and
//! the submission proceeds to the requires-signing leg.
//!
//! The adapter is reached natively, through a wire projection of these
//! Rust verbs rather than a booted `postage-venue.wasm` actor, so nothing
//! here proves the guest-boot leg; that stays covered by the echo
//! fixtures under `videre-host/tests`.

use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use videre_host::bindings as wire;
use videre_host::test_utils::{Liveness, SubmitQuota};
use videre_host::{
    EgressGuard, GuardContext, GuardVerdict, VenueId, VenueInvoker, VenueRegistry,
    VenueRegistryBuilder,
};
use videre_sdk::value_flow::{Asset, encode_uint};
use videre_test::MockTransport;

use super::*;

/// Shared call log: the invoker stamps the adapter verbs, the guard its
/// checkpoint, so a test pins the loop's order.
type CallLog = Arc<Mutex<Vec<&'static str>>>;

/// What the guard saw at its checkpoint.
struct GuardSighting {
    caller: String,
    venue: String,
    header: wire::IntentHeader,
}

/// This adapter's verbs as a native [`VenueInvoker`], projected onto the
/// host wire types, so the registry drives the real postage derivations
/// through its submit sequence.
struct PostageInvoker {
    log: CallLog,
}

impl VenueInvoker for PostageInvoker {
    fn derive_header<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<wire::IntentHeader, wire::VenueError>> {
        Box::pin(async move {
            self.log.lock().unwrap().push("derive-header");
            PostageVenue::derive_header(body.to_vec())
                .map(header_to_wire)
                .map_err(error_to_wire)
        })
    }

    fn quote<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<wire::Quotation, wire::VenueError>> {
        Box::pin(async move {
            self.log.lock().unwrap().push("quote");
            PostageVenue::quote(body.to_vec())
                .map(quotation_to_wire)
                .map_err(error_to_wire)
        })
    }

    fn submit<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<wire::SubmitOutcome, wire::VenueError>> {
        Box::pin(async move {
            self.log.lock().unwrap().push("submit");
            PostageVenue::submit(body.to_vec())
                .map(outcome_to_wire)
                .map_err(error_to_wire)
        })
    }

    fn status(
        &mut self,
        receipt: Vec<u8>,
    ) -> BoxFuture<'_, Result<wire::IntentStatus, wire::VenueError>> {
        Box::pin(async move {
            PostageVenue::status(receipt)
                .map(status_to_wire)
                .map_err(error_to_wire)
        })
    }

    fn cancel(&mut self, receipt: Vec<u8>) -> BoxFuture<'_, Result<(), wire::VenueError>> {
        Box::pin(async move { PostageVenue::cancel(receipt).map_err(error_to_wire) })
    }
}

/// Project a guest value-flow leg onto the host wire types.
fn amount_to_wire(leg: AssetAmount) -> wire::value_flow::AssetAmount {
    let asset = match leg.asset {
        Asset::Native => wire::value_flow::Asset::Native,
        Asset::Erc20(erc20) => {
            wire::value_flow::Asset::Erc20(wire::value_flow::Erc20 { token: erc20.token })
        }
        Asset::Service(service) => {
            wire::value_flow::Asset::Service(wire::value_flow::ServiceDesc {
                description: service.description,
            })
        }
    };
    wire::value_flow::AssetAmount {
        asset,
        amount: leg.amount,
    }
}

fn header_to_wire(header: IntentHeader) -> wire::IntentHeader {
    wire::IntentHeader {
        gives: amount_to_wire(header.gives),
        wants: amount_to_wire(header.wants),
        settlement: wire::Settlement {
            chain: header.settlement.chain,
        },
        authorisation: match header.authorisation {
            AuthScheme::Eip1271 => wire::AuthScheme::Eip1271,
            AuthScheme::Eip712 => wire::AuthScheme::Eip712,
        },
    }
}

fn quotation_to_wire(quotation: Quotation) -> wire::Quotation {
    wire::Quotation {
        gives: amount_to_wire(quotation.gives),
        wants: amount_to_wire(quotation.wants),
        fee: amount_to_wire(quotation.fee),
        valid_until_ms: quotation.valid_until_ms,
    }
}

fn outcome_to_wire(outcome: SubmitOutcome) -> wire::SubmitOutcome {
    match outcome {
        SubmitOutcome::Accepted(receipt) => wire::SubmitOutcome::Accepted(receipt),
        SubmitOutcome::RequiresSigning(tx) => {
            wire::SubmitOutcome::RequiresSigning(wire::UnsignedTx {
                chain: tx.chain,
                to: tx.to,
                value: tx.value,
                data: tx.data,
            })
        }
    }
}

fn status_to_wire(status: IntentStatus) -> wire::IntentStatus {
    match status {
        IntentStatus::Pending => wire::IntentStatus::Pending,
        IntentStatus::Open => wire::IntentStatus::Open,
        IntentStatus::Fulfilled => wire::IntentStatus::Fulfilled,
        IntentStatus::Cancelled => wire::IntentStatus::Cancelled,
        IntentStatus::Expired => wire::IntentStatus::Expired,
    }
}

fn error_to_wire(err: VenueError) -> wire::VenueError {
    match err {
        VenueError::UnknownVenue => wire::VenueError::UnknownVenue,
        VenueError::InvalidBody(detail) => wire::VenueError::InvalidBody(detail),
        VenueError::Unsupported => wire::VenueError::Unsupported,
        VenueError::Denied(detail) => wire::VenueError::Denied(detail),
        VenueError::RateLimited(limit) => wire::VenueError::RateLimited(wire::RateLimit {
            retry_after_ms: limit.retry_after_ms,
        }),
        VenueError::Unavailable(detail) => wire::VenueError::Unavailable(detail),
        VenueError::Timeout => wire::VenueError::Timeout,
        VenueError::InvalidReceipt => wire::VenueError::InvalidReceipt,
        VenueError::ReceiptMismatch => wire::VenueError::ReceiptMismatch,
    }
}

/// Hand a wire `unsigned-tx` to the kit's signer as the sdk type.
fn tx_to_sdk(tx: &wire::UnsignedTx) -> UnsignedTx {
    UnsignedTx {
        chain: tx.chain,
        to: tx.to.clone(),
        value: tx.value.clone(),
        data: tx.data.clone(),
    }
}

/// Advisory guard that records its checkpoint and allows the egress.
struct RecordingGuard {
    log: CallLog,
    seen: Arc<Mutex<Vec<GuardSighting>>>,
}

impl EgressGuard for RecordingGuard {
    fn check(&self, ctx: &GuardContext<'_>) -> GuardVerdict {
        self.log.lock().unwrap().push("guard");
        self.seen.lock().unwrap().push(GuardSighting {
            caller: ctx.caller.to_owned(),
            venue: ctx.venue.as_str().to_owned(),
            header: ctx.header.clone(),
        });
        GuardVerdict::Allow
    }
}

/// Advisory guard that denies every egress.
struct DenyGuard {
    log: CallLog,
}

impl EgressGuard for DenyGuard {
    fn check(&self, _ctx: &GuardContext<'_>) -> GuardVerdict {
        self.log.lock().unwrap().push("guard-deny");
        GuardVerdict::Deny("postage purchases refused by test policy".to_owned())
    }
}

/// A valid batch purchase; the loop tests need one body, not a family.
fn purchase() -> PostageV1 {
    PostageV1 {
        owner: [0x11; 20],
        initial_balance_per_chunk: 2_000,
        depth: 20,
        bucket_depth: 16,
        nonce: [0x22; 32],
        immutable: false,
    }
}

fn purchase_body() -> Vec<u8> {
    PostageBody::V1(purchase())
        .to_bytes()
        .expect("body encodes")
}

/// Registry over `guard` with the postage adapter installed under its
/// venue id.
fn registry_with(guard: Arc<dyn EgressGuard>, log: CallLog) -> (VenueRegistry, VenueId) {
    let registry = VenueRegistryBuilder::new(SubmitQuota::default())
        .with_guard(guard)
        .build();
    let venue: VenueId = "postage-venue".parse().expect("valid venue id");
    registry
        .install_for_test(venue.clone(), Liveness::default(), PostageInvoker { log })
        .expect("postage adapter installs");
    (registry, venue)
}

#[tokio::test]
async fn submit_round_trips_through_derive_guard_submit() {
    let log = CallLog::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let guard = Arc::new(RecordingGuard {
        log: Arc::clone(&log),
        seen: Arc::clone(&seen),
    });
    let (registry, venue) = registry_with(guard, Arc::clone(&log));

    let outcome = registry
        .submit("postage-keeper", &venue, purchase_body())
        .await
        .expect("submit succeeds");

    // The loop ran in order: derive, advisory checkpoint, submit.
    assert_eq!(*log.lock().unwrap(), ["derive-header", "guard", "submit"]);

    let sightings = seen.lock().unwrap();
    let [sighting] = sightings.as_slice() else {
        panic!("the guard runs exactly once, saw {}", sightings.len());
    };
    assert_eq!(sighting.caller, "postage-keeper");
    assert_eq!(sighting.venue, "postage-venue");

    // The header the guard read is policy-legible: the enforceable leg
    // is the bzz erc20 total, never a display-grade service.
    let header = &sighting.header;
    let wire::value_flow::Asset::Erc20(bzz) = &header.gives.asset else {
        panic!("gives must be an enforceable erc20 leg");
    };
    assert_eq!(bzz.token, BZZ_TOKEN.as_slice());
    assert_eq!(header.gives.amount, encode_uint(U256::from(2_000u64) << 20));
    let wire::value_flow::Asset::Service(capacity) = &header.wants.asset else {
        panic!("wants is the display-grade capacity leg");
    };
    assert_eq!(capacity.description, POSTAGE_SERVICE);
    assert_eq!(header.wants.amount, encode_uint(U256::ONE << 20));
    assert_eq!(header.settlement.chain, GNOSIS);
    assert!(matches!(header.authorisation, wire::AuthScheme::Eip712));
    drop(sightings);

    // The submit leg answers requires-signing with the adapter's tx,
    // handed through the registry unchanged.
    let wire::SubmitOutcome::RequiresSigning(tx) = outcome else {
        panic!("postage submit must answer requires-signing");
    };
    let direct = match PostageVenue::submit(purchase_body()).expect("direct submit") {
        SubmitOutcome::RequiresSigning(direct) => direct,
        SubmitOutcome::Accepted(_) => panic!("postage submit never accepts directly"),
    };
    assert_eq!(tx_to_sdk(&tx), direct);

    // The requires-signing leg settles at the kit's signer mock.
    let transport = MockTransport::new();
    transport.signer.scope_chains([GNOSIS]);
    transport
        .signer
        .sign_and_send(tx_to_sdk(&tx))
        .expect("the scoped signer accepts the gnosis purchase");
    assert_eq!(transport.signer.signed(), vec![direct]);
}

#[tokio::test]
async fn a_guard_deny_stays_advisory_on_the_postage_path() {
    let log = CallLog::default();
    let guard = Arc::new(DenyGuard {
        log: Arc::clone(&log),
    });
    let (registry, venue) = registry_with(guard, Arc::clone(&log));

    // Advisory-only: the deny is logged and the submission still
    // reaches the adapter, answering requires-signing.
    let outcome = registry
        .submit("postage-keeper", &venue, purchase_body())
        .await
        .expect("an advisory deny does not block");
    assert_eq!(
        *log.lock().unwrap(),
        ["derive-header", "guard-deny", "submit"],
    );

    let wire::SubmitOutcome::RequiresSigning(tx) = outcome else {
        panic!("postage submit must answer requires-signing");
    };
    // The denied-but-advisory purchase still settles its pre-sign leg.
    let transport = MockTransport::new();
    transport.signer.scope_chains([GNOSIS]);
    transport
        .signer
        .sign_and_send(tx_to_sdk(&tx))
        .expect("the signer accepts the advisory-denied purchase");
}

#[tokio::test]
async fn the_priced_and_unsupported_verbs_project_through_the_registry() {
    let log = CallLog::default();
    let guard = Arc::new(RecordingGuard {
        log: Arc::clone(&log),
        seen: Arc::new(Mutex::new(Vec::new())),
    });
    let (registry, venue) = registry_with(guard, Arc::clone(&log));

    let quoted = registry
        .quote("postage-keeper", &venue, purchase_body())
        .await
        .expect("quote succeeds");

    // The priced legs are the header's and the fee is its own zero bzz
    // leg: a transposed projection would move the empty amount.
    let wire::value_flow::Asset::Erc20(fee) = &quoted.fee.asset else {
        panic!("the venue fee is a bzz erc20 leg");
    };
    assert_eq!(fee.token, BZZ_TOKEN.as_slice());
    assert!(
        quoted.fee.amount.is_empty(),
        "the chain charges no venue fee"
    );
    assert_eq!(quoted.gives.amount, encode_uint(U256::from(2_000u64) << 20));
    assert_eq!(quoted.wants.amount, encode_uint(U256::ONE << 20));
    assert_eq!(quoted.valid_until_ms, u64::MAX);

    // A quotation valid past the ledger horizon records nothing, so the
    // quoted bytes still submit rather than being refused as stale.
    registry
        .submit("postage-keeper", &venue, purchase_body())
        .await
        .expect("a horizon-exceeding quote does not stale its submit");

    // Pricing derives no header and runs no checkpoint; only the submit
    // that follows reaches the guard.
    assert_eq!(
        *log.lock().unwrap(),
        ["quote", "derive-header", "guard", "submit"],
    );

    // Both observation verbs stay terminally unsupported through the
    // registry, so a keeper drops the watch rather than re-polling.
    assert!(matches!(
        registry.status(&venue, vec![1]).await,
        Err(wire::VenueError::Unsupported),
    ));
    assert!(matches!(
        registry.cancel(&venue, vec![1]).await,
        Err(wire::VenueError::Unsupported),
    ));
}

#[tokio::test]
async fn an_invalid_purchase_never_reaches_the_guard_or_submit() {
    let log = CallLog::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let guard = Arc::new(RecordingGuard {
        log: Arc::clone(&log),
        seen: Arc::clone(&seen),
    });
    let (registry, venue) = registry_with(guard, Arc::clone(&log));

    let invalid = PostageBody::V1(PostageV1 {
        owner: [0; 20],
        ..purchase()
    })
    .to_bytes()
    .expect("body encodes");
    let err = registry
        .submit("postage-keeper", &venue, invalid)
        .await
        .expect_err("a zero owner is refused at derive");

    // The guard runs on a derived header only: a refused derivation
    // ends the loop before the checkpoint and the adapter's submit.
    assert!(matches!(err, wire::VenueError::InvalidBody(_)));
    assert_eq!(*log.lock().unwrap(), ["derive-header"]);
    assert!(seen.lock().unwrap().is_empty());
}
