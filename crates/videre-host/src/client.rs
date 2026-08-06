//! `videre:venue/client`: the keeper-facing venue import. Every method
//! resolves the shared [`VenueRegistry`] from the store's service map and
//! delegates, metering against this store's module namespace. No registry
//! service resolves every call to `unknown-venue`.

use std::sync::Arc;

use nexum_runtime::host::component::RuntimeTypes;
use nexum_runtime::host::state::HostState;

use crate::bindings::client::Host;
use crate::bindings::{IntentStatus, Quotation, SubmitOutcome, VenueError};
use crate::registry::{VenueId, VenueRegistry};

/// The registry published under the videre service namespace.
fn registry<T: RuntimeTypes>(state: &HostState<T>) -> Result<Arc<VenueRegistry>, VenueError> {
    state
        .services
        .get::<VenueRegistry>(VenueRegistry::NAMESPACE)
        .ok_or(VenueError::UnknownVenue)
}

/// Validate the wire venue id at the host import. A blank or padded id
/// can never be installed, so it is `unknown-venue`, not a resolvable
/// venue.
fn venue_id(venue: String) -> Result<VenueId, VenueError> {
    VenueId::new(venue).map_err(|_| VenueError::UnknownVenue)
}

impl<T: RuntimeTypes> Host for HostState<T> {
    async fn quote(&mut self, venue: String, body: Vec<u8>) -> Result<Quotation, VenueError> {
        registry(self)?
            .quote(self.run.module.as_str(), &venue_id(venue)?, body)
            .await
    }

    async fn submit(&mut self, venue: String, body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        registry(self)?
            .submit(self.run.module.as_str(), &venue_id(venue)?, body)
            .await
    }

    async fn observe(&mut self, venue: String, receipt: Vec<u8>) -> Result<(), VenueError> {
        registry(self)?.observe(&venue_id(venue)?, receipt)
    }

    async fn status(
        &mut self,
        venue: String,
        receipt: Vec<u8>,
    ) -> Result<IntentStatus, VenueError> {
        registry(self)?.status(&venue_id(venue)?, receipt).await
    }

    async fn cancel(&mut self, venue: String, receipt: Vec<u8>) -> Result<(), VenueError> {
        registry(self)?.cancel(&venue_id(venue)?, receipt).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam every `Host` method routes the wire venue id through, and
    /// the only one a padded id reaches from a guest.
    #[test]
    fn a_blank_or_padded_wire_venue_id_is_unknown_venue() {
        for bad in ["", "  ", "cow ", " cow", "cow\n", "cow\u{a0}"] {
            assert_eq!(
                venue_id(bad.to_owned()).unwrap_err(),
                VenueError::UnknownVenue,
                "{bad:?}",
            );
        }
        assert_eq!(venue_id("cow".to_owned()).expect("parses").as_str(), "cow");
    }
}
