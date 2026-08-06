//! E2E coverage for the videre platform over the generic runtime seam: the
//! venue-adapter provider boot, the client -> registry -> adapter round
//! trip, the status-poll event source, and the trap-to-recovery sweeps.
//! Skips gracefully when a wasm artefact is absent.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use nexum_runtime::bindings::nexum;
use nexum_runtime::engine_config::{
    AdapterEntry, EngineConfig, ModuleEntry, ModuleLimits, PoisonLimitsSection,
};
use nexum_runtime::host::component::ChainMethod;
use nexum_runtime::host::extension::{EventSources, Extension, ExtensionEvent, ProviderManifest};
use nexum_runtime::host::state::HostState;
use nexum_runtime::manifest::{CapabilityRegistry, ExtensionSections, NamespaceCaps};
use nexum_runtime::supervisor::{BootEnv, Supervisor, build_linker, build_provider_linker};
use nexum_runtime::test_utils::clock::ManualClock;
use nexum_runtime::test_utils::rpc::FakeNode;
use nexum_runtime::test_utils::{
    BootScenario, MockStateStore, MockTypes, mock_components, mock_components_from,
    test_chain_configs,
};
use videre_host::bindings::{
    IntentHeader, IntentStatus, Quotation, Settlement, SubmitOutcome, VenueError, value_flow,
};
use videre_host::{
    VenueAdapterKind, VenueId, VenueInvoker, VenueRegistry, VenueRegistryBuilder, Videre, platform,
};
use wasmtime::component::Linker;

/// The subscription kind the platform's status poller emits.
const INTENT_STATUS: &str = "intent-status";

// ── fixtures + assembly ───────────────────────────────────────────────

/// Path under the workspace root (the topmost ancestor with a `Cargo.toml`).
fn workspace_path(relative: &str) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .filter(|d| d.join("Cargo.toml").is_file())
        .last()
        .unwrap_or(manifest)
        .join(relative)
}

/// A test venue id, parsed through the validating boundary.
fn venue(id: &str) -> VenueId {
    id.parse().expect("valid venue id")
}

/// Path to a module's `.wasm` artefact under the workspace target dir,
/// or `None` with a skip message when it is not built.
fn module_wasm_or_skip(module_name: &str) -> Option<PathBuf> {
    let artifact = module_name.replace('-', "_");
    let p = workspace_path(&format!("target/wasm32-wasip2/release/{artifact}.wasm"));
    if p.exists() {
        Some(p)
    } else {
        eprintln!(
            "SKIP: {} not found - build with `cargo build -p {module_name} --target wasm32-wasip2 --release`",
            p.display()
        );
        None
    }
}

fn make_wasmtime_engine() -> wasmtime::Engine {
    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    config.consume_fuel(true);
    wasmtime::Engine::new(&config).expect("wasmtime engine")
}

/// The platform's extension slice, keeping the concrete handle for
/// event-source calls.
fn videre_assembly(videre: &Arc<Videre>) -> Vec<Arc<dyn Extension<MockTypes>>> {
    vec![Arc::clone(videre) as Arc<dyn Extension<MockTypes>>]
}

fn make_linker(
    engine: &wasmtime::Engine,
    extensions: &[Arc<dyn Extension<MockTypes>>],
) -> Linker<HostState<MockTypes>> {
    build_linker::<MockTypes>(engine, extensions).expect("build_linker")
}

/// The registry the booted supervisor publishes.
fn registry_of(supervisor: &Supervisor<MockTypes>) -> Arc<VenueRegistry> {
    supervisor
        .services()
        .get::<VenueRegistry>(VenueRegistry::NAMESPACE)
        .expect("registry service")
}

/// A test block that drives dispatch and the dispatch-time sweeps.
fn block(chain_id: u64) -> nexum::host::types::Block {
    nexum::host::types::Block {
        chain_id,
        number: 19_000_000,
        hash: vec![0xab; 32],
        timestamp: 1_700_000_000_000,
    }
}

