//! Acceptance surface for the conformance kit: a reference adapter is held
//! to the published vector and golden files, and a divergent one is caught.

use nexum_sdk::host::ChainHost;
use nexum_sdk::http::Fetch;
use nexum_sdk::prelude::U256;
use videre_sdk::value_flow::{Asset, AssetAmount, decode_uint};
use videre_sdk::{
    AuthScheme, Config, Fault, IntentHeader, IntentStatus, Quotation, SubmitOutcome, VenueAdapter,
    VenueError,
};
use videre_test::reference::{
    CODEC_VECTORS_JSON, HEADER_GOLDENS_JSON, ReferenceBody, derive_reference_header,
};
use videre_test::{
    CodecVectors, HeaderGoldens, MockTransport, UINT_VECTORS_JSON, UintExpectation, UintVectors,
};

/// The reference venue implemented through the SDK trait, driven by the kit's mocks.
struct ReferenceAdapter;

impl VenueAdapter for ReferenceAdapter {
    fn init(_config: Config) -> Result<(), Fault> {
        Ok(())
    }

    fn derive_header(body: Vec<u8>) -> Result<IntentHeader, VenueError> {
        derive_reference_header(body)
    }

    fn quote(body: Vec<u8>) -> Result<Quotation, VenueError> {
        let header = derive_reference_header(body)?;
        Ok(Quotation {
            gives: header.gives,
            wants: header.wants,
            fee: AssetAmount {
                asset: Asset::Native,
                amount: Vec::new(),
            },
            valid_until_ms: u64::MAX,
        })
    }

    fn submit(body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        Ok(SubmitOutcome::Accepted(body))
    }

    fn status(_receipt: Vec<u8>) -> Result<IntentStatus, VenueError> {
        Ok(IntentStatus::Open)
    }

    fn cancel(_receipt: Vec<u8>) -> Result<(), VenueError> {
        Ok(())
    }
}

#[test]
fn adapter_codec_conforms_to_the_published_vectors() {
    CodecVectors::from_json(CODEC_VECTORS_JSON)
        .expect("the published vector file parses")
        .assert_conforms::<ReferenceBody>();
}

#[test]
fn adapter_derive_header_conforms_to_the_published_goldens() {
    HeaderGoldens::from_json(HEADER_GOLDENS_JSON)
        .expect("the published golden file parses")
        .assert_conforms(ReferenceAdapter::derive_header);
}

#[test]
fn divergent_derivation_is_caught_by_the_published_goldens() {
    // The classic byte-order bug: little-endian amounts.
    let derive = |body: Vec<u8>| -> Result<IntentHeader, VenueError> {
        let mut header = derive_reference_header(body)?;
        header.gives.amount.reverse();
        Ok(header)
    };
    let report = HeaderGoldens::from_json(HEADER_GOLDENS_JSON)
        .unwrap()
        .check(derive)
        .unwrap_err();
    assert!(!report.violations.is_empty());
    assert!(report.violations[0].detail.contains("diverges"));
}

#[test]
fn mock_transport_drives_seam_shaped_adapter_logic() {
    // A slice of adapter logic written against the seams: read the head
    // block over chain RPC, then confirm a submission through the venue's
    // HTTP API.
    fn head_block<C: ChainHost>(chain: &C) -> Result<String, VenueError> {
        chain
            .request(1, "eth_blockNumber", "[]")
            .map_err(|err| VenueError::Unavailable(err.to_string()))
    }

    fn confirm<F: Fetch>(fetch: &F, receipt: &[u8]) -> Result<u16, VenueError> {
        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri("https://venue.example/api/v1/receipts")
            .body(receipt.to_vec())
            .expect("the confirmation request builds");
        let response = fetch.fetch(request)?;
        Ok(response.status().as_u16())
    }

    let transport = MockTransport::new();
    transport.http.scope_hosts(["venue.example"]);
    transport.http.respond_to(
        http::Method::POST,
        "https://venue.example/api/v1/receipts",
        202,
        Vec::new(),
    );
    transport
        .chain
        .respond_to("eth_blockNumber", "[]", Ok("\"0x1\"".to_owned()));

    assert_eq!(head_block(&transport).unwrap(), "\"0x1\"");
    let SubmitOutcome::Accepted(receipt) = ReferenceAdapter::submit(vec![1, 2, 3]).unwrap() else {
        panic!("the reference venue accepts directly");
    };
    assert_eq!(confirm(&transport, &receipt).unwrap(), 202);
    assert_eq!(transport.http.last_request().unwrap().body, receipt);
    assert!(matches!(
        ReferenceAdapter::status(receipt).unwrap(),
        IntentStatus::Open,
    ));
    assert_eq!(transport.chain.call_count(), 1);

    // An off-grant host surfaces as the typed policy refusal.
    let stray = http::Request::builder()
        .uri("https://elsewhere.example/")
        .body(Vec::new())
        .expect("the stray request builds");
    let denied = transport
        .fetch(stray)
        .map_err(VenueError::from)
        .unwrap_err();
    assert!(matches!(denied, VenueError::Denied(_)));
}

#[test]
fn the_sdk_uint_codec_conforms_to_the_published_vectors() {
    UintVectors::from_json(UINT_VECTORS_JSON)
        .expect("the published vector file parses")
        .assert_conforms(decode_uint);
}

#[test]
fn a_tolerant_uint_decoder_is_caught_by_the_published_vectors() {
    // The classic uint bug: normalise the padding away instead of rejecting it.
    let tolerant = |bytes: &[u8]| -> Result<U256, &'static str> {
        if bytes.len() > 32 {
            return Err("too long");
        }
        Ok(U256::from_be_slice(bytes))
    };
    let vectors = UintVectors::from_json(UINT_VECTORS_JSON).unwrap();
    let report = vectors.check(tolerant).unwrap_err();
    let failed: Vec<&str> = report
        .violations
        .iter()
        .map(|violation| violation.vector.as_str())
        .collect();
    let rejects: Vec<&str> = vectors
        .vectors
        .iter()
        .filter(|vector| vector.expect == UintExpectation::Reject && vector.bytes.len() <= 32)
        .map(|vector| vector.name.as_str())
        .collect();
    assert_eq!(failed, rejects, "report: {report}");
}

#[test]
fn uint_vectors_round_trip_through_disk() {
    let vectors = UintVectors::from_json(UINT_VECTORS_JSON).unwrap();
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("uint.json");
    vectors.write(&path).expect("the file writes");
    assert_eq!(UintVectors::load(&path).expect("the file loads"), vectors);
}

#[test]
fn published_files_document_the_wire_format_in_hex() {
    // Non-Rust authors consume the files directly: every byte field is
    // lowercase hex, and the first round-trip vector carries prose.
    let vectors = CodecVectors::from_json(CODEC_VECTORS_JSON).unwrap();
    assert!(vectors.vectors.iter().any(|vector| vector.notes.is_some()));

    let goldens = HeaderGoldens::from_json(HEADER_GOLDENS_JSON).unwrap();
    let golden = &goldens.goldens[0];
    // The golden's body is a codec vector's bytes: the two files pin
    // the same wire form from both sides.
    assert!(
        vectors
            .vectors
            .iter()
            .any(|vector| vector.bytes == golden.body),
        "header goldens reuse published codec bodies",
    );
    // And the expected header speaks the value-flow vocabulary.
    let derived = derive_reference_header(golden.body.clone()).unwrap();
    assert_eq!(derived.gives.asset, Asset::Native);
    assert_eq!(derived.authorisation, AuthScheme::Eip712);
    let _: &AssetAmount = &derived.gives;
}
