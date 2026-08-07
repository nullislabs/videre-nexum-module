//! The venue logging capability at the host boundary: a declaring
//! adapter's `tracing` events reach the host log pipeline with level and
//! fields intact, and an undeclared logging import refuses at boot.
//! A missing wasm artefact fails the run; see `common::module_wasm_or_skip`.

mod common;

use std::sync::Arc;

use nexum_runtime::engine_config::{AdapterEntry, EngineConfig};
use nexum_runtime::supervisor::Supervisor;
use nexum_runtime::test_utils::mock_components;
use videre_host::bindings::SubmitOutcome;
use videre_host::platform;

use common::{
    make_linker, make_wasmtime_engine, module_wasm_or_skip, registry_of, venue, videre_assembly,
    workspace_path,
};

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