/// Wrap a polled transition as the extension event the platform emits.
fn status_event(update: videre_host::IntentStatusUpdate) -> ExtensionEvent {
    let attrs = vec![("venue", update.venue.clone())];
    let payload = update.encode().expect("encode intent-status envelope");
    ExtensionEvent {
        kind: INTENT_STATUS,
        attrs,
        event: nexum::host::types::Event::Custom(nexum::host::types::CustomEvent {
            kind: INTENT_STATUS.to_owned(),
            payload,
        }),
    }
}

// ── world contract ────────────────────────────────────────────────────

/// An adapter built through `#[videre_sdk::venue]` imports exactly the
/// scoped transport its manifest declares (`chain`).
#[test]
fn e2e_echo_venue_component_imports_equal_declared_capabilities() {
    let Some(wasm) = module_wasm_or_skip("echo-venue") else {
        return;
    };
    let engine = make_wasmtime_engine();
    let component = wasmtime::component::Component::from_file(&engine, &wasm).expect("compile");
    let imports: Vec<String> = component
        .component_type()
        .imports(&engine)
        .map(|(name, _)| name.to_owned())
        .collect();

    // Capability-bearing imports resolve to exactly the declared set.
    let registry = CapabilityRegistry::core();
    let caps: std::collections::BTreeSet<&str> = imports
        .iter()
        .filter_map(|name| registry.wit_import_to_cap(name))
        .collect();
    assert_eq!(
        caps,
        std::collections::BTreeSet::from(["chain"]),
        "imports were: {imports:?}"
    );

    // No host key-material or persistence interface leaks in: an adapter
    // structurally cannot reach messaging it never declared, local-store,
    // identity, or logging.
    assert!(
        imports.iter().all(|name| !name.contains("messaging")
            && !name.contains("local-store")
            && !name.contains("identity")
            && !name.contains("logging")),
        "imports were: {imports:?}"
    );
}

/// The venue-adapter provider linker binds the scoped transport plus
/// logging and withholds the core-only interfaces, without a
/// duplicate-definition clash.
#[tokio::test]
async fn provider_linker_assembles_with_scoped_transport() {
    let engine = make_wasmtime_engine();
    build_provider_linker::<MockTypes>(&engine, &VenueAdapterKind)
        .expect("provider linker assembles");
}

/// An adapter declaring `logging` imports `nexum:host/logging` and nothing
/// else capability-bearing; the withheld core interfaces stay out.
#[test]
fn e2e_logging_venue_component_imports_logging_when_declared() {
    let Some(wasm) = module_wasm_or_skip("logging-venue") else {
        return;
    };
    let engine = make_wasmtime_engine();
    let component = wasmtime::component::Component::from_file(&engine, &wasm).expect("compile");
    let imports: Vec<String> = component
        .component_type()
        .imports(&engine)
        .map(|(name, _)| name.to_owned())
        .collect();

    let registry = CapabilityRegistry::core();
    let caps: std::collections::BTreeSet<&str> = imports
        .iter()
        .filter_map(|name| registry.wit_import_to_cap(name))
        .collect();
    assert_eq!(
        caps,
        std::collections::BTreeSet::from(["logging"]),
        "imports were: {imports:?}"
    );
    assert!(
        imports
            .iter()
            .any(|name| name.starts_with("nexum:host/logging")),
        "imports were: {imports:?}"
    );
    // Declared opt-in only: no transport leaks in beside it, and the
    // structurally refused core interfaces stay out.
    assert!(
        imports.iter().all(|name| !name.contains("chain")
            && !name.contains("messaging")
            && !name.contains("local-store")
            && !name.contains("remote-store")
            && !name.contains("identity")),
        "imports were: {imports:?}"
    );
}

// ── intent-status subscription E2E ────────────────────────────────────

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

/// The test `[chains]` set, so a chain-1 subscription admits.
fn test_engine_config() -> EngineConfig {
    EngineConfig {
        chains: test_chain_configs(),
        ..EngineConfig::default()
    }
}

