//! The intent-status subscription end to end: a scripted adapter's polled
//! transitions reach a subscribed module, the shared wall clock drives the
//! quote-staleness ledger, and the platform's event source opens only when
//! a subscriber and a venue both exist.
//! A missing wasm artefact fails the run; see `common::module_wasm_or_skip`.

mod common;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use nexum_runtime::engine_config::EngineConfig;
use nexum_runtime::host::extension::{EventSources, Extension};
use nexum_runtime::supervisor::Supervisor;
use nexum_runtime::test_utils::clock::ManualClock;
use nexum_runtime::test_utils::{BootScenario, MockTypes, mock_components};
use videre_host::bindings::{
    IntentHeader, IntentStatus, Quotation, Settlement, SubmitOutcome, VenueError, value_flow,
};
use videre_host::{VenueInvoker, VenueRegistry, VenueRegistryBuilder, Videre, platform};

use common::{
    INTENT_STATUS, boot_one, make_linker, make_wasmtime_engine, module_wasm_or_skip, status_event,
    venue, videre_assembly,
};

/// Scripted registry adapter: accepts every submission with a fixed receipt
/// and serves statuses front-first; once drained, reports `open`.
struct ScriptedAdapter {
    statuses: VecDeque<IntentStatus>,
}

impl ScriptedAdapter {
    fn new(statuses: impl IntoIterator<Item = IntentStatus>) -> Self {
        Self {
            statuses: statuses.into_iter().collect(),
        }
    }
}

fn native(bytes: Vec<u8>) -> value_flow::AssetAmount {
    value_flow::AssetAmount {
        asset: value_flow::Asset::Native,
        amount: bytes,
    }
}

impl VenueInvoker for ScriptedAdapter {
    fn derive_header<'a>(
        &'a mut self,
        _body: &'a [u8],
    ) -> BoxFuture<'a, Result<IntentHeader, VenueError>> {
        Box::pin(async move {
            Ok(IntentHeader {
                gives: native(vec![1]),
                wants: native(Vec::new()),
                settlement: Settlement { chain: 1 },
                authorisation: videre_host::bindings::AuthScheme::Eip712,
            })
        })
    }

    fn quote<'a>(&'a mut self, _body: &'a [u8]) -> BoxFuture<'a, Result<Quotation, VenueError>> {
        Box::pin(async move {
            Ok(Quotation {
                gives: native(vec![1]),
                wants: native(Vec::new()),
                fee: native(Vec::new()),
                valid_until_ms: SCRIPTED_QUOTE_VALIDITY_MS,
            })
        })
    }

    fn submit<'a>(
        &'a mut self,
        _body: &'a [u8],
    ) -> BoxFuture<'a, Result<SubmitOutcome, VenueError>> {
        Box::pin(async move { Ok(SubmitOutcome::Accepted(b"receipt".to_vec())) })
    }

    fn status(&mut self, _receipt: Vec<u8>) -> BoxFuture<'_, Result<IntentStatus, VenueError>> {
        Box::pin(async move { Ok(self.statuses.pop_front().unwrap_or(IntentStatus::Open)) })
    }

    fn cancel(&mut self, _receipt: Vec<u8>) -> BoxFuture<'_, Result<(), VenueError>> {
        Box::pin(async move { Ok(()) })
    }
}

/// The `valid-until-ms` a scripted quote carries. Inside the record
/// horizon of a [`ManualClock`] starting at the epoch, so a quote on that
/// timeline is recorded and stays fresh until a test advances the clock
/// past this instant. No other test quotes through the scripted adapter.
const SCRIPTED_QUOTE_VALIDITY_MS: u64 = 60_000;

/// A registry with one scripted adapter installed under `cow`. Its clock
/// is the builder's real-clock default until a launch replaces it through
/// `Extension::attach_clock`.
fn scripted_registry(adapter: ScriptedAdapter) -> VenueRegistry {
    let registry = VenueRegistryBuilder::new(Default::default()).build();
    registry
        .install_for_test(
            venue("cow"),
            nexum_runtime::host::actor::Liveness::default(),
            adapter,
        )
        .expect("install scripted adapter");
    registry
}

/// Write a manifest subscribing the example module to intent-status
/// events from the `cow` venue.
fn echo_client_status_manifest(dir: &Path) -> PathBuf {
    let manifest = dir.join("module.toml");
    std::fs::write(
        &manifest,
        r#"
[module]
name = "echo-client"

[capabilities]
required = ["client", "logging"]

[[subscription]]
kind  = "intent-status"
venue = "cow"
"#,
    )
    .expect("write manifest");
    manifest
}

