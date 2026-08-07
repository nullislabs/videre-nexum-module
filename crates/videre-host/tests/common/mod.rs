//! Shared fixtures for the videre-host integration tests: workspace paths,
//! the hardened wasm lookup, and the platform boot assembly.
//!
//! Every `tests/*.rs` binary compiles this module afresh and uses a subset,
//! so the unused-item lint is silenced here. The wasm lookup's own unit
//! tests live in `wasm_helper.rs`, one binary, so they do not multiply
//! across the suites.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nexum_runtime::bindings::nexum;
use nexum_runtime::engine_config::{EngineConfig, ModuleEntry};
use nexum_runtime::host::extension::{Extension, ExtensionEvent};
use nexum_runtime::host::state::HostState;
use nexum_runtime::supervisor::{BootEnv, Supervisor, build_linker};
use nexum_runtime::test_utils::{MockTypes, test_chain_configs};
use videre_host::{VenueId, VenueRegistry, Videre};
use wasmtime::component::Linker;

/// Path under the workspace root (the topmost ancestor with a `Cargo.toml`).
pub fn workspace_path(relative: &str) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .filter(|d| d.join("Cargo.toml").is_file())
        .last()
        .unwrap_or(manifest)
        .join(relative)
}

/// Opt-in escape hatch for a run with no guest wasms built.
const SKIP_VAR: &str = "VIDERE_SKIP_MISSING_WASMS";

/// Path to a module's `.wasm` artefact under the workspace target dir.
/// A missing artefact fails the test: skipping is what let a run with zero
/// wasms built report all green, and a runner captures the skip message of a
/// passing test. `VIDERE_SKIP_MISSING_WASMS` opts back into the skip, except
/// under `CI` where the gate may not excuse itself.
pub fn module_wasm_or_skip(module_name: &str) -> Option<PathBuf> {
    wasm_or_skip(
        module_name,
        std::env::var_os(SKIP_VAR).is_some(),
        std::env::var_os("CI").is_some(),
    )
}

/// The seam under [`module_wasm_or_skip`], env reads lifted out so
/// `wasm_helper.rs` pins the skip/hard-fail policy without touching the
/// process environment.
pub fn wasm_or_skip(module_name: &str, skip_requested: bool, ci: bool) -> Option<PathBuf> {
    let artifact = module_name.replace('-', "_");
    let p = workspace_path(&format!("target/wasm32-wasip2/release/{artifact}.wasm"));
    if p.is_file() {
        return Some(p);
    }
    assert!(
        skip_requested && !ci,
        "{} is missing - run `just build-modules`",
        p.display()
    );
    eprintln!("SKIP: {} not found - run `just build-modules`", p.display());
    None
}

/// The subscription kind the platform's status poller emits.
pub const INTENT_STATUS: &str = "intent-status";

/// A test venue id, parsed through the validating boundary.
pub fn venue(id: &str) -> VenueId {
    id.parse().expect("valid venue id")
}

pub fn make_wasmtime_engine() -> wasmtime::Engine {
    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    config.consume_fuel(true);
    wasmtime::Engine::new(&config).expect("wasmtime engine")
}

/// The platform's extension slice, keeping the concrete handle for
/// event-source calls.
pub fn videre_assembly(videre: &Arc<Videre>) -> Vec<Arc<dyn Extension<MockTypes>>> {
    vec![Arc::clone(videre) as Arc<dyn Extension<MockTypes>>]
}

pub fn make_linker(
    engine: &wasmtime::Engine,
    extensions: &[Arc<dyn Extension<MockTypes>>],
) -> Linker<HostState<MockTypes>> {
    build_linker::<MockTypes>(engine, extensions).expect("build_linker")
}

/// The registry the booted supervisor publishes.
pub fn registry_of(supervisor: &Supervisor<MockTypes>) -> Arc<VenueRegistry> {
    supervisor
        .services()
        .get::<VenueRegistry>(VenueRegistry::NAMESPACE)
        .expect("registry service")
}

/// A test block that drives dispatch and the dispatch-time sweeps.
pub fn block(chain_id: u64) -> nexum::host::types::Block {
    nexum::host::types::Block {
        chain_id,
        number: 19_000_000,
        hash: vec![0xab; 32],
        timestamp: 1_700_000_000_000,
    }
}

/// Wrap a polled transition as the extension event the platform emits.
pub fn status_event(update: videre_host::IntentStatusUpdate) -> ExtensionEvent {
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

/// The test `[chains]` set, so a chain-1 subscription admits.
fn test_engine_config() -> EngineConfig {
    EngineConfig {
        chains: test_chain_configs(),
        ..EngineConfig::default()
    }
}

/// Boot one module from its wasm and manifest through `boot_single`.
pub async fn boot_one(
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