/// Boot one module from its wasm and manifest through `boot_single`.
async fn boot_one(
    engine: &wasmtime::Engine,
    linker: &Linker<HostState<MockTypes>>,
    wasm: &Path,
    manifest: &Path,
    components: &nexum_runtime::host::component::Components<MockTypes>,
    extensions: &[Arc<dyn Extension<MockTypes>>],
) -> Supervisor<MockTypes> {
    let entry = ModuleEntry {
        path: wasm.to_path_buf(),
        manifest: Some(manifest.to_path_buf()),
    };
    let config = test_engine_config();
    Supervisor::boot_single(
        engine,
        linker,
        &entry,
        components,
        &BootEnv::from_config(&config),
        extensions,
        None,
    )
    .await
    .expect("boot_single")
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

// ── echo round trip ───────────────────────────────────────────────────

/// End to end over two real components: the echo-client module submits
/// through `videre:venue/client`, the host registry forwards to the
/// echo-venue adapter, and the module receives the fulfilled
/// `intent-status` polled back.
#[tokio::test]
async fn e2e_echo_module_registry_adapter_round_trip() {
    let (Some(adapter_wasm), Some(module_wasm)) = (
        module_wasm_or_skip("echo-venue"),
        module_wasm_or_skip("echo-client"),
    ) else {
        return;
    };

    // The adapter reads eth_blockNumber on submit to justify its `chain`
    // grant; program the mock so that read succeeds. The response body is
    // discarded by the adapter, so any Ok value serves.
    let chain = FakeNode::new();
    chain.on_method(ChainMethod::EthBlockNumber, "\"0x1\"");
    let components = mock_components_from(&chain, MockStateStore::new());
    let logs = components.logs.clone();

    let engine = make_wasmtime_engine();
    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: adapter_wasm,
            manifest: Some(workspace_path("modules/examples/echo-venue/module.toml")),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        modules: vec![ModuleEntry {
            path: module_wasm,
            manifest: Some(workspace_path("modules/examples/echo-client/module.toml")),
        }],
        chains: test_chain_configs(),
        ..Default::default()
    };
    let videre = Arc::new(platform(&config));
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);

    let mut supervisor =
        Supervisor::boot(&engine, &linker, &config, &components, &extensions, None)
            .await
            .expect("boot");
    assert_eq!(
        registry_of(&supervisor).alive_venue_count(),
        1,
        "echo-venue is routable"
    );
    assert_eq!(supervisor.alive_count(), 1, "echo-client is alive");
    assert!(
        supervisor
            .subscription_plan()
            .extension_kinds
            .contains(INTENT_STATUS)
    );

    // A block drives the module's on_block, which submits to the echo venue
    // through the shared registry; the registry watches the accepted receipt.
    assert_eq!(supervisor.dispatch_block(block(1)).await, 1);

    // Poll the registry the module submitted through and fan its transitions
    // back to the module. echo-venue settles instantly, so the first poll
    // reports a terminal status and the watch is pruned.
    let registry = registry_of(&supervisor);
    let mut delivered = 0;
    for _ in 0..2 {
        for update in registry.poll_status_transitions().await {
            assert_eq!(update.venue, "echo-venue");
            let body = videre_status_body::StatusBody::decode(&update.status)
                .expect("status body decodes");
            assert_eq!(
                body.status,
                videre_status_body::IntentStatus::Fulfilled,
                "echo settles instantly",
            );
            delivered += supervisor
                .dispatch_extension_event(status_event(update))
                .await;
        }
    }
    assert_eq!(
        delivered, 1,
        "one terminal status delivered to the subscriber"
    );
    assert_eq!(supervisor.alive_count(), 1, "module must remain alive");

    // The module observably completed the round trip: it quoted, it
    // submitted, and it received the settled status from the echo venue.
    let runs = logs.list_runs("echo-client");
    assert_eq!(runs.len(), 1, "one run recorded for echo-client");
    let page = logs.read(&runs[0].run, 0);
    let messages: Vec<&str> = page.records.iter().map(|r| r.message.as_str()).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("quoted") && m.contains("echo-venue")),
        "module quoted through the client face; records were: {messages:?}",
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("submitted") && m.contains("echo-venue")),
        "module submitted through the client face; records were: {messages:?}",
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("intent status from venue echo-venue")),
        "module received the settled status; records were: {messages:?}",
    );
}

