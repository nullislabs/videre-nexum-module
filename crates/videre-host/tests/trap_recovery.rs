//! Venue-adapter trap recovery: a trapped venue reads `unavailable`, the
//! dispatch-time sweep restarts it past backoff, and a crash loop is
//! quarantined at the poison threshold.
//! A missing wasm artefact fails the run; see `common::module_wasm_or_skip`.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nexum_runtime::engine_config::{AdapterEntry, EngineConfig, ModuleLimits, PoisonLimitsSection};
use nexum_runtime::host::component::ChainMethod;
use nexum_runtime::supervisor::Supervisor;
use nexum_runtime::test_utils::rpc::FakeNode;
use nexum_runtime::test_utils::{MockStateStore, MockTypes, mock_components_from};
use videre_host::bindings::{SubmitOutcome, VenueError};
use videre_host::platform;

use common::{
    block, make_linker, make_wasmtime_engine, module_wasm_or_skip, registry_of, venue,
    videre_assembly, workspace_path,
};

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
