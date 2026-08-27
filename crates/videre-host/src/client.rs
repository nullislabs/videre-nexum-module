//! `videre:venue/client`: the keeper-facing venue import. Every method
//! resolves the shared [`VenueRegistry`] and delegates, metering against
//! this store's module namespace. No published registry resolves every call
//! to `unknown-venue`.
//!
//! The registry lives in a process-wide slot rather than in the store: the
//! bindgen `add_to_linker` getter is a plain `fn` pointer that cannot
//! capture, and `HostState` no longer carries the service map the runtime
//! removed with the extension-installed component path. One composition
//! root wires one [`Videre`](crate::Videre), so one slot is the whole
//! process's registry.

use std::sync::{Arc, OnceLock};

use nexum_runtime::component::RuntimeTypes;
use nexum_runtime::extension::HostState;

use crate::bindings::client::Host;
use crate::bindings::{IntentStatus, Quotation, SubmitOutcome, VenueError};
use crate::registry::{VenueId, VenueRegistry};

/// The process-wide registry, published at link time.
static REGISTRY: OnceLock<Arc<VenueRegistry>> = OnceLock::new();

/// Publish the registry the client glue delegates to. The first publish
/// wins; a second is ignored, so wiring two platforms in one process keeps
/// the first rather than tearing a live registry out from under a store.
pub(crate) fn publish_registry(registry: Arc<VenueRegistry>) {
    let _ = REGISTRY.set(registry);
}

/// The registry published for this process.
fn registry() -> Result<&'static Arc<VenueRegistry>, VenueError> {
    REGISTRY.get().ok_or(VenueError::UnknownVenue)
}

/// Validate the wire venue id at the host import. A blank id can never be
/// installed, so it is `unknown-venue`, not a resolvable venue.
fn venue_id(venue: String) -> Result<VenueId, VenueError> {
    VenueId::new(venue).map_err(|_| VenueError::UnknownVenue)
}

impl<T: RuntimeTypes> Host for HostState<T> {
    async fn quote(&mut self, venue: String, body: Vec<u8>) -> Result<Quotation, VenueError> {
        registry()?
            .quote(self.run.module.as_str(), &venue_id(venue)?, body)
            .await
    }

    async fn submit(&mut self, venue: String, body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        registry()?
            .submit(self.run.module.as_str(), &venue_id(venue)?, body)
            .await
    }

    async fn observe(&mut self, venue: String, receipt: Vec<u8>) -> Result<(), VenueError> {
        registry()?.observe(&venue_id(venue)?, receipt)
    }

    async fn status(
        &mut self,
        venue: String,
        receipt: Vec<u8>,
    ) -> Result<IntentStatus, VenueError> {
        registry()?.status(&venue_id(venue)?, receipt).await
    }

    async fn cancel(&mut self, venue: String, receipt: Vec<u8>) -> Result<(), VenueError> {
        registry()?.cancel(&venue_id(venue)?, receipt).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::SubmitQuota;
    use crate::registry::VenueRegistryBuilder;

    /// The seam every `Host` method routes the wire venue id through. The
    /// `VenueId` field is private to `registry`, so no method here can
    /// build one that skipped this check.
    #[test]
    fn a_blank_wire_venue_id_is_unknown_venue() {
        assert!(matches!(
            venue_id(String::new()),
            Err(VenueError::UnknownVenue)
        ));
        assert!(matches!(
            venue_id("  ".to_owned()),
            Err(VenueError::UnknownVenue)
        ));
        assert_eq!(venue_id("cow".to_owned()).expect("parses").as_str(), "cow");
    }

    fn empty_registry() -> Arc<VenueRegistry> {
        Arc::new(VenueRegistryBuilder::new(SubmitQuota::default()).build())
    }

    /// The slot is process-wide, so this is the only test that publishes:
    /// the first publish wins and a second is ignored, rather than tearing
    /// a live registry out from under a store.
    #[test]
    fn the_first_published_registry_wins() {
        assert!(
            registry().is_err(),
            "the slot is process-wide and this test must be its only \
             publisher: no other unit test may call publish_registry or \
             Videre::link",
        );

        let first = empty_registry();
        publish_registry(Arc::clone(&first));
        publish_registry(empty_registry());

        let resolved = registry().expect("the published registry resolves");
        assert!(Arc::ptr_eq(resolved, &first), "the first publish wins");
    }
}