/// The keeper path over two real components: the echo-keeper module (built
/// by `#[videre_sdk::keeper]`) drives the echo-venue adapter through the
/// typed `VenueClient<EchoVenue>` (quote, submit, status, cancel) and
/// receives the fulfilled `intent-status` polled back.
#[tokio::test]
async fn e2e_keeper_module_drives_the_venue_through_the_typed_client() {
    let (Some(adapter_wasm), Some(module_wasm)) = (
        module_wasm_or_skip("echo-venue"),
        module_wasm_or_skip("echo-keeper"),
    ) else {
        return;
    };

    let chain = FakeNode::new();
    chain.on_method(ChainMethod::EthBlockNumber, "\"0x1\"");
    let components = mock_components_from(&chain, MockStateStore::new());
    let logs = components.logs.clone();

    let engine = make_wasmtime_engine();
    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: adapter_wasm,
            manifest: Some(workspace_path("modules/examples/echo-venue/module.toml")),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        modules: vec![ModuleEntry {
            path: module_wasm,
            manifest: Some(workspace_path("modules/examples/echo-keeper/module.toml")),
        }],
        chains: test_chain_configs(),
        ..Default::default()
    };
    let videre = Arc::new(platform(&config));
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);

    let mut supervisor =
        Supervisor::boot(&engine, &linker, &config, &components, &extensions, None)
            .await
            .expect("boot");
    assert_eq!(
        registry_of(&supervisor).alive_venue_count(),
        1,
        "echo-venue is routable"
    );
    assert_eq!(supervisor.alive_count(), 1, "echo-keeper is alive");

    // One block drives the keeper's async on_block: quote, submit,
    // status, cancel, all through the typed client.
    assert_eq!(supervisor.dispatch_block(block(1)).await, 1);

    // The accepted receipt is under status watch; echo settles
    // instantly, so the first poll fans the terminal status back.
    let registry = registry_of(&supervisor);
    let mut delivered = 0;
    for _ in 0..2 {
        for update in registry.poll_status_transitions().await {
            assert_eq!(update.venue, "echo-venue");
            delivered += supervisor
                .dispatch_extension_event(status_event(update))
                .await;
        }
    }
    assert_eq!(delivered, 1, "one terminal status delivered to the keeper");
    assert_eq!(supervisor.alive_count(), 1, "keeper must remain alive");

    // Every typed verb observably ran.
    let runs = logs.list_runs("echo-keeper");
    assert_eq!(runs.len(), 1, "one run recorded for echo-keeper");
    let page = logs.read(&runs[0].run, 0);
    let messages: Vec<&str> = page.records.iter().map(|r| r.message.as_str()).collect();
    for needle in [
        "quoted at echo-venue",
        "submitted to echo-venue",
        "status at echo-venue",
        "cancelled at echo-venue",
        "intent status from venue echo-venue",
    ] {
        assert!(
            messages.iter().any(|m| m.contains(needle)),
            "missing `{needle}`; records were: {messages:?}",
        );
    }
}

/// The body-version handshake refuses a mismatched pair: an adapter decoding
/// only v1 against a keeper encoding v2 fails the boot before instantiation.
#[tokio::test]
async fn e2e_mismatched_body_versions_refuse_the_pair_at_boot() {
    let (Some(adapter_wasm), Some(module_wasm)) = (
        module_wasm_or_skip("echo-venue"),
        module_wasm_or_skip("echo-client"),
    ) else {
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let adapter_manifest = dir.path().join("echo-venue.toml");
    std::fs::write(
        &adapter_manifest,
        r#"
[module]
name = "echo-venue"
kind = "venue-adapter"

[capabilities]
required = ["chain"]

[venue]
body_versions = [1]
"#,
    )
    .expect("write adapter manifest");
    let keeper_manifest = dir.path().join("echo-client.toml");
    std::fs::write(
        &keeper_manifest,
        r#"
[module]
name = "echo-client"

[capabilities]
required = ["client", "logging"]

[venue]
body_version = 2
"#,
    )
    .expect("write keeper manifest");

    let engine = make_wasmtime_engine();
    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: adapter_wasm,
            manifest: Some(adapter_manifest),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        modules: vec![ModuleEntry {
            path: module_wasm,
            manifest: Some(keeper_manifest),
        }],
        ..Default::default()
    };
    let videre = Arc::new(platform(&config));
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);
    let components = mock_components();

    let Err(err) =
        Supervisor::boot(&engine, &linker, &config, &components, &extensions, None).await
    else {
        panic!("mismatched pair must refuse to boot");
    };
    let chain = format!("{err:#}");
    assert!(chain.contains("body version 2"), "{chain}");
    assert!(chain.contains("echo-venue decodes {1}"), "{chain}");
}