/// Boot one module against the given videre platform on fresh mocks.
async fn boot_module(videre: &Arc<Videre>, wasm: &Path, manifest: &Path) -> Supervisor<MockTypes> {
    let engine = make_wasmtime_engine();
    let extensions = videre_assembly(videre);
    let linker = make_linker(&engine, &extensions);
    let components = mock_components();
    boot_one(&engine, &linker, wasm, manifest, &components, &extensions).await
}

/// A module subscribed to `intent-status` receives the polled transitions;
/// a transition outside its venue filter is not delivered.
#[tokio::test]
async fn e2e_intent_status_subscription_receives_polled_transitions() {
    let Some(wasm) = module_wasm_or_skip("echo-client") else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = echo_client_status_manifest(dir.path());

    let registry = scripted_registry(ScriptedAdapter::new([
        IntentStatus::Pending,
        IntentStatus::Fulfilled,
    ]));
    let videre = Arc::new(Videre::from_registry(registry.clone()));
    let mut supervisor = boot_module(&videre, &wasm, &manifest).await;
    assert!(
        supervisor
            .subscription_plan()
            .extension_kinds
            .contains(INTENT_STATUS)
    );

    // The registry watches the receipt of an accepted submission and polls
    // the adapter's status export; each poll here observes a transition.
    registry
        .submit("test-caller", &venue("cow"), b"body".to_vec())
        .await
        .expect("submit");

    let mut delivered = 0;
    for _ in 0..2 {
        for update in registry.poll_status_transitions().await {
            delivered += supervisor
                .dispatch_extension_event(status_event(update))
                .await;
        }
    }
    assert_eq!(delivered, 2, "pending then fulfilled, one subscriber each");
    assert_eq!(supervisor.alive_count(), 1, "module must remain alive");

    // A venue outside the module's filter is not delivered.
    let foreign = videre_host::IntentStatusUpdate {
        venue: "other".to_owned(),
        receipt: b"receipt".to_vec(),
        status: videre_status_body::StatusBody {
            status: videre_status_body::IntentStatus::Open,
            proof: None,
            reason: None,
        }
        .encode()
        .expect("encode"),
    };
    assert_eq!(
        supervisor
            .dispatch_extension_event(status_event(foreign))
            .await,
        0
    );
}

/// One [`ManualClock`] drives the whole launch, booted through the
/// runtime's own launch path so the runtime performs the attach rather
/// than this test: `as_override()` feeds the supervisor's WASI clocks and
/// the same wall clock reaches the quote ledger through
/// `Extension::attach_clock`. The runtime already pins that the attached
/// handle is the guest-served one, so the staleness case runs host-side on
/// that timeline: a quote is fresh until the clock advances past its
/// validity, then the submit is refused with the machine-readable
/// `stale-quote:` prefix.
#[tokio::test]
async fn e2e_stale_quote_is_refused_on_the_shared_wall_clock() {
    let Some(wasm) = module_wasm_or_skip("echo-client") else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = echo_client_status_manifest(dir.path());

    let clock = ManualClock::new();
    let registry = scripted_registry(ScriptedAdapter::new([]));
    let videre = Arc::new(Videre::from_registry(registry.clone()));
    let booted = BootScenario::over(mock_components())
        .wasm(wasm)
        .module(manifest)
        .extensions(videre_assembly(&videre))
        .clock(clock.as_override())
        .boot()
        .await
        .expect("boot");
    assert_eq!(
        booted.supervisor.alive_count(),
        1,
        "the module boots on the overridden clocks"
    );

    // Quote, then submit inside the validity window. This leg fails if the
    // launch clock never reaches the ledger: the real wall clock sits far
    // past the scripted validity, so the fresh submit would be refused.
    registry
        .quote("test-caller", &venue("cow"), b"body".to_vec())
        .await
        .expect("quote succeeds");
    registry
        .submit("test-caller", &venue("cow"), b"body".to_vec())
        .await
        .expect("a fresh quote submits");

    // Advance the one clock past the quoted validity: the same bytes are
    // now refused ahead of the adapter.
    clock.advance(Duration::from_millis(SCRIPTED_QUOTE_VALIDITY_MS + 1));
    let err = registry
        .submit("test-caller", &venue("cow"), b"body".to_vec())
        .await
        .expect_err("the stale quote is refused");
    let VenueError::Denied(reason) = err else {
        panic!("expected denied, got {err:?}");
    };
    assert!(
        reason.starts_with("stale-quote: "),
        "machine prefix intact: {reason}"
    );
}

