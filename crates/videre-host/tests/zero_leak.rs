//! Zero-leak oracle: the host boots the echo venue and routes a worker's
//! submission purely through the generic extension seam, while the
//! `nexum-runtime` crate graph reaches no venue-shaped crate.

mod common;

use std::process::Command;
use std::sync::Arc;

use nexum_runtime::bindings::nexum;
use nexum_runtime::engine_config::{AdapterEntry, EngineConfig, ModuleEntry};
use nexum_runtime::host::component::ChainMethod;
use nexum_runtime::host::extension::Extension;
use nexum_runtime::supervisor::{Supervisor, build_linker};
use nexum_runtime::test_utils::rpc::FakeNode;
use nexum_runtime::test_utils::{
    MockStateStore, MockTypes, mock_components_from, test_chain_configs,
};
use videre_host::{VenueRegistry, platform};

use common::{module_wasm_or_skip, workspace_path};

/// The boot oracle: the venue adapter installs and a worker's submission
/// reaches it, with the platform supplied only as a generic extension.
#[tokio::test]
async fn e2e_echo_venue_boots_and_submits_through_the_generic_seam() {
    let (Some(adapter_wasm), Some(module_wasm)) = (
        module_wasm_or_skip("echo-venue"),
        module_wasm_or_skip("echo-client"),
    ) else {
        return;
    };

    // The adapter reads eth_blockNumber on submit to justify its `chain`
    // grant; program the mock so that read succeeds.
    let chain = FakeNode::new();
    chain.on_method(ChainMethod::EthBlockNumber, "\"0x1\"");
    let components = mock_components_from(&chain, MockStateStore::new());

    let mut engine_config = wasmtime::Config::new();
    engine_config.wasm_component_model(true);
    engine_config.consume_fuel(true);
    let engine = wasmtime::Engine::new(&engine_config).expect("wasmtime engine");

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
    let extensions: Vec<Arc<dyn Extension<MockTypes>>> = vec![Arc::new(platform(&config))];
    let linker = build_linker::<MockTypes>(&engine, &extensions).expect("build_linker");

    let mut supervisor =
        Supervisor::boot(&engine, &linker, &config, &components, &extensions, None)
            .await
            .expect("boot");
    let registry = supervisor
        .services()
        .get::<VenueRegistry>(VenueRegistry::NAMESPACE)
        .expect("registry service");
    assert_eq!(registry.alive_venue_count(), 1, "echo-venue installed");
    assert_eq!(supervisor.alive_count(), 1, "echo-client alive");

    // One block drives the worker's on_block submission; the registry the
    // extension published on the service map observes the accepted receipt.
    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 19_000_000,
        hash: vec![0xab; 32],
        timestamp: 1_700_000_000_000,
    };
    assert_eq!(supervisor.dispatch_block(block).await, 1);
    let updates = registry.poll_status_transitions().await;
    assert!(
        updates.iter().any(|u| u.venue == "echo-venue"),
        "the submission reached the venue; updates were: {updates:?}"
    );
}

/// The graph oracle: `cargo tree` for the host crate (normal + build
/// edges) names no videre, intent, venue, or cow crate.
///
/// After the carve, `nexum-runtime` is a git dependency rather than a
/// local workspace member, so `--all-features` cannot be requested for it
/// (`cargo` rejects feature selection for packages outside the workspace).
/// The subtree is instead rendered with the feature set the workspace
/// already resolves for it (which includes the `test-utils` feature
/// `videre-host` activates), keeping the invariant meaningful: the generic
/// runtime, as this host actually links it, reaches no venue-shaped crate.
#[test]
fn host_crate_graph_reaches_no_venue_shaped_crate() {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "nexum-runtime",
            "-e",
            "normal,build",
            "--prefix",
            "none",
            "--locked",
        ])
        .current_dir(workspace_path(""))
        .output()
        .expect("cargo tree runs");
    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8_lossy(&output.stdout);
    let reached: Vec<&str> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| {
            let name = name.to_lowercase();
            ["videre", "intent", "venue", "cow"]
                .iter()
                .any(|word| name.contains(word))
        })
        .collect();
    assert!(
        reached.is_empty(),
        "venue-shaped crates reached: {reached:?}"
    );
}