/// An adapter whose `body-versions()` export diverges from its manifest
/// `[venue] body_versions` fails its own install.
#[tokio::test]
async fn e2e_manifest_export_divergence_refuses_the_adapter_at_boot() {
    let Some(adapter_wasm) = module_wasm_or_skip("echo-venue") else {
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let adapter_manifest = dir.path().join("echo-venue.toml");
    std::fs::write(
        &adapter_manifest,
        r#"
[module]
name = "echo-venue"
kind = "venue-adapter"

[capabilities]
required = ["chain"]

[venue]
body_versions = [1, 2]
"#,
    )
    .expect("write adapter manifest");

    let engine = make_wasmtime_engine();
    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: adapter_wasm,
            manifest: Some(adapter_manifest),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        ..Default::default()
    };
    let videre = Arc::new(platform(&config));
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);
    let components = mock_components();

    let Err(err) =
        Supervisor::boot(&engine, &linker, &config, &components, &extensions, None).await
    else {
        panic!("a diverging adapter must refuse to boot");
    };
    let chain = format!("{err:#}");
    assert!(chain.contains("exports body versions {1}"), "{chain}");
    assert!(chain.contains("declares {1, 2}"), "{chain}");
}

/// An adapter whose manifest name is whitespace-only is refused before it
/// can register a blank venue id; the runtime rejects it at manifest parse.
#[tokio::test]
async fn e2e_blank_manifest_name_refuses_the_adapter_at_boot() {
    let Some(adapter_wasm) = module_wasm_or_skip("echo-venue") else {
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let adapter_manifest = dir.path().join("echo-venue.toml");
    std::fs::write(
        &adapter_manifest,
        r#"
[module]
name = "  "
kind = "venue-adapter"

[capabilities]
required = ["chain"]

[venue]
body_versions = [1]
"#,
    )
    .expect("write adapter manifest");

    let engine = make_wasmtime_engine();
    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: adapter_wasm,
            manifest: Some(adapter_manifest),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        ..Default::default()
    };
    let videre = Arc::new(platform(&config));
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);
    let components = mock_components();

    let Err(err) =
        Supervisor::boot(&engine, &linker, &config, &components, &extensions, None).await
    else {
        panic!("a blank-named adapter must refuse to boot");
    };
    let chain = format!("{err:#}");
    assert!(
        chain.contains("[module].name is missing or blank"),
        "{chain}"
    );
}

