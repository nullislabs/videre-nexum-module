//! The videre venue platform as one [`nexum_runtime`] extension: the
//! [`VenueRegistry`] service, the advisory [`EgressGuard`] seam, and the
//! keeper-facing `videre:venue/client` interface. A composition root wires
//! it via `builder.with_extensions([Arc::new(videre_host::platform())])`.
//!
//! A venue is a native Rust [`VenueInvoker`] the composition root registers
//! with [`VenueRegistry::register`]. Venues were guest wasm components of
//! kind `venue-adapter` until the runtime deleted the extension-installed
//! component path: an extension can no longer install or supervise a guest,
//! so the adapter seam moved in-process.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

pub mod bindings;
mod client;
mod handshake;
pub mod policy;
mod registry;

use std::sync::Arc;
use std::time::Duration;

use nexum_runtime::bindings::nexum::host::types::{ExtensionTrigger, Trigger};
use nexum_runtime::component::RuntimeTypes;
use nexum_runtime::extension::{
    Extension, ExtensionDelivery, ExtensionError, ExtensionSource, HostState, SourceContext,
};
use nexum_runtime::manifest::{ExtensionSections, NamespaceCaps};
use tokio::sync::mpsc;
use tracing::warn;
use videre_status_body::INTENT_STATUS_KIND;
use wasmtime::component::{HasSelf, Linker};

pub use policy::{Liveness, SubmitQuota, WatchLimit};
pub use registry::{
    DuplicateVenue, EgressGuard, GuardContext, GuardVerdict, IntentStatusUpdate, VenueId,
    VenueInvoker, VenueRegistry, VenueRegistryBuilder,
};

/// Status-poll channel buffer.
const STATUS_CHANNEL_BUF: usize = 64;

/// Default cadence the status-poll source refreshes watched receipts at.
pub const DEFAULT_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// The venue platform over the default quota and watch policy, with the
/// unit guard. Register venues on [`Videre::registry`] before launch.
#[must_use]
pub fn platform() -> Videre {
    Videre::from_registry(VenueRegistryBuilder::new(SubmitQuota::default()).build())
}

/// The videre platform as one runtime extension.
pub struct Videre {
    registry: Arc<VenueRegistry>,
    status_poll_interval: Duration,
}

impl Videre {
    /// Assemble over a pre-built registry, for a custom [`EgressGuard`] or
    /// policy.
    #[must_use]
    pub fn from_registry(registry: VenueRegistry) -> Self {
        Self {
            registry: Arc::new(registry),
            status_poll_interval: DEFAULT_STATUS_POLL_INTERVAL,
        }
    }

    /// Override the status-poll cadence.
    #[must_use]
    pub fn with_status_poll_interval(mut self, interval: Duration) -> Self {
        self.status_poll_interval = interval;
        self
    }

    /// The shared registry, for registering venues before launch.
    #[must_use]
    pub fn registry(&self) -> &Arc<VenueRegistry> {
        &self.registry
    }
}

impl<T> Extension<T> for Videre
where
    T: RuntimeTypes<State = HostState<T>>,
{
    fn namespace(&self) -> &'static str {
        VenueRegistry::NAMESPACE
    }

    /// Only `client` is a capability; the type-only packages need no
    /// declaration.
    fn capabilities(&self) -> NamespaceCaps {
        NamespaceCaps {
            prefix: "videre:venue/",
            ifaces: &["client"],
        }
    }

    /// Publishes the registry for the client glue, then adds the import.
    /// The bindgen getter is a plain `fn` pointer and `HostState` no longer
    /// carries a service map, so the registry reaches the glue through the
    /// process-wide slot rather than through the store.
    fn link(&self, linker: &mut Linker<T::State>) -> Result<(), ExtensionError> {
        client::publish_registry(Arc::clone(&self.registry));
        bindings::client::add_to_linker::<HostState<T>, HasSelf<HostState<T>>>(linker, |state| {
            state
        })
        .map_err(|e| ExtensionError::link(VenueRegistry::NAMESPACE, e))
    }

    fn manifest_sections(&self) -> &'static [&'static str] {
        handshake::SECTIONS
    }

    /// The body-version membership predicate: a keeper declaring
    /// `[venue] body_version` boots only when every registered venue's
    /// declared body-version set contains it.
    fn admit_worker(
        &self,
        worker: &str,
        sections: &ExtensionSections,
    ) -> Result<(), ExtensionError> {
        handshake::admit_worker(worker, sections, &self.registry.body_versions())
            .map_err(|e| ExtensionError::admit(worker, e))
    }

    fn emits_trigger_kinds(&self) -> &'static [&'static str] {
        &[INTENT_STATUS_KIND]
    }

    /// The status-poll source, opened only when a module demands the
    /// intent-status kind and a venue is registered.
    fn open_sources(
        &self,
        sources: &mut SourceContext<'_>,
    ) -> Result<Vec<ExtensionSource>, ExtensionError> {
        if !sources
            .demanded_extension_kinds
            .contains(INTENT_STATUS_KIND)
        {
            return Ok(Vec::new());
        }
        let registry = (*self.registry).clone();
        if registry.venue_count() == 0 {
            return Ok(Vec::new());
        }
        let cadence = self.status_poll_interval;
        let (tx, rx) = mpsc::channel::<ExtensionDelivery>(STATUS_CHANNEL_BUF);
        sources.spawn(INTENT_STATUS_KIND, status_poll_task(registry, cadence, tx));
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(vec![Box::pin(stream)])
    }
}

/// Poll loop behind [`Extension::open_sources`]. Sleeps the cadence before
/// each poll; ends when the receiver drops.
async fn status_poll_task(
    registry: VenueRegistry,
    cadence: Duration,
    tx: mpsc::Sender<ExtensionDelivery>,
) {
    loop {
        tokio::time::sleep(cadence).await;
        for update in registry.poll_status_transitions().await {
            let attrs = vec![("venue", update.venue.clone())];
            // The transition rides the generic extension trigger: the
            // envelope is a version tag plus borsh, the status body its
            // inner encoding. A keeper recovers it through
            // `videre_sdk::event`.
            let payload = match update.encode() {
                Ok(payload) => payload,
                Err(err) => {
                    warn!(
                        error = %err,
                        "intent-status envelope failed to encode - dropping transition",
                    );
                    continue;
                }
            };
            let delivery = ExtensionDelivery {
                extension_kind: INTENT_STATUS_KIND,
                attrs,
                trigger: Trigger::Extension(ExtensionTrigger {
                    extension_kind: INTENT_STATUS_KIND.to_owned(),
                    payload,
                }),
            };
            if tx.send(delivery).await.is_err() {
                // Receiver dropped -> engine shutting down.
                return;
            }
        }
    }
}
