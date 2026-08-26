//! # flaky-venue (test fixture)
//!
//! Evil-by-design venue adapter: `submit` fails while the chain head reads
//! the poison sentinel [`POISON_HEAD`], and serves again once the head
//! moves on and a sweep revives it. The fixture drives recovery: the
//! registry must route around a dead venue, and a submit must succeed only
//! after the sweep. Test-only.
//!
//! ## How the misbehaviour changed
//!
//! This was a guest wasm component whose `submit` panicked, which trapped
//! its store and left the supervisor to reinstantiate the adapter. A venue
//! is now a native Rust adapter in the host process, so a panic would
//! unwind into the caller (and abort, because both profiles set
//! `panic = "abort"`). That is a fixture that kills the test process, not a
//! fixture that exercises recovery.
//!
//! The honest analogue of a trapped adapter is therefore a two-part
//! failure, not a panic:
//!
//! 1. The poisoned `submit` answers
//!    [`VenueError::Unavailable`], the same error the caller saw from a
//!    trapped store.
//! 2. It marks its [`Liveness`] dead, so the registry resolves every later
//!    call to `unavailable` without reaching the adapter. That is what a
//!    poisoned store did: the venue stayed installed and stopped serving.
//!
//! Recovery is explicit, because nothing supervises a native adapter:
//! [`FlakyHandle::sweep`] revives the venue once the head is healthy. It
//! stands in for the supervisor sweep the wasm fixture leaned on.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use std::sync::{Arc, Mutex, MutexGuard};

use futures::FutureExt;
use futures::future::BoxFuture;
use videre_host::bindings::value_flow::{Asset, AssetAmount};
use videre_host::bindings::{
    AuthScheme, IntentHeader, IntentStatus, Quotation, Settlement, SubmitOutcome, VenueError,
};
use videre_host::{DuplicateVenue, Liveness, VenueId, VenueInvoker, VenueRegistry};

/// The chain-head response that detonates `submit`.
pub const POISON_HEAD: &str = "0xdead";

/// A healthy chain head, for the recovered half of the fixture.
pub const HEALTHY_HEAD: &str = "0x1";

/// The chain the fixture settles on.
pub const SETTLEMENT_CHAIN: u64 = 1;

/// The mock chain head the fixture reads, shared with whoever set it up.
///
/// It replaces the `chain::request` host import the wasm fixture called.
/// Cheap to clone; every clone observes the same head.
#[derive(Clone, Debug)]
pub struct ChainHead(Arc<Mutex<String>>);

impl ChainHead {
    /// A head at the given response.
    #[must_use]
    pub fn new(head: impl Into<String>) -> Self {
        Self(Arc::new(Mutex::new(head.into())))
    }

    /// A head at [`POISON_HEAD`].
    #[must_use]
    pub fn poisoned() -> Self {
        Self::new(POISON_HEAD)
    }

    /// A head at [`HEALTHY_HEAD`].
    #[must_use]
    pub fn healthy() -> Self {
        Self::new(HEALTHY_HEAD)
    }

    /// The current head response.
    #[must_use]
    pub fn get(&self) -> String {
        self.lock().clone()
    }

    /// Move the head to a new response.
    pub fn set(&self, head: impl Into<String>) {
        *self.lock() = head.into();
    }

    /// Move the head off the sentinel.
    pub fn heal(&self) {
        self.set(HEALTHY_HEAD);
    }

    /// Whether the head currently reads the sentinel.
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.lock().contains(POISON_HEAD)
    }

    /// The head, recovered from a poisoned lock.
    fn lock(&self) -> MutexGuard<'_, String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Default for ChainHead {
    /// A poisoned head: the fixture is evil by default.
    fn default() -> Self {
        Self::poisoned()
    }
}

/// The fixture adapter. It reads [`ChainHead`] on every submit and kills
/// itself while the head reads the sentinel.
#[derive(Clone, Debug)]
pub struct FlakyVenue {
    head: ChainHead,
    liveness: Liveness,
}

impl FlakyVenue {
    /// An adapter over `head`, live until its first poisoned submit.
    #[must_use]
    pub fn new(head: ChainHead) -> Self {
        Self {
            head,
            liveness: Liveness::new(),
        }
    }

