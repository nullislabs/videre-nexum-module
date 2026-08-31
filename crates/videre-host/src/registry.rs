//! The venue registry behind the keeper-facing `videre:venue/client`
//! import: resolves a venue id to its installed adapter and drives the
//! submit sequence (derive header, advisory [`EgressGuard`] seam, submit).
//! Status, cancel, and observe skip the header, guard, and quota.
//!
//! Each adapter sits behind its own [`ActorSlot`], so calls to one venue
//! serialise while calls to different venues run in parallel. A per-caller
//! quota gates every quote and submit; a decode failure is charged to the
//! calling module, so a caller feeding garbage exhausts its own budget.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use nexum_runtime::ModuleId;
use nexum_runtime::error::InvalidModuleName;
use tokio::sync::Mutex as AsyncMutex;
use tracing::warn;
use videre_status_body::StatusBody;

use crate::policy::{Liveness, SubmitQuota, WatchLimit};

/// Status transition carried in the `custom` event payload.
pub use videre_status_body::IntentStatusUpdate;

use crate::bindings::{
    IntentHeader, IntentStatus, Quotation, RateLimit, SubmitOutcome, VenueError,
};

/// Venue identifier an adapter registers under. Opaque beyond equality.
/// Backed by [`ModuleId`], so a clone on the dispatch path is a refcount
/// bump. [`VenueId::new`] is the only constructor.
#[derive(Clone, Debug, Eq, Hash, PartialEq, derive_more::Display)]
pub struct VenueId(ModuleId);

