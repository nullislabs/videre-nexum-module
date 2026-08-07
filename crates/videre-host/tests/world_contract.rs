//! World-contract coverage: a component built through the SDK macros
//! imports exactly the capabilities its manifest declares, and the
//! venue-adapter provider linker assembles the scoped transport.
//! A missing wasm artefact fails the run; see `common::module_wasm_or_skip`.

mod common;

use nexum_runtime::manifest::CapabilityRegistry;
use nexum_runtime::supervisor::build_provider_linker;
use nexum_runtime::test_utils::MockTypes;
use videre_host::VenueAdapterKind;

use common::{make_wasmtime_engine, module_wasm_or_skip};

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