    /// The liveness flag the adapter kills, shared with the registry.
    #[must_use]
    pub fn liveness(&self) -> &Liveness {
        &self.liveness
    }

    /// The head the adapter reads.
    #[must_use]
    pub fn head(&self) -> &ChainHead {
        &self.head
    }

    /// Submit, failing while the head reads the sentinel.
    ///
    /// # Errors
    ///
    /// Returns `Unavailable` on a poisoned head, and marks the adapter
    /// dead first, so the registry answers `unavailable` for every later
    /// call until a sweep revives it.
    fn submit_now(&mut self, body: &[u8]) -> Result<SubmitOutcome, VenueError> {
        if self.head.is_poisoned() {
            // Where the wasm fixture panicked and trapped its store, the
            // native fixture kills itself and reports the same error the
            // trap surfaced to the caller.
            self.liveness.mark_dead();
            return Err(VenueError::Unavailable(
                "flaky-venue poison head".to_owned(),
            ));
        }
        Ok(SubmitOutcome::Accepted(body.to_vec()))
    }
}

impl VenueInvoker for FlakyVenue {
    fn derive_header<'a>(
        &'a mut self,
        _body: &'a [u8],
    ) -> BoxFuture<'a, Result<IntentHeader, VenueError>> {
        async move {
            Ok(IntentHeader {
                gives: zero_native(),
                wants: zero_native(),
                settlement: Settlement {
                    chain: SETTLEMENT_CHAIN,
                },
                authorisation: AuthScheme::Eip1271,
            })
        }
        .boxed()
    }

    fn quote<'a>(&'a mut self, _body: &'a [u8]) -> BoxFuture<'a, Result<Quotation, VenueError>> {
        async move {
            Ok(Quotation {
                gives: zero_native(),
                wants: zero_native(),
                fee: zero_native(),
                valid_until_ms: u64::MAX,
            })
        }
        .boxed()
    }

    fn submit<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<SubmitOutcome, VenueError>> {
        async move { self.submit_now(body) }.boxed()
    }

    fn status(&mut self, _receipt: Vec<u8>) -> BoxFuture<'_, Result<IntentStatus, VenueError>> {
        async move { Ok(IntentStatus::Open) }.boxed()
    }

    fn cancel(&mut self, _receipt: Vec<u8>) -> BoxFuture<'_, Result<(), VenueError>> {
        async move { Ok(()) }.boxed()
    }
}

/// What a test keeps after registration: the head to poison or heal, and
/// the liveness flag the adapter kills.
///
/// The fixture registers with an empty body-version set, because the wasm
/// module declared no `[venue]` section. It exercises the opt-out path of
/// the keeper handshake.
#[derive(Clone, Debug)]
pub struct FlakyHandle {
    head: ChainHead,
    liveness: Liveness,
}

impl FlakyHandle {
    /// The head the adapter reads.
    #[must_use]
    pub fn head(&self) -> &ChainHead {
        &self.head
    }

    /// The adapter's liveness, as the registry sees it.
    #[must_use]
    pub fn liveness(&self) -> &Liveness {
        &self.liveness
    }

    /// Whether the registry still routes to the adapter.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.liveness.is_alive()
    }

    /// Poison the head. The next submit kills the adapter.
    pub fn poison(&self) {
        self.head.set(POISON_HEAD);
    }

    /// Move the head off the sentinel. The adapter stays dead until
    /// [`sweep`](Self::sweep) revives it.
    pub fn heal(&self) {
        self.head.heal();
    }

    /// Revive the adapter when the head is healthy, and report whether it
    /// now serves.
    ///
    /// This stands in for the supervisor sweep that restarted the wasm
    /// fixture: nothing supervises a native adapter, so recovery is an
    /// explicit call.
    pub fn sweep(&self) -> bool {
        if !self.head.is_poisoned() {
            self.liveness.mark_alive();
        }
        self.is_alive()
    }
}

