//! Expansion for `#[keeper]`: the worker mirror of `#[module]`. Same world
//! synthesis and trigger dispatch, with the keeper deltas: the `client`
//! dependency is required, the videre interfaces remap onto the SDK
//! bindings, async handlers complete via `videre_sdk::client::poll_once`,
//! and `ClientError` folds into the wire fault so `?` works in handlers.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{ImplItem, ItemImpl};

/// The handler names recognised on a `#[keeper]` impl. `init`,
/// `on_block`, `on_event`, and `on_schedule` mirror `#[module]`.
/// `on_intent_status` is the videre one: it replaces `#[module]`'s
/// `on_extension`, because a keeper reads the extension trigger as an
/// intent-status transition.
const HANDLERS: [&str; 5] = [
    "init",
    "on_block",
    "on_event",
    "on_schedule",
    "on_intent_status",
];

/// The handler names a keeper declared before the trigger vocabulary
/// replaced the event one, each mapped to its replacement. A keeper still
/// on an old name gets the rename rather than a bare refusal.
const RETIRED_HANDLERS: [(&str, &str); 3] = [
    (
        "on_chain_logs",
        "on_event, which takes one log and not a batch",
    ),
    ("on_tick", "on_schedule"),
    (
        "on_message",
        "nothing, because the host messaging capability is retired",
    ),
];

/// The manifest dependency granting the client import.
const CLIENT_CAPABILITY: &str = "client";

/// The import the `client` dependency must map to.
const CLIENT_IMPORT: &str = "videre:venue/client@0.1.0";

/// WIT packages the client import needs on the resolve path, in
/// dependency order.
const CLIENT_PACKAGES: [&str; 3] = ["videre-value-flow", "videre-types", "videre-venue"];

/// The fault detail for a handler future that suspended.
const SUSPENDED: &str = "keeper handler suspended: guest futures complete in one poll";

