//! The service-lookup miss: with the registry service withheld from the
//! service map, every venue call resolves to `unknown-venue`, distinct
//! from the adapter-map miss where the registry exists but the id does not.
//! A missing wasm artefact fails the run; see `common::module_wasm_or_skip`.

mod common;

use std::sync::Arc;

use nexum_runtime::engine_config::EngineConfig;
use nexum_runtime::host::extension::{Extension, ProviderManifest};
use nexum_runtime::host::state::HostState;
use nexum_runtime::manifest::{ExtensionSections, NamespaceCaps};
use nexum_runtime::test_utils::{MockTypes, mock_components};
use videre_host::{VenueRegistry, Videre, platform};
use wasmtime::component::Linker;

use common::{block, boot_one, make_linker, make_wasmtime_engine, module_wasm_or_skip};

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