/// Register a fixture adapter under `venue`, over `head`.
///
/// # Errors
///
/// Returns [`DuplicateVenue`] when a live venue already claims the id.
pub fn register(
    registry: &VenueRegistry,
    venue: VenueId,
    head: ChainHead,
) -> Result<FlakyHandle, DuplicateVenue> {
    let adapter = FlakyVenue::new(head.clone());
    let liveness = adapter.liveness().clone();
    // The empty set is the opt-out: this fixture declares no body versions,
    // so it constrains no keeper handshake.
    registry.register(
        venue,
        liveness.clone(),
        std::collections::BTreeSet::new(),
        adapter,
    )?;
    Ok(FlakyHandle { head, liveness })
}

/// A zero native amount.
fn zero_native() -> AssetAmount {
    AssetAmount {
        asset: Asset::Native,
        amount: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{ChainHead, register};
    use std::collections::BTreeSet;
    use videre_host::bindings::{SubmitOutcome, VenueError};
    use videre_host::{SubmitQuota, VenueId, VenueRegistryBuilder};

    fn registry() -> videre_host::VenueRegistry {
        VenueRegistryBuilder::new(SubmitQuota::default()).build()
    }

    #[tokio::test]
    async fn a_poisoned_submit_kills_the_venue_and_the_registry_routes_around_it() {
        let registry = registry();
        let venue = VenueId::new("flaky-venue").expect("a valid venue id");
        let handle =
            register(&registry, venue.clone(), ChainHead::poisoned()).expect("registration");

        // The first submit reaches the adapter and detonates.
        let first = registry.submit("mod-a", &venue, b"body".to_vec()).await;
        assert!(matches!(first, Err(VenueError::Unavailable(_))));
        assert!(!handle.is_alive(), "the poisoned submit kills the adapter");

        // The second never reaches it: the registry resolves a dead venue
        // to `unavailable`, not to `unknown-venue`.
        let second = registry.submit("mod-a", &venue, b"body".to_vec()).await;
        assert!(matches!(second, Err(VenueError::Unavailable(_))));
    }

    #[tokio::test]
    async fn healing_the_head_alone_does_not_revive_the_venue() {
        let registry = registry();
        let venue = VenueId::new("flaky-venue").expect("a valid venue id");
        let handle =
            register(&registry, venue.clone(), ChainHead::poisoned()).expect("registration");

        let _ = registry.submit("mod-a", &venue, b"body".to_vec()).await;
        handle.heal();
        assert!(!handle.is_alive(), "recovery waits for the sweep");
        assert!(matches!(
            registry.submit("mod-a", &venue, b"body".to_vec()).await,
            Err(VenueError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn a_sweep_over_a_healthy_head_restores_the_venue() {
        let registry = registry();
        let venue = VenueId::new("flaky-venue").expect("a valid venue id");
        let handle =
            register(&registry, venue.clone(), ChainHead::poisoned()).expect("registration");

        let _ = registry.submit("mod-a", &venue, b"body".to_vec()).await;
        // A sweep against the sentinel leaves the venue dead.
        assert!(!handle.sweep());
        handle.heal();
        assert!(handle.sweep(), "a healthy head revives the adapter");

        let outcome = registry
            .submit("mod-a", &venue, b"body".to_vec())
            .await
            .expect("the revived adapter accepts");
        assert_eq!(outcome, SubmitOutcome::Accepted(b"body".to_vec()));
    }

    #[tokio::test]
    async fn a_healthy_fixture_accepts_and_can_be_poisoned_again() {
        let registry = registry();
        let venue = VenueId::new("flaky-venue").expect("a valid venue id");
        let handle =
            register(&registry, venue.clone(), ChainHead::healthy()).expect("registration");

        assert!(
            registry
                .submit("mod-a", &venue, b"body".to_vec())
                .await
                .is_ok()
        );
        handle.poison();
        assert!(matches!(
            registry.submit("mod-a", &venue, b"body".to_vec()).await,
            Err(VenueError::Unavailable(_))
        ));
        assert!(!handle.is_alive());
    }

    #[test]
    fn the_fixture_declares_no_body_versions() {
        let registry = registry();
        let venue = VenueId::new("flaky-venue").expect("a valid venue id");
        register(&registry, venue.clone(), ChainHead::healthy()).expect("registration");
        assert_eq!(
            registry.body_versions().get(&venue).map(BTreeSet::len),
            Some(0),
            "the fixture takes the handshake opt-out",
        );
    }
}