/// A logging-declaring adapter's `tracing` events reach the host log
/// pipeline from `init` and from a dispatched verb, as host-interface
/// records with each event's own level and its structured fields intact:
/// the stderr-sink workaround's signature (every line a WARN stderr
/// record) must not be the carrier.
#[tokio::test]
async fn e2e_logging_adapter_tracing_reaches_the_host_pipeline() {
    use nexum_runtime::host::logs::LogSource;

    let Some(wasm) = module_wasm_or_skip("logging-venue") else {
        return;
    };
    let components = mock_components();
    let logs = components.logs.clone();
    let engine = make_wasmtime_engine();
    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: wasm,
            manifest: Some(workspace_path("modules/fixtures/logging-venue/module.toml")),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        ..Default::default()
    };
    let videre = Arc::new(platform(&config));
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);

    let supervisor = Supervisor::boot(&engine, &linker, &config, &components, &extensions, None)
        .await
        .expect("boot");
    let registry = registry_of(&supervisor);
    assert_eq!(registry.alive_venue_count(), 1, "logging-venue boots alive");

    // The motivating case is a verb-interior fact, not a boot-time one, so
    // drive one submit: the host logging call must carry from inside a
    // dispatched verb as well as from `init`.
    let outcome = registry
        .submit("mod-a", &venue("logging-venue"), b"body".to_vec())
        .await
        .expect("the adapter accepts");
    assert!(matches!(outcome, SubmitOutcome::Accepted(r) if r == b"body"));

    // `init` installed the facade, emitted one INFO and one WARN event
    // carrying fields, then one self-naming probe per level; `submit`
    // added its own. All must land as host-interface records at the
    // emitting event's level, fields rendered in.
    let runs = logs.list_runs("logging-venue");
    assert_eq!(runs.len(), 1, "one run recorded for the adapter");
    let page = logs.read(&runs[0].run, 0);
    let dump = || {
        page.records
            .iter()
            .map(|r| (r.source, r.level, r.message.as_str()))
            .collect::<Vec<_>>()
    };
    let record_at = |level: tracing::Level, needle: &str| {
        page.records.iter().find(|r| {
            r.source == LogSource::HostInterface && r.level == level && r.message.contains(needle)
        })
    };
    let info = record_at(tracing::Level::INFO, "logging-venue facade installed");
    assert!(
        info.is_some(),
        "INFO facade record missing; records were: {:?}",
        dump(),
    );
    assert!(
        info.expect("info record").message.contains("flow=init"),
        "the structured field must survive to the host record",
    );
    assert!(
        record_at(tracing::Level::WARN, "logging-venue config sighted").is_some(),
        "WARN record missing at its own level; records were: {:?}",
        dump(),
    );
    assert!(
        record_at(tracing::Level::INFO, "logging-venue submit")
            .is_some_and(|r| r.message.contains("body_len=4")),
        "the verb-interior record must reach the pipeline with its fields; records were: {:?}",
        dump(),
    );

    // The whole level ladder the venue macro emits, one self-naming probe
    // per level. Holding each probe's message to the level it arrived at
    // fails a transposed arm; a record count per level would not, since
    // any permutation of the arms preserves it.
    const LEVEL_PROBE: &str = "logging-venue level probe";
    for (level, name) in [
        (tracing::Level::TRACE, "trace"),
        (tracing::Level::DEBUG, "debug"),
        (tracing::Level::INFO, "info"),
        (tracing::Level::WARN, "warn"),
        (tracing::Level::ERROR, "error"),
    ] {
        let expected = format!("{LEVEL_PROBE} {name}");
        let levels: Vec<tracing::Level> = page
            .records
            .iter()
            .filter(|r| r.source == LogSource::HostInterface && r.message == expected)
            .map(|r| r.level)
            .collect();
        assert_eq!(
            levels,
            vec![level],
            "the {name} probe must arrive exactly once, at {level}; records were: {:?}",
            dump(),
        );
    }
}

/// An adapter whose component imports `nexum:host/logging` without
/// declaring the capability refuses at boot: the runtime holds the
/// provider's imports to its manifest just as it does for modules.
#[tokio::test]
async fn e2e_undeclared_logging_import_refuses_the_adapter_at_boot() {
    let Some(wasm) = module_wasm_or_skip("logging-venue") else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = dir.path().join("module.toml");
    std::fs::write(
        &manifest,
        r#"
[module]
name = "logging-venue"
version = "0.1.0"
kind = "venue-adapter"

[capabilities]
required = []
optional = []
"#,
    )
    .expect("write boot manifest");

    let components = mock_components();
    let engine = make_wasmtime_engine();
    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: wasm,
            manifest: Some(manifest),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        ..Default::default()
    };
    let videre = Arc::new(platform(&config));
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);

    let Err(err) =
        Supervisor::boot(&engine, &linker, &config, &components, &extensions, None).await
    else {
        panic!("an undeclared logging import must refuse to boot");
    };
    let chain = format!("{err:#}");
    assert!(chain.contains("capability violation"), "{chain}");
    assert!(chain.contains("nexum:host/logging"), "{chain}");
    assert!(chain.contains("not listed in [capabilities]"), "{chain}");
}

// ── venue-adapter trap recovery ───────────────────────────────────────