/// Expand the handler impl into the keeper module glue.
pub(crate) fn expand(input: &ItemImpl) -> syn::Result<TokenStream> {
    let self_ty = &input.self_ty;
    if !nexum_world::is_plain_type(self_ty) {
        return Err(syn::Error::new_spanned(
            self_ty,
            "#[videre_sdk::keeper] must be applied to an inherent impl of a named type",
        ));
    }
    if let Some((_, trait_path, _)) = &input.trait_ {
        return Err(syn::Error::new_spanned(
            trait_path,
            "#[videre_sdk::keeper] must be applied to an inherent impl, not a trait impl",
        ));
    }
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[videre_sdk::keeper] must be applied to a non-generic impl",
        ));
    }

    // Reserve the `on_` prefix for the recognised handler set, exactly
    // as `#[module]` does: a typo'd handler must not silently no-op.
    for item in &input.items {
        if let ImplItem::Fn(f) = item {
            let name = f.sig.ident.to_string();
            if name.starts_with("on_") && !HANDLERS.contains(&name.as_str()) {
                // A retired name is a rename, so say what replaced it
                // rather than only that the name is unknown.
                let hint = RETIRED_HANDLERS
                    .iter()
                    .find(|(retired, _)| *retired == name)
                    .map_or_else(
                        || "rename helpers so they do not start with `on_`".to_owned(),
                        |(_, replacement)| format!("`{name}` is replaced by {replacement}"),
                    );
                return Err(syn::Error::new_spanned(
                    &f.sig.ident,
                    format!(
                        "`{name}` is not a recognised #[videre_sdk::keeper] handler; expected one \
                         of {HANDLERS:?} ({hint})"
                    ),
                ));
            }
        }
    }

    // Present handlers with their asyncness: async ones are completed
    // on the synchronous guest boundary by the emitted dispatch.
    let present: Vec<(&str, bool)> = input
        .items
        .iter()
        .filter_map(|item| match item {
            ImplItem::Fn(f) => {
                let name = f.sig.ident.to_string();
                HANDLERS
                    .into_iter()
                    .find(|h| *h == name)
                    .map(|h| (h, f.sig.asyncness.is_some()))
            }
            _ => None,
        })
        .collect();
    if present.is_empty() {
        return Err(syn::Error::new_spanned(
            self_ty,
            "#[videre_sdk::keeper] found no recognised handlers on this impl; define at least one \
             of `init`, `on_block`, `on_event`, `on_schedule`, `on_intent_status`",
        ));
    }
    let handler = |name: &str| present.iter().find(|(h, _)| *h == name).copied();

    let (anchors, module_world) = derive_keeper_world()
        .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;
    let wit_paths = nexum_world::manifest_wit_packages(&module_world.packages)
        .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;
    let inline_world = &module_world.wit;
    let adapter_caps: Vec<syn::Ident> = module_world
        .adapters
        .iter()
        .map(|cap| syn::Ident::new(cap, proc_macro2::Span::call_site()))
        .collect();

    // Complete an async handler's future in one poll; a suspension is a
    // typed internal fault, never a hang.
    let drive = |call: TokenStream| {
        quote! {
            match ::videre_sdk::client::poll_once(#call) {
                ::core::task::Poll::Ready(result) => result,
                ::core::task::Poll::Pending => ::core::result::Result::Err(
                    nexum::host::types::Fault::Internal(
                        ::std::string::String::from(#SUSPENDED),
                    ),
                ),
            }
        }
    };

    let init_impl = match handler("init") {
        Some((_, is_async)) => {
            let call = quote! { <#self_ty>::init(config) };
            let body = if is_async { drive(call) } else { call };
            quote! {
                fn init(
                    config: ::std::vec::Vec<(::std::string::String, ::std::string::String)>,
                ) -> ::core::result::Result<(), Fault> {
                    #body
                }
            }
        }
        None => quote! {
            fn init(
                _config: ::std::vec::Vec<(::std::string::String, ::std::string::String)>,
            ) -> ::core::result::Result<(), Fault> {
                ::core::result::Result::Ok(())
            }
        },
    };

    let arm = |name: &str, variant: &str| -> TokenStream {
        let variant = syn::Ident::new(variant, proc_macro2::Span::call_site());
        match handler(name) {
            Some((_, is_async)) => {
                let call = syn::Ident::new(name, proc_macro2::Span::call_site());
                let call = quote! { <#self_ty>::#call(payload) };
                let body = if is_async { drive(call) } else { call };
                quote! { nexum::host::types::Trigger::#variant(payload) => #body, }
            }
            None => quote! {
                nexum::host::types::Trigger::#variant(_) => ::core::result::Result::Ok(()),
            },
        }
    };
    let block_arm = arm("on_block", "Block");
    let event_arm = arm("on_event", "Event");
    let schedule_arm = arm("on_schedule", "Schedule");
    // The intent-status transition rides the generic extension trigger;
    // recover it typed through `videre_sdk::event` and dispatch to the
    // keeper's `on_intent_status` when the kind matches. A malformed
    // payload is the caller's `invalid-input`; a foreign kind belongs to
    // another extension and no-ops.
    let extension_arm = match handler("on_intent_status") {
        Some((_, is_async)) => {
            let call = quote! { <#self_ty>::on_intent_status(update) };
            let body = if is_async { drive(call) } else { call };
            quote! {
                nexum::host::types::Trigger::Extension(payload) => {
                    match ::videre_sdk::event::intent_status_update(
                        &payload.extension_kind,
                        &payload.payload,
                    ) {
                        ::core::option::Option::Some(::core::result::Result::Ok(update)) => #body,
                        ::core::option::Option::Some(::core::result::Result::Err(err)) => {
                            ::core::result::Result::Err(nexum::host::types::Fault::InvalidInput(
                                ::std::string::ToString::to_string(&err),
                            ))
                        }
                        ::core::option::Option::None => ::core::result::Result::Ok(()),
                    }
                }
            }
        }
        None => quote! {
            nexum::host::types::Trigger::Extension(_) => ::core::result::Result::Ok(()),
        },
    };

    Ok(quote! {
        // Anchor a rebuild on the manifest and the extension registry:
        // the emitted world is derived from them.
        #(const _: &[u8] = ::core::include_bytes!(#anchors);)*

        wit_bindgen::generate!({
            inline: #inline_world,
            path: [#(#wit_paths),*],
            world: "nexum:module-world/module",
            generate_all,
            with: {
                "videre:types/types@0.1.0": ::videre_sdk::bindings::videre::types::types,
                "videre:value-flow/types@0.1.0":
                    ::videre_sdk::bindings::videre::value_flow::types,
                "videre:venue/client@0.1.0":
                    ::videre_sdk::bindings::videre::venue::client,
            },
        });

        ::nexum_sdk::bind_host_via_wit_bindgen!(caps: [#(#adapter_caps),*]);

        #input

        // Folds a typed client failure into the wire fault, so `?`
        // applies to client calls inside handlers.
        impl ::core::convert::From<::videre_sdk::ClientError> for nexum::host::types::Fault {
            fn from(err: ::videre_sdk::ClientError) -> Self {
                ::core::convert::Into::into(::nexum_sdk::host::Fault::from(err))
            }
        }

        #[doc(hidden)]
        struct __VidereKeeperExport;

        impl Guest for __VidereKeeperExport {
            #init_impl

            fn on_trigger(
                trigger: nexum::host::types::Trigger,
            ) -> ::core::result::Result<(), Fault> {
                match trigger {
                    #block_arm
                    #event_arm
                    #schedule_arm
                    #extension_arm
                }
            }
        }

        export!(__VidereKeeperExport);
    })
}

/// The canonical `client` extension row, injected when the composition
/// root's registry does not carry one.
fn client_row() -> nexum_world::ExtensionRow {
    nexum_world::ExtensionRow {
        name: CLIENT_CAPABILITY.to_owned(),
        import: CLIENT_IMPORT.to_owned(),
        packages: CLIENT_PACKAGES.map(str::to_owned).into(),
    }
}

/// Read `component.toml`, require the `client` dependency, and synthesize
/// the module world with the client extension row. Returns the rebuild
/// anchor paths and the world.
fn derive_keeper_world() -> Result<(Vec<String>, nexum_world::ModuleWorld), String> {
    let crate_dir = nexum_world::manifest_dir().map_err(|e| e.to_string())?;
    let manifest_path = crate_dir.join(crate::world::MANIFEST_FILE);
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "could not read {} ({e}); #[videre_sdk::keeper] derives the component's WIT world \
             from the manifest's [dependencies] section, so the manifest must sit next to \
             Cargo.toml",
            manifest_path.display()
        )
    })?;
    let declared = nexum_world::manifest_capabilities(&text)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    if !declared.iter().any(|cap| cap == CLIENT_CAPABILITY) {
        return Err(format!(
            "{}: a keeper drives venues through `{CLIENT_IMPORT}`; declare the \
             `{CLIENT_CAPABILITY}` dependency under [dependencies]",
            manifest_path.display()
        ));
    }
    let manifest_path = manifest_path.to_string_lossy().into_owned();

    let mut anchors = vec![manifest_path.clone()];
    let mut extensions = match nexum_world::find_extensions_manifest(&crate_dir) {
        None => Vec::new(),
        Some(registry) => {
            let text = std::fs::read_to_string(&registry)
                .map_err(|e| format!("could not read {}: {e}", registry.display()))?;
            let rows = nexum_world::manifest_extensions(&text)
                .map_err(|e| format!("{}: {e}", registry.display()))?;
            anchors.push(registry.to_string_lossy().into_owned());
            rows
        }
    };
    match extensions.iter().find(|row| row.name == CLIENT_CAPABILITY) {
        None => extensions.push(client_row()),
        Some(row) if row.import == CLIENT_IMPORT => {}
        Some(row) => {
            return Err(format!(
                "the registered `{CLIENT_CAPABILITY}` extension imports `{}`; \
                 #[videre_sdk::keeper] requires `{CLIENT_IMPORT}`",
                row.import
            ));
        }
    }
    let module_world = nexum_world::synthesize(&declared, &extensions)
        .map_err(|e| format!("{manifest_path}: {e}"))?;
    Ok((anchors, module_world))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expand a keeper impl carrying one handler of the given name. The
    /// handler-name check runs before any manifest read, so this reaches
    /// the refusal without a `component.toml` on disk.
    fn refusal(handler: &str) -> String {
        let ident = syn::Ident::new(handler, proc_macro2::Span::call_site());
        let input: ItemImpl = syn::parse_quote! {
            impl Worker {
                fn #ident(_payload: ()) -> Result<(), Fault> { Ok(()) }
            }
        };
        expand(&input)
            .expect_err("an unrecognised handler must refuse")
            .to_string()
    }

    /// Every retired name maps to a live one, so the hint can never point
    /// an author at a name the dispatch does not carry.
    #[test]
    fn retired_handlers_are_disjoint_from_the_live_set() {
        for (retired, _) in RETIRED_HANDLERS {
            assert!(!HANDLERS.contains(&retired), "{retired} is still live");
        }
    }

    #[test]
    fn a_retired_handler_names_its_replacement() {
        assert!(refusal("on_tick").contains("`on_tick` is replaced by on_schedule"));
        let logs = refusal("on_chain_logs");
        assert!(
            logs.contains("replaced by on_event, which takes one log"),
            "message was: {logs}"
        );
        // Messaging is gone with no successor, so the hint says so.
        assert!(refusal("on_message").contains("host messaging capability is retired"));
    }

    #[test]
    fn an_unknown_handler_falls_back_to_the_prefix_advice() {
        let err = refusal("on_nonesuch");
        assert!(err.contains("is not a recognised"), "message was: {err}");
        assert!(
            err.contains("do not start with `on_`"),
            "message was: {err}"
        );
    }
}