/// The extension seam alone: `attach_clock` lands the handed wall clock
/// in the registry's quote ledger, over the real clock the builder
/// defaults to. Needs no wasm artefact, so the seam stays pinned when the
/// e2e skips.
#[tokio::test]
async fn attach_clock_installs_the_quote_ledger_clock() {
    let clock = ManualClock::new();
    let registry = scripted_registry(ScriptedAdapter::new([]));
    let videre = Videre::from_registry(registry.clone());
    Extension::<MockTypes>::attach_clock(&videre, Arc::new(clock.clone()));

    registry
        .quote("test-caller", &venue("cow"), b"body".to_vec())
        .await
        .expect("quote succeeds");
    registry
        .submit("test-caller", &venue("cow"), b"body".to_vec())
        .await
        .expect("a fresh quote submits on the attached clock");

    clock.advance(Duration::from_millis(SCRIPTED_QUOTE_VALIDITY_MS + 1));
    let err = registry
        .submit("test-caller", &venue("cow"), b"body".to_vec())
        .await
        .expect_err("the stale quote is refused");
    assert!(matches!(err, VenueError::Denied(r) if r.starts_with("stale-quote: ")));
}

/// The event-loop wiring through the real seam: the platform's `events`
/// source opens, its poll task drives the supervisor, and the module's
/// handler observably ran.
#[tokio::test]
async fn e2e_intent_status_flows_through_the_event_loop() {
    use nexum_tasks::{TaskManager, TaskSet};

    let Some(wasm) = module_wasm_or_skip("echo-client") else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = echo_client_status_manifest(dir.path());

    let registry = scripted_registry(ScriptedAdapter::new([]));
    let videre = Arc::new(Videre::from_registry(registry.clone()));

    let engine = make_wasmtime_engine();
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);
    let components = mock_components();
    let logs = components.logs.clone();
    let mut supervisor =
        boot_one(&engine, &linker, &wasm, &manifest, &components, &extensions).await;

    registry
        .submit("test-caller", &venue("cow"), b"body".to_vec())
        .await
        .expect("submit");

    // A fast cadence so the 300 ms window sees the first poll.
    let mut config = EngineConfig::default();
    config.limits.status_poll.interval_ms = Some(10);

    let manager = TaskManager::new();
    let executor = manager.executor();
    let mut tasks = TaskSet::new();
    let subscribed = supervisor.subscription_plan().extension_kinds;
    let streams = {
        let mut sources = EventSources::new(
            &config,
            supervisor.services(),
            &subscribed,
            &executor,
            &mut tasks,
        );
        Extension::<MockTypes>::events(&*videre, &mut sources).expect("open event source")
    };
    assert_eq!(streams.len(), 1, "one status-poll stream opened");

    nexum_runtime::runtime::event_loop::run(
        &mut supervisor,
        Vec::new(),
        Vec::new(),
        streams,
        tasks,
        tokio::time::sleep(Duration::from_millis(300)),
    )
    .await;

    assert_eq!(supervisor.alive_count(), 1, "module must remain alive");
    let runs = logs.list_runs("echo-client");
    assert_eq!(runs.len(), 1, "one run recorded for the echo-client module");
    let page = logs.read(&runs[0].run, 0);
    assert!(
        page.records
            .iter()
            .any(|r| r.message.contains("intent status from venue cow")),
        "the module's on_custom handler decoded the transition; records were: {:?}",
        page.records
            .iter()
            .map(|r| r.message.as_str())
            .collect::<Vec<_>>(),
    );
}

/// With no subscriber or no installed venue, the platform opens no event
/// source.
#[tokio::test]
async fn event_source_stays_closed_without_subscribers_or_venues() {
    use nexum_tasks::{TaskManager, TaskSet};

    let config = EngineConfig::default();
    let manager = TaskManager::new();
    let executor = manager.executor();
    let services = nexum_runtime::host::extension::HostServices::default();

    // A venue is installed but nothing subscribes.
    let with_venue = Arc::new(Videre::from_registry(scripted_registry(
        ScriptedAdapter::new([]),
    )));
    let empty = std::collections::BTreeSet::new();
    let mut tasks = TaskSet::new();
    let mut sources = EventSources::new(&config, &services, &empty, &executor, &mut tasks);
    let streams = Extension::<MockTypes>::events(&*with_venue, &mut sources).expect("events");
    assert!(streams.is_empty(), "no subscriber, no stream");

    // A subscriber exists but no venue is installed.
    let no_venue = Arc::new(platform(&config));
    let subscribed: std::collections::BTreeSet<String> =
        std::iter::once(INTENT_STATUS.to_owned()).collect();
    let mut tasks = TaskSet::new();
    let mut sources = EventSources::new(&config, &services, &subscribed, &executor, &mut tasks);
    let streams = Extension::<MockTypes>::events(&*no_venue, &mut sources).expect("events");
    assert!(streams.is_empty(), "no venue, no stream");
}