/// Boot one flaky-venue adapter over the mock chain, its head at the
/// fixture's poison sentinel. Returns the chain handle for recovery.
async fn boot_flaky_venue(
    adapter_wasm: PathBuf,
    limits: ModuleLimits,
) -> (Supervisor<MockTypes>, FakeNode) {
    let chain = FakeNode::new();
    chain.on_method(ChainMethod::EthBlockNumber, "\"0xdead\"");
    let components = mock_components_from(&chain, MockStateStore::new());
    let engine = make_wasmtime_engine();
    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: adapter_wasm,
            manifest: Some(workspace_path("modules/fixtures/flaky-venue/module.toml")),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        limits,
        ..Default::default()
    };
    let videre = Arc::new(platform(&config));
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);
    let supervisor = Supervisor::boot(&engine, &linker, &config, &components, &extensions, None)
        .await
        .expect("boot");
    (supervisor, chain)
}

/// The trap-to-recovery lifecycle over a real wasm adapter: a trapped venue
/// is `unavailable` (not `unknown-venue`), the restart sweep reinstantiates
/// it after backoff, and a submit then succeeds again.
#[tokio::test]
async fn e2e_trapped_adapter_is_swept_and_restarts() {
    let Some(wasm) = module_wasm_or_skip("flaky-venue") else {
        return;
    };
    let (mut supervisor, chain) = boot_flaky_venue(wasm, ModuleLimits::default()).await;
    assert_eq!(supervisor.adapter_count(), 1);
    let registry = registry_of(&supervisor);
    assert_eq!(registry.alive_venue_count(), 1, "boots alive");
    let flaky = venue("flaky-venue");

    // The poison head detonates submit: the guest panic traps the store
    // and the shared liveness drops.
    let err = registry
        .submit("mod-a", &flaky, b"body".to_vec())
        .await
        .expect_err("the poison head traps the adapter");
    assert!(matches!(err, VenueError::Unavailable(_)), "{err:?}");
    assert_eq!(registry.alive_venue_count(), 0, "the trap drops liveness");

    // Temporarily dead resolves distinctly from never installed.
    assert!(matches!(
        registry.submit("mod-a", &flaky, b"body".to_vec()).await,
        Err(VenueError::Unavailable(_))
    ));
    assert!(matches!(
        registry
            .submit("mod-a", &venue("unlisted"), b"body".to_vec())
            .await,
        Err(VenueError::UnknownVenue)
    ));

    // The venue recovers; past the 1s backoff the dispatch-time sweep
    // reinstalls the adapter on a fresh store.
    chain.on_method(ChainMethod::EthBlockNumber, "\"0x1\"");
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    supervisor.dispatch_block(block(1)).await;
    assert_eq!(registry.alive_venue_count(), 1, "the sweep revived it");
    let outcome = registry
        .submit("mod-a", &flaky, b"body".to_vec())
        .await
        .expect("the recovered adapter accepts");
    assert!(matches!(outcome, SubmitOutcome::Accepted(r) if r == b"body"));
}

/// A crash-looping adapter is quarantined by the poison sweep: at the
/// threshold the restarts stop and the venue stays dead.
#[tokio::test]
async fn e2e_crash_looping_adapter_is_poisoned() {
    let Some(wasm) = module_wasm_or_skip("flaky-venue") else {
        return;
    };
    let limits = ModuleLimits {
        poison: PoisonLimitsSection {
            max_failures: Some(2),
            window_secs: Some(600),
        },
        ..ModuleLimits::default()
    };
    // The chain head stays at the poison sentinel for the whole test: every
    // submit after a restart traps again.
    let (mut supervisor, _chain) = boot_flaky_venue(wasm, limits).await;
    let registry = registry_of(&supervisor);
    let flaky = venue("flaky-venue");

    // Trap 1, then a successful restart past the 1s backoff.
    let _ = registry.submit("mod-a", &flaky, b"body".to_vec()).await;
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    supervisor.dispatch_block(block(1)).await;
    assert_eq!(registry.alive_venue_count(), 1, "first restart lands");

    // Trap 2 crosses the 2-failure threshold: the sweep quarantines the
    // adapter instead of scheduling another restart.
    let _ = registry.submit("mod-a", &flaky, b"body".to_vec()).await;
    supervisor.dispatch_block(block(1)).await;
    assert_eq!(registry.alive_venue_count(), 0, "quarantined");

    // Past every backoff the poisoned adapter stays dead and unavailable.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    supervisor.dispatch_block(block(1)).await;
    assert_eq!(registry.alive_venue_count(), 0, "no restart while poisoned");
    assert!(matches!(
        registry.submit("mod-a", &flaky, b"body".to_vec()).await,
        Err(VenueError::Unavailable(_))
    ));
}

