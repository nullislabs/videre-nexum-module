//! The full round trip over two real components: a module submits through
//! `videre:venue/client`, the host registry forwards to the echo-venue
//! adapter, and the polled `intent-status` fans back to the module, over
//! both the plain client face and the typed keeper client.
//! A missing wasm artefact fails the run; see `common::module_wasm_or_skip`.

mod common;

use std::sync::Arc;

use nexum_runtime::engine_config::{AdapterEntry, EngineConfig, ModuleEntry};
use nexum_runtime::host::component::ChainMethod;
use nexum_runtime::supervisor::Supervisor;
use nexum_runtime::test_utils::rpc::FakeNode;
use nexum_runtime::test_utils::{MockStateStore, mock_components_from, test_chain_configs};
use videre_host::platform;

use common::{
    INTENT_STATUS, block, make_linker, make_wasmtime_engine, module_wasm_or_skip, registry_of,
    status_event, videre_assembly, workspace_path,
};

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
