//! Coverage for the videre platform over the generic runtime seam: the
//! native venue round trip through [`VenueRegistry`], the body-version
//! handshake the extension gates a keeper on, the routing policy, the
//! status-poll source, and the client-import linker hook.
//!
//! A venue is a native Rust [`VenueInvoker`], so none of this needs a guest
//! component: the platform is driven through its own public seam.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use futures::future::BoxFuture;
use nexum_runtime::bindings::nexum::host::types::Trigger;
use nexum_runtime::config::EngineConfig;
use nexum_runtime::extension::{Extension, HostState, SourceContext};
use nexum_runtime::manifest::ExtensionSections;
use nexum_runtime::nexum_tasks::{TaskManager, TaskSet};
use nexum_runtime::test_utils::{MockTypes, test_wasmtime_engine};
use videre_host::bindings::{
    AuthScheme, IntentHeader, IntentStatus, Quotation, Settlement, SubmitOutcome, VenueError,
    value_flow,
};
use videre_host::{
    Liveness, SubmitQuota, VenueId, VenueInvoker, VenueRegistry, VenueRegistryBuilder, Videre,
    WatchLimit, platform,
};
use videre_status_body::{INTENT_STATUS_KIND, IntentStatusUpdate, StatusBody};
use wasmtime::component::Linker;

/// The venue id the tests register their adapter under.
fn cow() -> VenueId {
    VenueId::new("cow").expect("valid venue id")
}

fn native(bytes: Vec<u8>) -> value_flow::AssetAmount {
    value_flow::AssetAmount {
        asset: value_flow::Asset::Native,
        amount: bytes,
    }
}

fn header() -> IntentHeader {
    IntentHeader {
        gives: native(vec![1]),
        wants: native(Vec::new()),
        settlement: Settlement { chain: 1 },
        authorisation: AuthScheme::Eip712,
    }
}

fn quotation() -> Quotation {
    Quotation {
        gives: native(vec![1]),
        wants: native(Vec::new()),
        fee: native(Vec::new()),
        valid_until_ms: 1_700_000_000_000,
    }
}

/// A native venue: it accepts every submission with the body as the
/// receipt and serves scripted statuses front-first. Once the script
/// drains, every further poll reports `open`. `cancelled` is shared with
/// the test, which the registry hands no handle on the venue back.
struct ScriptedVenue {
    statuses: VecDeque<IntentStatus>,
    cancelled: Arc<AtomicBool>,
}

impl ScriptedVenue {
    fn new(statuses: impl IntoIterator<Item = IntentStatus>) -> Self {
        Self {
            statuses: statuses.into_iter().collect(),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The flag this venue sets when its `cancel` runs.
    fn cancelled(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }
}

impl VenueInvoker for ScriptedVenue {
    fn derive_header<'a>(
        &'a mut self,
        _body: &'a [u8],
    ) -> BoxFuture<'a, Result<IntentHeader, VenueError>> {
        Box::pin(async move { Ok(header()) })
    }

    fn quote<'a>(&'a mut self, _body: &'a [u8]) -> BoxFuture<'a, Result<Quotation, VenueError>> {
        Box::pin(async move { Ok(quotation()) })
    }

    fn submit<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<SubmitOutcome, VenueError>> {
        Box::pin(async move { Ok(SubmitOutcome::Accepted(body.to_vec())) })
    }