// ── service-missing unknown-venue ─────────────────────────────────────

/// The videre platform with its registry service withheld: `service`
/// returns `None`, so `HostServices::from_extensions` seeds no venue
/// registry.
struct ClientWithoutRegistry(Videre);

impl Extension<MockTypes> for ClientWithoutRegistry {
    fn namespace(&self) -> &'static str {
        Extension::<MockTypes>::namespace(&self.0)
    }

    fn capabilities(&self) -> NamespaceCaps {
        Extension::<MockTypes>::capabilities(&self.0)
    }

    fn link(&self, linker: &mut Linker<HostState<MockTypes>>) -> anyhow::Result<()> {
        Extension::<MockTypes>::link(&self.0, linker)
    }

    fn manifest_sections(&self) -> &'static [&'static str] {
        Extension::<MockTypes>::manifest_sections(&self.0)
    }

    fn subscriptions(&self) -> &'static [&'static str] {
        Extension::<MockTypes>::subscriptions(&self.0)
    }

    fn admit_worker(
        &self,
        worker: &str,
        sections: &ExtensionSections,
        providers: &[ProviderManifest],
    ) -> anyhow::Result<()> {
        Extension::<MockTypes>::admit_worker(&self.0, worker, sections, providers)
    }
}

/// The service-lookup miss: with no registry service seeded, `client.rs`
/// resolves every venue call to `unknown-venue`, distinct from the
/// adapter-map miss where the registry is present but the venue id unlisted.
#[tokio::test]
async fn client_without_registry_service_resolves_every_venue_to_unknown() {
    let Some(wasm) = module_wasm_or_skip("echo-client") else {
        return;
    };

    // A [venue]-free manifest: no adapter boots under `boot_single`, so a
    // keeper declaring `[venue] body_version` would be refused before it
    // could reach the client face at all.
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = dir.path().join("echo-client.toml");
    std::fs::write(
        &manifest,
        r#"
[module]
name = "echo-client"

[capabilities]
required = ["client", "logging"]

[[subscription]]
kind     = "block"
chain_id = 1
"#,
    )
    .expect("write manifest");

    let engine = make_wasmtime_engine();
    let extensions: Vec<Arc<dyn Extension<MockTypes>>> = vec![Arc::new(ClientWithoutRegistry(
        platform(&EngineConfig::default()),
    ))];
    let linker = make_linker(&engine, &extensions);
    let components = mock_components();
    let logs = components.logs.clone();

    let mut supervisor =
        boot_one(&engine, &linker, &wasm, &manifest, &components, &extensions).await;

    // The precondition of the client.rs unknown-venue branch: the booted
    // service map holds no venue registry.
    assert!(
        supervisor
            .services()
            .get::<VenueRegistry>(VenueRegistry::NAMESPACE)
            .is_none(),
        "boot_single must seed no registry service",
    );

    // One chain-1 block drives the keeper's quote then submit; with no
    // registry both resolve to unknown-venue, which the keeper absorbs.
    assert_eq!(supervisor.dispatch_block(block(1)).await, 1);
    assert_eq!(supervisor.alive_count(), 1, "the keeper stays alive");

    let runs = logs.list_runs("echo-client");
    assert_eq!(runs.len(), 1, "one run recorded for echo-client");
    let page = logs.read(&runs[0].run, 0);
    let messages: Vec<&str> = page.records.iter().map(|r| r.message.as_str()).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("quote at echo-venue was refused")),
        "quote resolved to unknown-venue; records were: {messages:?}",
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("submit to echo-venue was refused")),
        "submit resolved to unknown-venue; records were: {messages:?}",
    );
}