/// A candidate venue id failed validation at a boundary.
#[derive(Debug, thiserror::Error)]
#[error("invalid venue id: {0}")]
pub struct InvalidVenueId(#[from] InvalidModuleName);

impl VenueId {
    /// Validating constructor, delegating to [`ModuleId::parse`]: a venue
    /// id is a manifest namespace, so it obeys the same rule. A padded id
    /// is rejected, never trimmed.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidVenueId`] when `id` is blank, whitespace-padded,
    /// or reaches outside its own namespace.
    pub fn new(id: impl AsRef<str>) -> Result<Self, InvalidVenueId> {
        Ok(Self(ModuleId::parse(id.as_ref())?))
    }

    /// The id at its wire spelling.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl AsRef<str> for VenueId {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl std::str::FromStr for VenueId {
    type Err = InvalidVenueId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Egress interposition run on the derived header between `derive-header`
/// and `submit`. Advisory-only: a `Deny` is logged, the submission proceeds.
pub trait EgressGuard: Send + Sync {
    /// Decide whether the derived header may proceed to the adapter's submit.
    fn check(&self, ctx: &GuardContext<'_>) -> GuardVerdict;
}

/// The unit guard: allow every egress.
impl EgressGuard for () {
    fn check(&self, _ctx: &GuardContext<'_>) -> GuardVerdict {
        GuardVerdict::Allow
    }
}

/// What the guard sees. The raw body never reaches it, only the derived
/// header.
pub struct GuardContext<'a> {
    /// Namespace of the calling module.
    pub caller: &'a str,
    /// Venue the submission is routed to.
    pub venue: &'a VenueId,
    /// Adapter-derived header for the body.
    pub header: &'a IntentHeader,
}

/// The guard's decision on one egress.
pub enum GuardVerdict {
    /// Forward the submission to the adapter.
    Allow,
    /// Refuse with an operator-facing reason. Logged, not enforced.
    Deny(String),
}

/// Per-adapter invocation seam, reached behind an async mutex. Boxed
/// futures so heterogeneous adapters share one `dyn` slot.
pub trait VenueInvoker: Send {
    /// Project the opaque body onto the stable header the guard runs on.
    fn derive_header<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<IntentHeader, VenueError>>;

    /// Price the opaque body at this adapter's venue.
    fn quote<'a>(&'a mut self, body: &'a [u8]) -> BoxFuture<'a, Result<Quotation, VenueError>>;

    /// Submit the opaque body to this adapter's venue.
    fn submit<'a>(&'a mut self, body: &'a [u8])
    -> BoxFuture<'a, Result<SubmitOutcome, VenueError>>;

    /// Report an intent's lifecycle state.
    fn status(&mut self, receipt: Vec<u8>) -> BoxFuture<'_, Result<IntentStatus, VenueError>>;

    /// Ask the venue to withdraw an intent.
    fn cancel(&mut self, receipt: Vec<u8>) -> BoxFuture<'_, Result<(), VenueError>>;
}

/// One installed adapter behind its serialising slot.
type AdapterSlot = Arc<AsyncMutex<dyn VenueInvoker>>;

/// One installed venue: adapter slot plus shared liveness. A dead entry
/// stays installed, resolving to `unavailable` (not `unknown-venue`) until
/// the sweep restarts it.
struct InstalledVenue {
    slot: AdapterSlot,
    liveness: Liveness,
    /// The body-schema versions this venue decodes, the authority the
    /// keeper handshake reads.
    body_versions: BTreeSet<u32>,
}

/// Per-caller charge history, pruned to the quota window on each touch.
#[derive(Default)]
struct QuotaLedger {
    per_caller: HashMap<String, VecDeque<Instant>>,
}

/// One receipt polled for status transitions. `last` starts `None` so the
/// first successful poll always reports. `expires_at` is the give-up
/// deadline, refreshed a `grace` window out whenever the venue is reachable;
/// `None` (arithmetic overflow) never expires.
struct WatchedIntent {
    venue: VenueId,
    receipt: Vec<u8>,
    last: Option<IntentStatus>,
    expires_at: Option<Instant>,
}

/// Whether a status is terminal, after which the receipt is dropped from
/// the watch.
fn is_terminal(status: IntentStatus) -> bool {
    matches!(
        status,
        IntentStatus::Fulfilled | IntentStatus::Cancelled | IntentStatus::Expired
    )
}

/// Lower a polled status onto the opaque status body. The registry attests
/// the lifecycle state only; proof and reason stay `None`.
fn status_body(status: IntentStatus) -> StatusBody {
    use videre_status_body::IntentStatus as Lifecycle;

    let status = match status {
        IntentStatus::Pending => Lifecycle::Pending,
        IntentStatus::Open => Lifecycle::Open,
        IntentStatus::Fulfilled => Lifecycle::Fulfilled,
        IntentStatus::Cancelled => Lifecycle::Cancelled,
        IntentStatus::Expired => Lifecycle::Expired,
    };
    StatusBody {
        status,
        proof: None,
        reason: None,
    }
}

/// Shared registry state behind the `Arc`. Every module store carries the
/// same handle, so all reach the same adapters and quota ledger.
struct VenueRegistryInner {
    adapters: Mutex<HashMap<VenueId, InstalledVenue>>,
    guard: Arc<dyn EgressGuard>,
    quota: SubmitQuota,
    ledger: Mutex<QuotaLedger>,
    watch_limit: WatchLimit,
    /// Receipts under status watch; pruned on terminal status, expiry, or
    /// [`WatchLimit`] overflow.
    watched: Mutex<Vec<WatchedIntent>>,
}

/// The keeper-facing venue registry, cheap to clone.
#[derive(Clone)]
pub struct VenueRegistry {
    inner: Arc<VenueRegistryInner>,
}

impl VenueRegistry {
    /// Service namespace: the videre extension's.
    pub const NAMESPACE: &'static str = "videre";

    /// Register a venue under its id, sharing `liveness`. Rejects a
    /// duplicate id while the incumbent is alive; replaces a dead incumbent.
    /// The composition root calls this before launch.
    pub fn register(
        &self,
        venue: VenueId,
        liveness: Liveness,
        body_versions: BTreeSet<u32>,
        invoker: impl VenueInvoker + 'static,
    ) -> Result<(), DuplicateVenue> {
        // Takes the adapter-map mutex only for the synchronous insert; never
        // held across an await.
        let mut adapters = self.inner.adapters.lock().expect("adapter map poisoned");
        if adapters.get(&venue).is_some_and(|v| v.liveness.is_alive()) {
            return Err(DuplicateVenue { venue });
        }
        adapters.insert(
            venue,
            InstalledVenue {
                slot: Arc::new(AsyncMutex::new(invoker)),
                liveness,
                body_versions,
            },
        );
        Ok(())
    }

    /// Test-only direct install, bypassing the provider boot path.
    #[cfg(feature = "test-utils")]
    pub fn install_for_test(
        &self,
        venue: VenueId,
        liveness: Liveness,
        invoker: impl VenueInvoker + 'static,
    ) -> Result<(), DuplicateVenue> {
        self.register(venue, liveness, BTreeSet::new(), invoker)
    }

    /// Every registered venue's declared body-version set, the input to the
    /// keeper handshake that [`crate::Videre`] runs in its `admit_worker`
    /// hook.
    #[must_use]
    pub fn body_versions(&self) -> HashMap<VenueId, BTreeSet<u32>> {
        self.inner
            .adapters
            .lock()
            .expect("adapter map poisoned")
            .iter()
            .map(|(venue, installed)| (venue.clone(), installed.body_versions.clone()))
            .collect()
    }

    /// Resolve a venue id to its slot: uninstalled is `unknown-venue`,
    /// installed-but-dead is `unavailable` pending the restart sweep.
    fn resolve(&self, venue: &VenueId) -> Result<AdapterSlot, VenueError> {
        let adapters = self.inner.adapters.lock().expect("adapter map poisoned");
        let installed = adapters.get(venue).ok_or(VenueError::UnknownVenue)?;
        if !installed.liveness.is_alive() {
            return Err(VenueError::Unavailable(format!(
                "venue {venue} is dead pending restart"
            )));
        }
        Ok(Arc::clone(&installed.slot))
    }

    /// Whether `caller` has budget left in the window. Prunes aged charges
    /// but records none.
    fn quota_admits(&self, caller: &str) -> bool {
        let mut ledger = self.inner.ledger.lock().expect("quota ledger poisoned");
        let history = ledger.per_caller.entry(caller.to_owned()).or_default();
        prune(history, self.inner.quota.window);
        (history.len() as u32) < self.inner.quota.max_charges
    }

    /// Record one charge against `caller`'s budget.
    fn charge(&self, caller: &str) {
        let mut ledger = self.inner.ledger.lock().expect("quota ledger poisoned");
        let history = ledger.per_caller.entry(caller.to_owned()).or_default();
        prune(history, self.inner.quota.window);
        history.push_back(Instant::now());
    }

    /// Submit an opaque body to `venue` for `caller`: resolve, quota-gate,
    /// derive the header, run the advisory guard, forward to the adapter.
    /// Charged once the header derives (ahead of guard and adapter), plus on
    /// a decode failure; other derive-stage errors stay uncharged and
    /// retryable.
    pub async fn submit(
        &self,
        caller: &str,
        venue: &VenueId,
        body: Vec<u8>,
    ) -> Result<SubmitOutcome, VenueError> {
        let slot = self.resolve(venue)?;
        // Gate before touching the adapter so a quota-exhausted caller never
        // reaches the adapter store or its mutex. Exhaustion is retryable
        // once the window slides, so it is rate-limited, never denied.
        if !self.quota_admits(caller) {
            return Err(VenueError::RateLimited(RateLimit {
                retry_after_ms: Some(window_ms(self.inner.quota.window)),
            }));
        }
        let mut adapter = slot.lock().await;
        let header = match adapter.derive_header(&body).await {
            Ok(header) => header,
            Err(e) => {
                // Charge decode failures to the caller before the adapter is
                // invoked again; other venue errors are not the caller's fault.
                if matches!(e, VenueError::InvalidBody(_)) {
                    self.charge(caller);
                }
                return Err(e);
            }
        };
        let ctx = GuardContext {
            caller,
            venue,
            header: &header,
        };
        // Charge before the guard so an enforcing deny stays non-free.
        self.charge(caller);
        // Advisory-only checkpoint: a deny is logged, never enforced.
        if let GuardVerdict::Deny(reason) = self.inner.guard.check(&ctx) {
            warn!(
                caller,
                venue = %venue,
                reason,
                "egress guard would deny - advisory-only, submission proceeds",
            );
        }
        let outcome = adapter.submit(&body).await?;
        // An accepted receipt goes under status watch so subscribers see
        // its transitions; requires-signing has no receipt to watch yet.
        if let SubmitOutcome::Accepted(receipt) = &outcome {
            self.watch(venue, receipt.clone());
        }
        Ok(outcome)
    }

    /// Price an opaque body at `venue` for `caller`. No header or guard, but
    /// quota-gated: each quote spends one unit.
    pub async fn quote(
        &self,
        caller: &str,
        venue: &VenueId,
        body: Vec<u8>,
    ) -> Result<Quotation, VenueError> {
        let slot = self.resolve(venue)?;
        if !self.quota_admits(caller) {
            return Err(VenueError::RateLimited(RateLimit {
                retry_after_ms: Some(window_ms(self.inner.quota.window)),
            }));
        }
        self.charge(caller);
        let mut adapter = slot.lock().await;
        adapter.quote(&body).await
    }

    /// Put an externally-obtained `(venue, receipt)` under status watch, for
    /// receipts the registry never submitted. No header, guard, or quota;
    /// watch-cap bounded. Idempotent.
    pub fn observe(&self, venue: &VenueId, receipt: Vec<u8>) -> Result<(), VenueError> {
        let _ = self.resolve(venue)?;
        if self.watch(venue, receipt) {
            Ok(())
        } else {
            Err(VenueError::Unavailable("status watch set full".to_owned()))
        }
    }

    /// Put a `(venue, receipt)` under watch, reporting whether admitted.
    /// Idempotent. Bounded: expired entries evict first; at the cap the new
    /// watch is refused, not an existing one dropped.
    fn watch(&self, venue: &VenueId, receipt: Vec<u8>) -> bool {
        let (evicted, admitted) = {
            let mut watched = self.inner.watched.lock().expect("watch list poisoned");
            let evicted = prune_expired(&mut watched);
            if watched
                .iter()
                .any(|w| w.venue == *venue && w.receipt == receipt)
            {
                (evicted, true)
            } else if watched.len() < self.inner.watch_limit.max_entries {
                watched.push(WatchedIntent {
                    venue: venue.clone(),
                    receipt,
                    last: None,
                    expires_at: Instant::now().checked_add(self.inner.watch_limit.grace),
                });
                (evicted, true)
            } else {
                (evicted, false)
            }
        };
        if evicted > 0 {
            warn!(evicted, "expired status watches evicted");
        }
        if !admitted {
            warn!(
                venue = %venue,
                "status watch set full - transitions for this receipt will not be reported",
            );
        }
        admitted
    }

    /// Number of receipts currently under status watch.
    pub fn watched_count(&self) -> usize {
        self.inner
            .watched
            .lock()
            .expect("watch list poisoned")
            .len()
    }

    /// Poll every watched receipt and return the transitions (statuses
    /// differing from the last reported; the first successful poll always
    /// reports). A terminal status is reported once, then dropped. An
    /// unreachable venue rides out against `grace` rather than refreshing it;
    /// an entry past `grace` is evicted unpolled.
    pub async fn poll_status_transitions(&self) -> Vec<IntentStatusUpdate> {
        // Snapshot so the std mutex is never held across the guest await.
        let (evicted, snapshot): (usize, Vec<(VenueId, Vec<u8>)>) = {
            let mut watched = self.inner.watched.lock().expect("watch list poisoned");
            let evicted = prune_expired(&mut watched);
            let snapshot = watched
                .iter()
                .map(|w| (w.venue.clone(), w.receipt.clone()))
                .collect();
            (evicted, snapshot)
        };
        if evicted > 0 {
            warn!(evicted, "expired status watches evicted");
        }
        let mut updates = Vec::new();
        for (venue, receipt) in snapshot {
            // Venue unreachable: a dead venue (poisoned/mid-restart) fails
            // to resolve. The watch is not refreshed but not dropped: it
            // rides out against `grace` while the sweep restarts the
            // adapter, and is pruned only if the outage outlasts it.
            let Ok(slot) = self.resolve(&venue) else {
                continue;
            };
            let polled = {
                let mut adapter = slot.lock().await;
                adapter.status(receipt.clone()).await
            };
            match polled {
                // Reachable: `record_polled_status` refreshes the deadline.
                Ok(status) => {
                    if let Some(update) = self.record_polled_status(&venue, &receipt, status) {
                        updates.push(update);
                    }
                }
                // Reachable adapter, errored poll (e.g. a venue-API outage):
                // like a resolve failure, the watch rides out against
                // `grace` rather than being refreshed or dropped.
                Err(err) => {
                    warn!(
                        venue = %venue,
                        error = %crate::bindings::venue_error_message(&err),
                        "status poll failed - retrying on the next cadence",
                    );
                }
            }
        }
        updates
    }

    /// Fold one polled status into the watch entry: `Some(update)` on a
    /// change. The venue answered, so every path refreshes the deadline (or
    /// drops the entry on a clean terminal); an encode failure costs the
    /// update, not the watch. `None` also covers an entry gone while the poll
    /// was in flight.
    fn record_polled_status(
        &self,
        venue: &VenueId,
        receipt: &[u8],
        status: IntentStatus,
    ) -> Option<IntentStatusUpdate> {
        let mut watched = self.inner.watched.lock().expect("watch list poisoned");
        let pos = watched
            .iter()
            .position(|w| w.venue == *venue && w.receipt == receipt)?;
        let grace = self.inner.watch_limit.grace;
        if watched[pos].last == Some(status) {
            // No transition, but the venue answered: refresh the deadline.
            watched[pos].expires_at = Instant::now().checked_add(grace);
            return None;
        }
        match status_body(status).encode() {
            Ok(body) => {
                if is_terminal(status) {
                    watched.remove(pos);
                } else {
                    watched[pos].last = Some(status);
                    watched[pos].expires_at = Instant::now().checked_add(grace);
                }
                Some(IntentStatusUpdate {
                    venue: venue.as_str().to_owned(),
                    receipt: receipt.to_vec(),
                    status: body,
                })
            }
            Err(err) => {
                // A host-side encode bug, not a silent venue. Refresh the
                // deadline (the venue is alive) and retry next cadence
                // rather than letting the watch expire.
                warn!(
                    venue = %venue,
                    error = %err,
                    "status body failed to encode - retrying on the next cadence",
                );
                watched[pos].expires_at = Instant::now().checked_add(grace);
                None
            }
        }
    }

    /// Report an intent's lifecycle state. No header, guard, or quota.
    pub async fn status(
        &self,
        venue: &VenueId,
        receipt: Vec<u8>,
    ) -> Result<IntentStatus, VenueError> {
        let slot = self.resolve(venue)?;
        let mut adapter = slot.lock().await;
        adapter.status(receipt).await
    }

    /// Ask the venue to withdraw an intent. No header, guard, or quota.
    pub async fn cancel(&self, venue: &VenueId, receipt: Vec<u8>) -> Result<(), VenueError> {
        let slot = self.resolve(venue)?;
        let mut adapter = slot.lock().await;
        adapter.cancel(receipt).await
    }

    /// Number of installed, routable adapters.
    pub fn venue_count(&self) -> usize {
        self.inner
            .adapters
            .lock()
            .expect("adapter map poisoned")
            .len()
    }
}

/// A quota window as whole milliseconds, saturating at `u64::MAX`.
fn window_ms(window: Duration) -> u64 {
    u64::try_from(window.as_millis()).unwrap_or(u64::MAX)
}

/// Drop watch entries whose eviction deadline has passed, returning how
/// many were evicted.
fn prune_expired(watched: &mut Vec<WatchedIntent>) -> usize {
    let now = Instant::now();
    let before = watched.len();
    watched.retain(|w| w.expires_at.is_none_or(|at| now < at));
    before - watched.len()
}

/// Drop charge timestamps that have aged out of the window.
fn prune(history: &mut VecDeque<Instant>, window: Duration) {
    let now = Instant::now();
    while let Some(&front) = history.front() {
        if now.duration_since(front) > window {
            history.pop_front();
        } else {
            break;
        }
    }
}

/// Assembles a [`VenueRegistry`]'s policy: guard, quota, watch bounds.
/// Adapters install afterwards at provider boot. Guard defaults to the unit
/// guard.
pub struct VenueRegistryBuilder {
    guard: Arc<dyn EgressGuard>,
    quota: SubmitQuota,
    watch_limit: WatchLimit,
}

impl VenueRegistryBuilder {
    /// Builder with the given quota, the unit guard, and the default watch
    /// limit.
    pub fn new(quota: SubmitQuota) -> Self {
        Self {
            guard: Arc::new(()),
            quota,
            watch_limit: WatchLimit::default(),
        }
    }

    /// Override the guard policy.
    pub fn with_guard(mut self, guard: Arc<dyn EgressGuard>) -> Self {
        self.guard = guard;
        self
    }

    /// Override the status-watch bounds.
    pub fn with_watch_limit(mut self, watch_limit: WatchLimit) -> Self {
        self.watch_limit = watch_limit;
        self
    }

    /// Freeze the builder into a shared registry.
    pub fn build(self) -> VenueRegistry {
        if self.quota.max_charges == 0 {
            // A zero budget would refuse every submission; saturate up to one
            // so a misconfigured quota still admits a single submission rather
            // than bricking every venue. Mirrors the poison-policy clamp.
            warn!("submission quota max_charges is 0; clamping to 1");
        }
        let quota = SubmitQuota::new(self.quota.max_charges.max(1), self.quota.window);
        if self.watch_limit.max_entries == 0 {
            // A zero cap would refuse every watch; saturate up to one so a
            // misconfigured bound still tracks a single receipt.
            warn!("watch limit max_entries is 0; clamping to 1");
        }
        // Clamp only `max_entries`. `WatchLimit::new` would re-derive
        // `grace` from `expiry` and silently discard an explicit one, which
        // made `WatchLimit::with_grace` a no-op through this builder.
        let watch_limit = WatchLimit::with_grace(
            self.watch_limit.max_entries.max(1),
            self.watch_limit.expiry,
            self.watch_limit.grace,
        );
        VenueRegistry {
            inner: Arc::new(VenueRegistryInner {
                adapters: Mutex::new(HashMap::new()),
                guard: self.guard,
                quota,
                watch_limit,
                ledger: Mutex::new(QuotaLedger::default()),
                watched: Mutex::new(Vec::new()),
            }),
        }
    }
}

/// Two installed adapters claimed the same venue id.
#[derive(Debug, thiserror::Error)]
#[error("venue id {venue} is claimed by more than one installed adapter")]
pub struct DuplicateVenue {
    /// The colliding venue id.
    pub venue: VenueId,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use videre_status_body::IntentStatus as Lifecycle;

    use crate::bindings::value_flow::{Asset, AssetAmount};
    use crate::bindings::{AuthScheme, IntentHeader, Settlement, UnsignedTx};
    use crate::policy::WATCH_GRACE_MAX;

    use super::*;

    /// The venue id every test installs its stub adapter under.
    fn cow() -> VenueId {
        "cow".parse().expect("valid venue id")
    }

    /// A venue id no test ever installs.
    fn unlisted() -> VenueId {
        "unlisted".parse().expect("valid venue id")
    }

    /// Decode an update's opaque status body.
    fn decoded(update: &IntentStatusUpdate) -> StatusBody {
        StatusBody::decode(&update.status).expect("status body decodes")
    }

    /// A body carrying a bare lifecycle state.
    fn plain(status: Lifecycle) -> StatusBody {
        StatusBody {
            status,
            proof: None,
            reason: None,
        }
    }

    /// Programmable adapter recording call counts, so routing is tested
    /// without a wasmtime store.
    #[derive(Default)]
    struct StubCalls {
        derive: AtomicUsize,
        quote: AtomicUsize,
        submit: AtomicUsize,
        status: AtomicUsize,
        cancel: AtomicUsize,
        /// Highest overlapping invocation count observed; proves the mutex
        /// serialises.
        max_concurrency: AtomicUsize,
        live: AtomicUsize,
    }

    struct StubAdapter {
        calls: Arc<StubCalls>,
        derive: Result<IntentHeader, VenueError>,
        submit: Result<SubmitOutcome, VenueError>,
        /// Accept each submission with its body as the receipt.
        echo_receipt: bool,
        /// Statuses served front-first by consecutive `status` calls;
        /// once drained, every further call reports `open`.
        status_script: VecDeque<Result<IntentStatus, VenueError>>,
    }

    impl StubAdapter {
        fn new(calls: Arc<StubCalls>) -> Self {
            Self {
                calls,
                derive: Ok(header()),
                submit: Ok(SubmitOutcome::Accepted(b"receipt".to_vec())),
                echo_receipt: false,
                status_script: VecDeque::new(),
            }
        }

        fn with_receipt_echo(mut self) -> Self {
            self.echo_receipt = true;
            self
        }

        fn with_derive(mut self, derive: Result<IntentHeader, VenueError>) -> Self {
            self.derive = derive;
            self
        }

        fn with_submit(mut self, submit: Result<SubmitOutcome, VenueError>) -> Self {
            self.submit = submit;
            self
        }

        fn with_status_script(
            mut self,
            script: impl IntoIterator<Item = Result<IntentStatus, VenueError>>,
        ) -> Self {
            self.status_script = script.into_iter().collect();
            self
        }

        async fn enter(&self) {
            let live = self.calls.live.fetch_add(1, Ordering::SeqCst) + 1;
            self.calls.max_concurrency.fetch_max(live, Ordering::SeqCst);
            // Yield inside the critical section so any missing serialisation
            // would let a second call observe `live == 2`.
            tokio::task::yield_now().await;
            self.calls.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl VenueInvoker for StubAdapter {
        fn derive_header<'a>(
            &'a mut self,
            _body: &'a [u8],
        ) -> BoxFuture<'a, Result<IntentHeader, VenueError>> {
            Box::pin(async move {
                self.calls.derive.fetch_add(1, Ordering::SeqCst);
                self.enter().await;
                self.derive.clone()
            })
        }

        fn quote<'a>(
            &'a mut self,
            _body: &'a [u8],
        ) -> BoxFuture<'a, Result<Quotation, VenueError>> {
            Box::pin(async move {
                self.calls.quote.fetch_add(1, Ordering::SeqCst);
                self.enter().await;
                Ok(quotation())
            })
        }

        fn submit<'a>(
            &'a mut self,
            body: &'a [u8],
        ) -> BoxFuture<'a, Result<SubmitOutcome, VenueError>> {
            Box::pin(async move {
                self.calls.submit.fetch_add(1, Ordering::SeqCst);
                self.enter().await;
                if self.echo_receipt {
                    return Ok(SubmitOutcome::Accepted(body.to_vec()));
                }
                self.submit.clone()
            })
        }

        fn status(&mut self, _receipt: Vec<u8>) -> BoxFuture<'_, Result<IntentStatus, VenueError>> {
            Box::pin(async move {
                self.calls.status.fetch_add(1, Ordering::SeqCst);
                self.status_script
                    .pop_front()
                    .unwrap_or(Ok(IntentStatus::Open))
            })
        }

        fn cancel(&mut self, _receipt: Vec<u8>) -> BoxFuture<'_, Result<(), VenueError>> {
            Box::pin(async move {
                self.calls.cancel.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    /// A guard that refuses every egress with a fixed reason.
    struct DenyGuard;
    impl EgressGuard for DenyGuard {
        fn check(&self, _ctx: &GuardContext<'_>) -> GuardVerdict {
            GuardVerdict::Deny("blocked by test policy".to_owned())
        }
    }

    fn quotation() -> Quotation {
        Quotation {
            gives: AssetAmount {
                asset: Asset::Native,
                amount: vec![1],
            },
            wants: AssetAmount {
                asset: Asset::Native,
                amount: Vec::new(),
            },
            fee: AssetAmount {
                asset: Asset::Native,
                amount: Vec::new(),
            },
            valid_until_ms: 1_700_000_000_000,
        }
    }

    fn header() -> IntentHeader {
        IntentHeader {
            gives: AssetAmount {
                asset: Asset::Native,
                amount: vec![1],
            },
            wants: AssetAmount {
                asset: Asset::Native,
                amount: Vec::new(),
            },
            settlement: Settlement { chain: 1 },
            authorisation: AuthScheme::Eip712,
        }
    }

    fn registry_with(
        quota: SubmitQuota,
        guard: Option<Arc<dyn EgressGuard>>,
        adapter: StubAdapter,
    ) -> VenueRegistry {
        let mut builder = VenueRegistryBuilder::new(quota);
        if let Some(guard) = guard {
            builder = builder.with_guard(guard);
        }
        let registry = builder.build();
        registry
            .register(cow(), Liveness::default(), BTreeSet::new(), adapter)
            .expect("install adapter");
        registry
    }

    #[tokio::test]
    async fn submit_round_trips_through_derive_guard_submit() {
        let calls = Arc::new(StubCalls::default());
        let registry = registry_with(
            SubmitQuota::default(),
            None,
            StubAdapter::new(calls.clone()),
        );

        let outcome = registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");

        assert!(matches!(outcome, SubmitOutcome::Accepted(r) if r == b"receipt"));
        assert_eq!(calls.derive.load(Ordering::SeqCst), 1);
        assert_eq!(calls.submit.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_venue_is_rejected_without_touching_an_adapter() {
        let calls = Arc::new(StubCalls::default());
        let registry = registry_with(
            SubmitQuota::default(),
            None,
            StubAdapter::new(calls.clone()),
        );

        let err = registry
            .submit("mod-a", &unlisted(), b"body".to_vec())
            .await
            .expect_err("unknown venue rejected");

        assert!(matches!(err, VenueError::UnknownVenue));
        assert_eq!(calls.derive.load(Ordering::SeqCst), 0);
        assert_eq!(calls.submit.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn guard_deny_is_advisory_and_does_not_block_submit() {
        let calls = Arc::new(StubCalls::default());
        let registry = registry_with(
            SubmitQuota::default(),
            Some(Arc::new(DenyGuard)),
            StubAdapter::new(calls.clone()),
        );

        let outcome = registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("advisory deny does not block");

        // The seam runs on the derived header but only logs: derive ran and
        // the submission still reached the adapter.
        assert!(matches!(outcome, SubmitOutcome::Accepted(r) if r == b"receipt"));
        assert_eq!(calls.derive.load(Ordering::SeqCst), 1);
        assert_eq!(calls.submit.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn repeated_guard_denies_exhaust_the_caller_quota() {
        let calls = Arc::new(StubCalls::default());
        let quota = SubmitQuota::new(2, Duration::from_secs(3600));
        let registry = registry_with(
            quota,
            Some(Arc::new(DenyGuard)),
            StubAdapter::new(calls.clone()),
        );

        // Each denied submit spends exactly one unit: the second is still
        // admitted, so a deny is never double-charged.
        assert!(
            registry
                .submit("mod-a", &cow(), b"b".to_vec())
                .await
                .is_ok()
        );
        assert!(
            registry
                .submit("mod-a", &cow(), b"b".to_vec())
                .await
                .is_ok()
        );
        // The deny loop is rate-limited at the gate, not free.
        assert!(matches!(
            registry.submit("mod-a", &cow(), b"b".to_vec()).await,
            Err(VenueError::RateLimited(_))
        ));
        assert_eq!(calls.derive.load(Ordering::SeqCst), 2);
        assert_eq!(calls.submit.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn quote_reaches_the_adapter_without_header_or_guard() {
        let calls = Arc::new(StubCalls::default());
        // A denying guard proves quotes skip the seam: no value moves.
        let registry = registry_with(
            SubmitQuota::default(),
            Some(Arc::new(DenyGuard)),
            StubAdapter::new(calls.clone()),
        );

        let quoted = registry
            .quote("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("quote succeeds");

        assert_eq!(quoted, quotation());
        assert_eq!(calls.quote.load(Ordering::SeqCst), 1);
        assert_eq!(calls.derive.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn quote_spends_the_caller_quota() {
        let calls = Arc::new(StubCalls::default());
        let quota = SubmitQuota::new(1, Duration::from_secs(3600));
        let registry = registry_with(quota, None, StubAdapter::new(calls.clone()));

        assert!(registry.quote("mod-a", &cow(), b"b".to_vec()).await.is_ok());
        // The quote spent the only unit: both a further quote and a
        // submit are stopped at the gate.
        assert!(matches!(
            registry.quote("mod-a", &cow(), b"b".to_vec()).await,
            Err(VenueError::RateLimited(_))
        ));
        assert!(matches!(
            registry.submit("mod-a", &cow(), b"b".to_vec()).await,
            Err(VenueError::RateLimited(_))
        ));
        assert_eq!(calls.quote.load(Ordering::SeqCst), 1);
        assert_eq!(calls.submit.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn quote_to_an_unknown_venue_is_rejected() {
        let calls = Arc::new(StubCalls::default());
        let registry = registry_with(
            SubmitQuota::default(),
            None,
            StubAdapter::new(calls.clone()),
        );

        assert!(matches!(
            registry.quote("mod-a", &unlisted(), b"b".to_vec()).await,
            Err(VenueError::UnknownVenue)
        ));
        assert_eq!(calls.quote.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn submission_quota_rate_limits_once_the_budget_is_spent() {
        let calls = Arc::new(StubCalls::default());
        let quota = SubmitQuota::new(2, Duration::from_secs(3600));
        let registry = registry_with(quota, None, StubAdapter::new(calls.clone()));

        assert!(
            registry
                .submit("mod-a", &cow(), b"b".to_vec())
                .await
                .is_ok()
        );
        assert!(
            registry
                .submit("mod-a", &cow(), b"b".to_vec())
                .await
                .is_ok()
        );
        let err = registry
            .submit("mod-a", &cow(), b"b".to_vec())
            .await
            .expect_err("third submit over quota");

        // Exhaustion is retryable once the window slides: rate-limited
        // carrying the window, never denied.
        assert!(matches!(
            err,
            VenueError::RateLimited(rl) if rl.retry_after_ms == Some(3_600_000)
        ));
        // The over-quota call is stopped at the gate, so the adapter saw only
        // the two admitted submits.
        assert_eq!(calls.submit.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn quota_is_per_caller() {
        let calls = Arc::new(StubCalls::default());
        let quota = SubmitQuota::new(1, Duration::from_secs(3600));
        let registry = registry_with(quota, None, StubAdapter::new(calls.clone()));

        assert!(
            registry
                .submit("mod-a", &cow(), b"b".to_vec())
                .await
                .is_ok()
        );
        assert!(
            registry
                .submit("mod-a", &cow(), b"b".to_vec())
                .await
                .is_err(),
            "mod-a is over its own budget"
        );
        // A different caller has its own budget.
        assert!(
            registry
                .submit("mod-b", &cow(), b"b".to_vec())
                .await
                .is_ok(),
            "mod-b has an independent budget"
        );
    }

    #[tokio::test]
    async fn decode_failures_are_charged_and_stop_re_invoking_the_adapter() {
        let calls = Arc::new(StubCalls::default());
        let quota = SubmitQuota::new(1, Duration::from_secs(3600));
        let adapter =
            StubAdapter::new(calls.clone()).with_derive(Err(VenueError::InvalidBody("bad".into())));
        let registry = registry_with(quota, None, adapter);

        // First garbage body: derive fails, the failure is charged.
        let first = registry.submit("mod-a", &cow(), b"junk".to_vec()).await;
        assert!(matches!(first, Err(VenueError::InvalidBody(_))));
        // Second: the charge from the decode failure exhausts the budget, so
        // the caller is stopped at the gate and the adapter is not re-invoked.
        let second = registry.submit("mod-a", &cow(), b"junk".to_vec()).await;
        assert!(matches!(second, Err(VenueError::RateLimited(_))));
        assert_eq!(
            calls.derive.load(Ordering::SeqCst),
            1,
            "adapter derive-header was invoked exactly once",
        );
    }

    #[tokio::test]
    async fn non_decode_venue_errors_are_not_charged() {
        let calls = Arc::new(StubCalls::default());
        let quota = SubmitQuota::new(1, Duration::from_secs(3600));
        let adapter = StubAdapter::new(calls.clone())
            .with_derive(Err(VenueError::Unavailable("rpc down".into())));
        let registry = registry_with(quota, None, adapter);

        assert!(matches!(
            registry.submit("mod-a", &cow(), b"b".to_vec()).await,
            Err(VenueError::Unavailable(_))
        ));
        // A venue-side failure did not spend the caller's budget: it may try
        // again, so derive is reached a second time.
        assert!(matches!(
            registry.submit("mod-a", &cow(), b"b".to_vec()).await,
            Err(VenueError::Unavailable(_))
        ));
        assert_eq!(calls.derive.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn status_and_cancel_pass_through_without_quota() {
        let calls = Arc::new(StubCalls::default());
        // A spent budget must not block reads: status and cancel are not
        // submissions.
        let quota = SubmitQuota::new(1, Duration::from_secs(3600));
        let registry = registry_with(quota, None, StubAdapter::new(calls.clone()));

        assert!(matches!(
            registry.status(&cow(), b"r".to_vec()).await,
            Ok(IntentStatus::Open)
        ));
        assert!(registry.cancel(&cow(), b"r".to_vec()).await.is_ok());
        assert_eq!(calls.status.load(Ordering::SeqCst), 1);
        assert_eq!(calls.cancel.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_calls_to_one_adapter_are_serialised() {
        let calls = Arc::new(StubCalls::default());
        let quota = SubmitQuota::new(1000, Duration::from_secs(3600));
        let registry = registry_with(quota, None, StubAdapter::new(calls.clone()));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let registry = registry.clone();
            handles.push(tokio::spawn(async move {
                let _ = registry.submit("mod-a", &cow(), b"b".to_vec()).await;
            }));
        }
        for h in handles {
            h.await.expect("task joins");
        }
        // The adapter mutex is held across the guest await, so no two calls
        // ever overlapped inside the adapter.
        assert_eq!(calls.max_concurrency.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn blank_venue_id_is_rejected_at_construction() {
        assert!("".parse::<VenueId>().is_err());
        assert!("  ".parse::<VenueId>().is_err());
        assert!(VenueId::new("\t\n").is_err());
        assert_eq!("cow".parse::<VenueId>().expect("parses").as_str(), "cow");
        // `:` stays legal for the future chain-qualified shape.
        assert_eq!(
            "cow:11155111".parse::<VenueId>().expect("parses").as_str(),
            "cow:11155111"
        );
        assert_eq!(VenueId::new("cow").expect("constructs").to_string(), "cow");
        // The derived accessor is `AsRef<str>`, matching the SDK id.
        assert_eq!(AsRef::<str>::as_ref(&cow()), "cow");
    }

    #[test]
    fn padded_venue_id_is_rejected_at_construction() {
        assert!("cow ".parse::<VenueId>().is_err());
        assert!(" cow".parse::<VenueId>().is_err());
        assert!(VenueId::new("cow\n").is_err());
        assert!(VenueId::new("\tcow").is_err());
        assert!(VenueId::new("cow\u{a0}").is_err());
        assert!(VenueId::new("cow venue").is_ok());
    }

    #[test]
    fn duplicate_venue_id_is_rejected() {
        let registry = VenueRegistryBuilder::new(SubmitQuota::default()).build();
        let a = Arc::new(StubCalls::default());
        let b = Arc::new(StubCalls::default());
        registry
            .register(
                cow(),
                Liveness::default(),
                BTreeSet::new(),
                StubAdapter::new(a),
            )
            .expect("first install");
        let err = registry
            .register(
                cow(),
                Liveness::default(),
                BTreeSet::new(),
                StubAdapter::new(b),
            )
            .expect_err("second install collides");
        assert_eq!(err.venue, cow());
    }

    /// The handshake authority: every registered venue's declared set,
    /// including the empty set of a venue that opted out.
    #[test]
    fn body_versions_reports_each_registered_venue_set() {
        let registry = VenueRegistryBuilder::new(SubmitQuota::default()).build();
        registry
            .register(
                cow(),
                Liveness::default(),
                BTreeSet::from([1, 2]),
                StubAdapter::new(Arc::new(StubCalls::default())),
            )
            .expect("register the declaring venue");
        registry
            .register(
                VenueId::new("uni").expect("valid venue id"),
                Liveness::default(),
                BTreeSet::new(),
                StubAdapter::new(Arc::new(StubCalls::default())),
            )
            .expect("register the opted-out venue");

        let versions = registry.body_versions();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[&cow()], BTreeSet::from([1, 2]));
        assert!(versions[&VenueId::new("uni").expect("valid venue id")].is_empty());
    }

    #[tokio::test]
    async fn dead_venue_is_unavailable_not_unknown() {
        let calls = Arc::new(StubCalls::default());
        let liveness = Liveness::default();
        let registry = VenueRegistryBuilder::new(SubmitQuota::default()).build();
        registry
            .register(
                cow(),
                liveness.clone(),
                BTreeSet::new(),
                StubAdapter::new(calls.clone()),
            )
            .expect("install adapter");
        liveness.mark_dead();

        // Temporarily dead resolves distinctly from never installed, and
        // the dead adapter's slot is never entered.
        assert!(matches!(
            registry.submit("mod-a", &cow(), b"b".to_vec()).await,
            Err(VenueError::Unavailable(_))
        ));
        assert!(matches!(
            registry.submit("mod-a", &unlisted(), b"b".to_vec()).await,
            Err(VenueError::UnknownVenue)
        ));
        assert_eq!(calls.derive.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_dead_incumbent_is_replaced_on_reinstall() {
        let registry = VenueRegistryBuilder::new(SubmitQuota::default()).build();
        let liveness = Liveness::default();
        registry
            .register(
                cow(),
                liveness.clone(),
                BTreeSet::new(),
                StubAdapter::new(Arc::new(StubCalls::default())),
            )
            .expect("first install");
        liveness.mark_dead();
        registry
            .register(
                cow(),
                Liveness::default(),
                BTreeSet::new(),
                StubAdapter::new(Arc::new(StubCalls::default())),
            )
            .expect("a restart replaces the dead incumbent");
        assert_eq!(registry.venue_count(), 1);
    }

    #[test]
    fn zero_quota_saturates_to_one() {
        let registry =
            VenueRegistryBuilder::new(SubmitQuota::new(0, Duration::from_secs(60))).build();
        assert_eq!(registry.inner.quota.max_charges, 1);
    }

    #[test]
    fn zero_watch_cap_saturates_to_one() {
        let registry = VenueRegistryBuilder::new(SubmitQuota::default())
            .with_watch_limit(WatchLimit::new(0, Duration::from_secs(60)))
            .build();
        assert_eq!(registry.inner.watch_limit.max_entries, 1);
    }

    // ── status watch + polling ────────────────────────────────────────

    #[tokio::test]
    async fn accepted_submission_goes_under_status_watch() {
        let calls = Arc::new(StubCalls::default());
        let registry = registry_with(SubmitQuota::default(), None, StubAdapter::new(calls));

        assert_eq!(registry.watched_count(), 0);
        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");
        assert_eq!(registry.watched_count(), 1);

        // Re-submitting the same receipt does not double-watch it.
        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");
        assert_eq!(registry.watched_count(), 1);
    }

    #[tokio::test]
    async fn observe_watches_an_externally_obtained_receipt() {
        let calls = Arc::new(StubCalls::default());
        let adapter =
            StubAdapter::new(calls.clone()).with_status_script([Ok(IntentStatus::Fulfilled)]);
        let registry = registry_with(SubmitQuota::default(), None, adapter);

        registry
            .observe(&cow(), b"onchain".to_vec())
            .expect("observe succeeds");
        // Re-observing keeps the existing entry.
        registry
            .observe(&cow(), b"onchain".to_vec())
            .expect("observe is idempotent");
        assert_eq!(registry.watched_count(), 1);
        // No adapter work happened at observe time.
        assert_eq!(calls.status.load(Ordering::SeqCst), 0);
        assert_eq!(calls.submit.load(Ordering::SeqCst), 0);

        // The watch polls like a submitted one: the terminal status
        // reports once and prunes the entry.
        let updates = registry.poll_status_transitions().await;
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].receipt, b"onchain");
        assert_eq!(decoded(&updates[0]), plain(Lifecycle::Fulfilled));
        assert_eq!(registry.watched_count(), 0);
    }

    #[test]
    fn observe_rejects_an_unknown_venue() {
        let registry = registry_with(
            SubmitQuota::default(),
            None,
            StubAdapter::new(Arc::new(StubCalls::default())),
        );
        assert!(matches!(
            registry.observe(&unlisted(), b"r".to_vec()),
            Err(VenueError::UnknownVenue)
        ));
        assert_eq!(registry.watched_count(), 0);
    }

    #[test]
    fn observe_of_a_dead_venue_is_unavailable() {
        let liveness = Liveness::default();
        let registry = VenueRegistryBuilder::new(SubmitQuota::default()).build();
        registry
            .register(
                cow(),
                liveness.clone(),
                BTreeSet::new(),
                StubAdapter::new(Arc::new(StubCalls::default())),
            )
            .expect("install adapter");
        liveness.mark_dead();
        assert!(matches!(
            registry.observe(&cow(), b"r".to_vec()),
            Err(VenueError::Unavailable(_))
        ));
        assert_eq!(registry.watched_count(), 0);
    }

    #[test]
    fn observe_at_the_watch_cap_is_refused_typedly() {
        let limit = WatchLimit::new(1, Duration::from_secs(3600));
        let registry =
            watch_bounded_registry(limit, StubAdapter::new(Arc::new(StubCalls::default())));

        registry.observe(&cow(), b"a".to_vec()).expect("admitted");
        let err = registry
            .observe(&cow(), b"b".to_vec())
            .expect_err("overflow refused");
        assert!(matches!(err, VenueError::Unavailable(_)));
        // The live watch is kept; the overflow was refused.
        assert_eq!(registry.watched_count(), 1);
    }

    #[tokio::test]
    async fn requires_signing_outcome_is_not_watched() {
        let calls = Arc::new(StubCalls::default());
        let adapter =
            StubAdapter::new(calls).with_submit(Ok(SubmitOutcome::RequiresSigning(UnsignedTx {
                chain: 1,
                to: vec![0u8; 20],
                value: Vec::new(),
                data: Vec::new(),
            })));
        let registry = registry_with(SubmitQuota::default(), None, adapter);

        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");
        // No receipt exists yet, so there is nothing to poll.
        assert_eq!(registry.watched_count(), 0);
        assert!(registry.poll_status_transitions().await.is_empty());
    }

    #[tokio::test]
    async fn poll_reports_the_first_status_then_dedupes_repeats() {
        let calls = Arc::new(StubCalls::default());
        let registry = registry_with(
            SubmitQuota::default(),
            None,
            StubAdapter::new(calls.clone()),
        );
        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");

        // First poll: `last` is unset, so the current status reports.
        let first = registry.poll_status_transitions().await;
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].venue, "cow");
        assert_eq!(first[0].receipt, b"receipt");
        assert_eq!(decoded(&first[0]), plain(Lifecycle::Open));

        // Second poll: same status, nothing to report.
        assert!(registry.poll_status_transitions().await.is_empty());
        assert_eq!(calls.status.load(Ordering::SeqCst), 2);
        assert_eq!(registry.watched_count(), 1, "open is not terminal");
    }

    #[tokio::test]
    async fn poll_reports_each_transition_and_prunes_on_terminal() {
        let calls = Arc::new(StubCalls::default());
        let adapter = StubAdapter::new(calls).with_status_script([
            Ok(IntentStatus::Pending),
            Ok(IntentStatus::Pending),
            Ok(IntentStatus::Open),
            Ok(IntentStatus::Fulfilled),
        ]);
        let registry = registry_with(SubmitQuota::default(), None, adapter);
        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");

        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.extend(registry.poll_status_transitions().await);
        }
        let statuses: Vec<StatusBody> = seen.iter().map(decoded).collect();
        assert_eq!(
            statuses,
            vec![
                plain(Lifecycle::Pending),
                plain(Lifecycle::Open),
                plain(Lifecycle::Fulfilled),
            ],
            "the repeated pending is deduplicated; each transition reports once",
        );
        assert_eq!(registry.watched_count(), 0, "fulfilled prunes the watch");
        // A further poll has nothing left to ask the adapter about.
        assert!(registry.poll_status_transitions().await.is_empty());
    }

    #[tokio::test]
    async fn poll_failure_keeps_the_watch_for_the_next_cadence() {
        let calls = Arc::new(StubCalls::default());
        let adapter = StubAdapter::new(calls)
            .with_status_script([Err(VenueError::Unavailable("venue down".into()))]);
        let registry = registry_with(SubmitQuota::default(), None, adapter);
        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");

        assert!(registry.poll_status_transitions().await.is_empty());
        assert_eq!(
            registry.watched_count(),
            1,
            "transient failure keeps the entry"
        );

        // The venue recovered: the next poll reports the current status.
        let updates = registry.poll_status_transitions().await;
        assert_eq!(updates.len(), 1);
        assert_eq!(decoded(&updates[0]), plain(Lifecycle::Open));
    }

    /// A registry with the given watch bounds and one echo-receipt-capable
    /// stub adapter under `cow`.
    fn watch_bounded_registry(watch_limit: WatchLimit, adapter: StubAdapter) -> VenueRegistry {
        let registry = VenueRegistryBuilder::new(SubmitQuota::default())
            .with_watch_limit(watch_limit)
            .build();
        registry
            .register(cow(), Liveness::default(), BTreeSet::new(), adapter)
            .expect("install adapter");
        registry
    }

    #[tokio::test]
    async fn watch_cap_refuses_the_overflow_and_never_drops_live_watches() {
        let calls = Arc::new(StubCalls::default());
        let adapter = StubAdapter::new(calls)
            .with_receipt_echo()
            .with_status_script([Ok(IntentStatus::Pending), Ok(IntentStatus::Pending)]);
        let limit = WatchLimit::new(2, Duration::from_secs(3600));
        let registry = watch_bounded_registry(limit, adapter);

        for body in [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()] {
            registry
                .submit("mod-a", &cow(), body)
                .await
                .expect("submit succeeds");
        }
        assert_eq!(registry.watched_count(), 2, "the cap bounds the set");

        // The live pending watches kept their tracking; only the overflow
        // watch was refused.
        let updates = registry.poll_status_transitions().await;
        let receipts: Vec<&[u8]> = updates.iter().map(|u| u.receipt.as_slice()).collect();
        assert_eq!(receipts, vec![b"a".as_slice(), b"b".as_slice()]);
        assert!(
            updates
                .iter()
                .all(|u| decoded(u) == plain(Lifecycle::Pending))
        );
    }

    #[tokio::test]
    async fn pending_polls_keep_a_live_watch_across_expiry_windows() {
        let calls = Arc::new(StubCalls::default());
        let adapter = StubAdapter::new(calls).with_status_script([
            Ok(IntentStatus::Pending),
            Ok(IntentStatus::Pending),
            Ok(IntentStatus::Fulfilled),
        ]);
        let expiry = Duration::from_secs(1);
        let registry = watch_bounded_registry(WatchLimit::new(8, expiry), adapter);

        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");
        let deadline_at = |registry: &VenueRegistry| {
            let watched = registry.inner.watched.lock().expect("watch list poisoned");
            watched[0].expires_at
        };
        let inserted = deadline_at(&registry);

        // Two pending polls, each pushing the deadline a full window out.
        let mut reported = Vec::new();
        for _ in 0..2 {
            reported.extend(registry.poll_status_transitions().await);
            assert_eq!(
                registry.watched_count(),
                1,
                "a reporting venue stays watched"
            );
            assert!(
                deadline_at(&registry) > inserted,
                "the poll refreshed the deadline"
            );
            tokio::time::sleep(expiry * 7 / 10).await;
        }

        // Well past the insert-time window, the terminal transition still
        // reports and prunes the watch.
        reported.extend(registry.poll_status_transitions().await);
        let statuses: Vec<StatusBody> = reported.iter().map(decoded).collect();
        assert_eq!(
            statuses,
            vec![plain(Lifecycle::Pending), plain(Lifecycle::Fulfilled)],
        );
        assert_eq!(registry.watched_count(), 0);
    }

    #[tokio::test]
    async fn expired_watches_are_evicted_unpolled() {
        let calls = Arc::new(StubCalls::default());
        let limit = WatchLimit::new(8, Duration::ZERO);
        let registry = watch_bounded_registry(limit, StubAdapter::new(calls.clone()));

        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");
        assert_eq!(registry.watched_count(), 1);

        // The entry expired before the cadence: evicted without a venue call.
        assert!(registry.poll_status_transitions().await.is_empty());
        assert_eq!(registry.watched_count(), 0);
        assert_eq!(calls.status.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn expiry_frees_room_at_the_cap() {
        let calls = Arc::new(StubCalls::default());
        let limit = WatchLimit::new(1, Duration::ZERO);
        let registry = watch_bounded_registry(limit, StubAdapter::new(calls).with_receipt_echo());

        registry
            .submit("mod-a", &cow(), b"a".to_vec())
            .await
            .expect("submit succeeds");
        registry
            .submit("mod-a", &cow(), b"b".to_vec())
            .await
            .expect("submit succeeds");

        // The expired first watch was evicted at insert, admitting the second.
        let watched = registry.inner.watched.lock().expect("watch list poisoned");
        assert_eq!(watched.len(), 1);
        assert_eq!(watched[0].receipt, b"b");
    }

    #[test]
    fn grace_derives_from_expiry_and_caps_at_a_day() {
        // A short base window: grace is the fixed multiple of it.
        assert_eq!(
            WatchLimit::new(8, Duration::from_secs(900)).grace,
            Duration::from_secs(1800),
        );
        // A long base window: grace saturates at the ceiling.
        assert_eq!(
            WatchLimit::new(8, Duration::from_secs(86_400)).grace,
            WATCH_GRACE_MAX,
        );
        // An explicit grace overrides the derivation.
        assert_eq!(
            WatchLimit::with_grace(8, Duration::from_secs(900), Duration::from_secs(60)).grace,
            Duration::from_secs(60),
        );
    }

    #[test]
    fn the_builder_keeps_an_explicit_grace_while_clamping_the_entry_cap() {
        // Regression: `build` used to rebuild the limit with
        // `WatchLimit::new`, which re-derives `grace` from `expiry`, so an
        // explicit grace never reached the registry.
        let registry = VenueRegistryBuilder::new(SubmitQuota::default())
            .with_watch_limit(WatchLimit::with_grace(
                0,
                Duration::from_secs(900),
                Duration::from_secs(60),
            ))
            .build();
        let limit = registry.inner.watch_limit;
        assert_eq!(limit.grace, Duration::from_secs(60), "explicit grace kept");
        assert_eq!(limit.max_entries, 1, "a zero entry cap still clamps to 1");
        assert_eq!(limit.expiry, Duration::from_secs(900));
    }

    /// Read the sole watch entry's give-up deadline.
    fn sole_deadline(registry: &VenueRegistry) -> Option<Instant> {
        registry
            .inner
            .watched
            .lock()
            .expect("watch list poisoned")
            .first()
            .and_then(|e| e.expires_at)
    }

    #[tokio::test]
    async fn a_dead_venue_rides_out_without_refreshing_the_deadline() {
        let calls = Arc::new(StubCalls::default());
        let registry = VenueRegistryBuilder::new(SubmitQuota::default())
            .with_watch_limit(WatchLimit::new(8, Duration::from_secs(3600)))
            .build();
        let liveness = Liveness::default();
        registry
            .register(
                cow(),
                liveness.clone(),
                BTreeSet::new(),
                StubAdapter::new(calls),
            )
            .expect("install adapter");
        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");
        let inserted = sole_deadline(&registry);

        // Venue goes dead: resolve fails, so the poll neither reports nor
        // refreshes - the watch rides out against grace instead of being
        // dropped or having its deadline pushed out.
        liveness.mark_dead();
        assert!(registry.poll_status_transitions().await.is_empty());
        assert_eq!(
            registry.watched_count(),
            1,
            "a dead venue rides out rather than being dropped",
        );
        assert_eq!(
            sole_deadline(&registry),
            inserted,
            "a resolve failure does not refresh the deadline",
        );
    }

    #[tokio::test]
    async fn an_errored_poll_rides_out_without_refreshing_the_deadline() {
        let calls = Arc::new(StubCalls::default());
        let adapter = StubAdapter::new(calls)
            .with_status_script([Err(VenueError::Unavailable("api down".into()))]);
        let registry =
            watch_bounded_registry(WatchLimit::new(8, Duration::from_secs(3600)), adapter);
        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");
        let inserted = sole_deadline(&registry);

        // A reachable adapter whose poll errors (a venue-API outage) rides
        // out exactly like a resolve failure: kept, deadline untouched.
        assert!(registry.poll_status_transitions().await.is_empty());
        assert_eq!(
            registry.watched_count(),
            1,
            "an errored poll keeps the watch",
        );
        assert_eq!(
            sole_deadline(&registry),
            inserted,
            "an errored poll does not refresh the deadline",
        );
    }

    #[test]
    fn every_lifecycle_state_lowers_onto_the_status_body() {
        for (wire, lowered) in [
            (IntentStatus::Pending, Lifecycle::Pending),
            (IntentStatus::Open, Lifecycle::Open),
            (IntentStatus::Fulfilled, Lifecycle::Fulfilled),
            (IntentStatus::Cancelled, Lifecycle::Cancelled),
            (IntentStatus::Expired, Lifecycle::Expired),
        ] {
            assert_eq!(status_body(wire), plain(lowered));
        }
    }
}