    fn status(&mut self, _receipt: Vec<u8>) -> BoxFuture<'_, Result<IntentStatus, VenueError>> {
        Box::pin(async move { Ok(self.statuses.pop_front().unwrap_or(IntentStatus::Open)) })
    }

    fn cancel(&mut self, _receipt: Vec<u8>) -> BoxFuture<'_, Result<(), VenueError>> {
        Box::pin(async move {
            self.cancelled.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}

/// A registry with one scripted venue registered under `cow`, declaring
/// `versions` as the body-schema set it decodes.
fn registry_with(venue: ScriptedVenue, versions: impl IntoIterator<Item = u32>) -> VenueRegistry {
    let registry = VenueRegistryBuilder::new(SubmitQuota::default()).build();
    registry
        .register(
            cow(),
            Liveness::new(),
            versions.into_iter().collect(),
            venue,
        )
        .expect("register the scripted venue");
    registry
}

/// One `[venue]` manifest section, as the loader hands it to the extension.
fn sections(toml: &str) -> ExtensionSections {
    let table: toml::Table = toml.parse().expect("parse the [venue] section");
    table.into_iter().collect()
}

/// Every verb of the keeper-facing seam reaches the registered venue, and
/// an accepted receipt goes under status watch.
#[tokio::test]
async fn every_verb_reaches_the_registered_native_venue() {
    // A scripted `pending` so the status leg pins the venue's own answer
    // rather than the drained-script fallback.
    let venue = ScriptedVenue::new([IntentStatus::Pending]);
    let cancelled = venue.cancelled();
    let registry = registry_with(venue, [1]);

    let quoted = registry
        .quote("mod-a", &cow(), b"body".to_vec())
        .await
        .expect("quote succeeds");
    assert_eq!(quoted, quotation());

    let outcome = registry
        .submit("mod-a", &cow(), b"body".to_vec())
        .await
        .expect("submit succeeds");
    assert!(matches!(outcome, SubmitOutcome::Accepted(r) if r == b"body"));
    assert_eq!(registry.watched_count(), 1, "the receipt is watched");

    // An externally obtained receipt joins the watch without a submission.
    registry
        .observe(&cow(), b"onchain".to_vec())
        .expect("observe succeeds");
    assert_eq!(registry.watched_count(), 2);

    assert!(matches!(
        registry.status(&cow(), b"body".to_vec()).await,
        Ok(IntentStatus::Pending)
    ));
    registry
        .cancel(&cow(), b"body".to_vec())
        .await
        .expect("cancel succeeds");
    assert!(
        cancelled.load(Ordering::SeqCst),
        "the venue's own cancel ran, not just the registry's dispatch",
    );
}

/// An id no venue registered under resolves to `unknown-venue`, distinct
/// from a registered venue that is dead.
#[tokio::test]
async fn an_unregistered_venue_is_unknown_and_a_dead_one_unavailable() {
    let liveness = Liveness::new();
    let registry = VenueRegistryBuilder::new(SubmitQuota::default()).build();
    registry
        .register(
            cow(),
            liveness.clone(),
            BTreeSet::new(),
            ScriptedVenue::new([]),
        )
        .expect("register the scripted venue");

    assert!(matches!(
        registry
            .submit(
                "mod-a",
                &VenueId::new("unlisted").expect("valid venue id"),
                b"b".to_vec()
            )
            .await,
        Err(VenueError::UnknownVenue)
    ));

    liveness.mark_dead();
    assert!(matches!(
        registry.submit("mod-a", &cow(), b"b".to_vec()).await,
        Err(VenueError::Unavailable(_))
    ));
}

/// The quota the composition root builds the registry with rate-limits a
/// caller once its budget is spent, per caller.
#[tokio::test]
async fn the_submit_quota_rate_limits_the_spending_caller_only() {
    let registry =
        VenueRegistryBuilder::new(SubmitQuota::new(1, Duration::from_secs(3600))).build();
    registry
        .register(
            cow(),
            Liveness::new(),
            BTreeSet::new(),
            ScriptedVenue::new([]),
        )
        .expect("register the scripted venue");

    assert!(
        registry
            .submit("mod-a", &cow(), b"b".to_vec())
            .await
            .is_ok()
    );
    assert!(matches!(
        registry.submit("mod-a", &cow(), b"b".to_vec()).await,
        Err(VenueError::RateLimited(rl)) if rl.retry_after_ms == Some(3_600_000)
    ));
    assert!(
        registry
            .submit("mod-b", &cow(), b"b".to_vec())
            .await
            .is_ok(),
        "a second caller has its own budget",
    );
}

/// The watch cap bounds the status-watch set: the overflow is refused and
/// the live watches are kept.
#[tokio::test]
async fn the_watch_limit_bounds_the_status_watch_set() {
    let registry = VenueRegistryBuilder::new(SubmitQuota::default())
        .with_watch_limit(WatchLimit::new(1, Duration::from_secs(3600)))
        .build();
    registry
        .register(
            cow(),
            Liveness::new(),
            BTreeSet::new(),
            ScriptedVenue::new([]),
        )
        .expect("register the scripted venue");

    registry
        .submit("mod-a", &cow(), b"first".to_vec())
        .await
        .expect("submit succeeds");
    registry
        .submit("mod-a", &cow(), b"second".to_vec())
        .await
        .expect("the submission still lands; only its watch is refused");
    assert_eq!(registry.watched_count(), 1, "the cap bounds the set");

    let err = registry
        .observe(&cow(), b"third".to_vec())
        .expect_err("an observe past the cap is refused typedly");
    assert!(matches!(err, VenueError::Unavailable(_)));
}

/// A keeper boots only when every registered venue decodes the one body
/// version it encodes; the refusal names the version and the decoded set.
#[test]
fn the_body_version_handshake_gates_a_keeper_on_the_registered_venues() {
    let videre = Videre::from_registry(registry_with(ScriptedVenue::new([]), [1, 2]));
    let keeper = sections("[venue]\nbody_version = 2");
    Extension::<MockTypes>::admit_worker(&videre, "keeper", &keeper).expect("admitted");

    let stale = sections("[venue]\nbody_version = 3");
    let err = Extension::<MockTypes>::admit_worker(&videre, "keeper", &stale)
        .expect_err("no registered venue decodes version 3");
    // `ExtensionError` is a thiserror enum, so its Display already
    // interpolates the refusal it wraps; no alternate flag walks a chain.
    let chain = err.to_string();
    assert!(chain.contains("body version 3"), "{chain}");
    assert!(chain.contains("cow decodes {1, 2}"), "{chain}");
}

/// A venue registering an empty version set has opted out of the
/// handshake: it neither satisfies a declaring keeper nor refuses one on
/// its own.
#[test]
fn a_venue_that_declares_no_versions_never_satisfies_a_declaring_keeper() {
    let videre = Videre::from_registry(registry_with(ScriptedVenue::new([]), []));
    let keeper = sections("[venue]\nbody_version = 1");
    let err = Extension::<MockTypes>::admit_worker(&videre, "keeper", &keeper)
        .expect_err("an opted-out venue cannot satisfy the keeper");
    assert!(
        err.to_string().contains("no registered venue declares"),
        "{err}",
    );

    // A keeper declaring nothing is admitted whatever is registered.
    Extension::<MockTypes>::admit_worker(&videre, "keeper", &ExtensionSections::new())
        .expect("an undeclared keeper opts out of the handshake");
}

/// Open the platform's sources over a fresh task set, demanding `kinds`.
fn open_sources(videre: &Videre, kinds: &BTreeSet<String>) -> usize {
    let config = EngineConfig::default();
    let manager = TaskManager::new();
    let executor = manager.executor();
    let mut tasks = TaskSet::new();
    let mut sources = SourceContext::new(&config, kinds, &executor, &mut tasks);
    Extension::<MockTypes>::open_sources(videre, &mut sources)
        .expect("open the platform sources")
        .len()
}

/// The demanded kind: one owned set, as the source plan hands it over.
fn demanding_intent_status() -> BTreeSet<String> {
    std::iter::once(INTENT_STATUS_KIND.to_owned()).collect()
}

/// The polled transition rides the extension trigger: the delivery carries
/// the venue attribute for routing and the borsh envelope as its payload.
#[tokio::test]
async fn the_status_poll_source_delivers_a_polled_transition() {
    let registry = VenueRegistryBuilder::new(SubmitQuota::default()).build();
    // The one call site of the `test-utils` seam, which is `register` with
    // no declared version set. The source does not read the handshake, so
    // this test doubles as the seam's only coverage.
    registry
        .install_for_test(
            cow(),
            Liveness::new(),
            ScriptedVenue::new([IntentStatus::Fulfilled]),
        )
        .expect("install the scripted venue");
    registry
        .submit("mod-a", &cow(), b"receipt".to_vec())
        .await
        .expect("submit succeeds");

    let videre =
        Videre::from_registry(registry.clone()).with_status_poll_interval(Duration::from_millis(5));

    let config = EngineConfig::default();
    let manager = TaskManager::new();
    let executor = manager.executor();
    let mut tasks = TaskSet::new();
    let kinds = demanding_intent_status();
    let mut streams = {
        let mut sources = SourceContext::new(&config, &kinds, &executor, &mut tasks);
        Extension::<MockTypes>::open_sources(&videre, &mut sources).expect("open the source")
    };
    assert_eq!(streams.len(), 1, "one status-poll source opened");

    let delivery = tokio::time::timeout(Duration::from_secs(5), streams[0].next())
        .await
        .expect("the poll task reports within the timeout")
        .expect("the source yields a delivery");

    assert_eq!(delivery.extension_kind, INTENT_STATUS_KIND);
    assert_eq!(delivery.attrs, vec![("venue", "cow".to_owned())]);
    let Trigger::Extension(trigger) = delivery.trigger else {
        panic!("the platform delivers an extension trigger");
    };
    assert_eq!(trigger.extension_kind, INTENT_STATUS_KIND);
    let update = IntentStatusUpdate::decode(&trigger.payload).expect("the envelope decodes");
    assert_eq!(update.venue, "cow");
    assert_eq!(update.receipt, b"receipt");
    let body = StatusBody::decode(&update.status).expect("the status body decodes");
    assert_eq!(body.status, videre_status_body::IntentStatus::Fulfilled);

    // The terminal status pruned the watch, so nothing is left to poll.
    assert_eq!(registry.watched_count(), 0);
}

/// With nothing demanding the kind, or with no venue registered, the
/// platform opens no source at all.
#[tokio::test]
async fn the_status_poll_source_stays_closed_without_demand_or_a_venue() {
    let with_venue = Videre::from_registry(registry_with(ScriptedVenue::new([]), [1]));
    assert_eq!(
        open_sources(&with_venue, &BTreeSet::new()),
        0,
        "no demanded kind, no source",
    );

    let without_venue = platform();
    assert_eq!(
        open_sources(&without_venue, &demanding_intent_status()),
        0,
        "no registered venue, no source",
    );
}

/// The keeper-facing `videre:venue/client` import adds to a worker linker,
/// beside the capability and trigger kinds the platform declares.
///
/// `link` also fills the process-wide registry slot the client glue reads,
/// so this test is the only publisher in this binary. A test that needs its
/// own registry must drive [`VenueRegistry`] directly, because the slot
/// keeps the first publish for the life of the process.
#[test]
fn the_client_import_links_into_a_worker_linker() {
    let videre = Videre::from_registry(registry_with(ScriptedVenue::new([]), [1]));
    let engine = test_wasmtime_engine();
    let mut linker: Linker<HostState<MockTypes>> = Linker::new(&engine);
    Extension::<MockTypes>::link(&videre, &mut linker).expect("the client import links");

    // The declared capability is the one the manifest grants a keeper.
    let caps = Extension::<MockTypes>::capabilities(&videre);
    assert_eq!(caps.prefix, "videre:venue/");
    assert_eq!(caps.ifaces, &["client"]);
    assert_eq!(
        Extension::<MockTypes>::emits_trigger_kinds(&videre),
        &[INTENT_STATUS_KIND],
    );
}

/// One `Arc<dyn Extension<..>>` is what a composition root wires, so the
/// platform must be object-safe at the seam it advertises.
#[test]
fn the_platform_is_wired_as_one_generic_extension() {
    let extension: Arc<dyn Extension<MockTypes>> = Arc::new(platform());
    assert_eq!(extension.namespace(), VenueRegistry::NAMESPACE);
    assert_eq!(extension.manifest_sections(), &["venue"]);
}
