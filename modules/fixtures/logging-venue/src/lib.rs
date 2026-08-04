//! # logging-venue (test fixture)
//!
//! Venue adapter declaring only the `logging` capability: `init` installs
//! the standard tracing facade the `#[videre_sdk::venue]` expansion emits
//! for a logging-declaring adapter, then reports adapter-interior facts
//! as structured `tracing` events. The platform tests assert those events
//! reach the host log pipeline with level and fields intact. Test-only.

// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

// `Config` and `Fault` come from the macro's world bindgen at the crate
// root: aliases of the SDK types, so the trait impl lines up.
use videre_sdk::value_flow::{Asset, AssetAmount};
use videre_sdk::{
    AuthScheme, IntentHeader, IntentStatus, Quotation, Settlement, SubmitOutcome, VenueAdapter,
    VenueError,
};

struct LoggingVenue;

#[videre_sdk::venue]
impl VenueAdapter for LoggingVenue {
    fn init(config: Config) -> Result<(), Fault> {
        // The facade first, so everything after it is observable; the
        // events below are what the platform test asserts on.
        install_tracing();
        tracing::info!(flow = "init", "logging-venue facade installed");
        tracing::warn!(
            config_entries = config.len(),
            "logging-venue config sighted"
        );
        Ok(())
    }

    fn derive_header(_body: Vec<u8>) -> Result<IntentHeader, VenueError> {
        Ok(IntentHeader {
            gives: zero_native(),
            wants: zero_native(),
            settlement: Settlement { chain: 1 },
            authorisation: AuthScheme::Eip1271,
        })
    }

    fn quote(_body: Vec<u8>) -> Result<Quotation, VenueError> {
        Ok(Quotation {
            gives: zero_native(),
            wants: zero_native(),
            fee: zero_native(),
            valid_until_ms: u64::MAX,
        })
    }

    fn submit(body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        // An adapter-interior fact per verb: the platform test proves the
        // structured field survives to the host record.
        tracing::info!(
            flow = "submit",
            body_len = body.len(),
            "logging-venue submit"
        );
        Ok(SubmitOutcome::Accepted(body))
    }

    fn status(_receipt: Vec<u8>) -> Result<IntentStatus, VenueError> {
        Ok(IntentStatus::Fulfilled)
    }

    fn cancel(_receipt: Vec<u8>) -> Result<(), VenueError> {
        Ok(())
    }
}

/// A zero native amount.
fn zero_native() -> AssetAmount {
    AssetAmount {
        asset: Asset::Native,
        amount: Vec::new(),
    }
}
