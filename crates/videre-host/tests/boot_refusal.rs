//! Boot-time refusal of a bad venue manifest: a mismatched body-version
//! pair, an adapter whose export diverges from its manifest, and a blank
//! adapter name all fail the boot before anything registers.
//! A missing wasm artefact fails the run; see `common::module_wasm_or_skip`.

mod common;

use std::sync::Arc;

use nexum_runtime::engine_config::{AdapterEntry, EngineConfig, ModuleEntry};
use nexum_runtime::supervisor::Supervisor;
use nexum_runtime::test_utils::mock_components;
use videre_host::platform;

use common::{make_linker, make_wasmtime_engine, module_wasm_or_skip, videre_assembly};

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
