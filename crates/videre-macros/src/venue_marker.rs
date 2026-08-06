//! The client-side `#[videre_sdk::venue(id = "...", body = Type)]` path:
//! fills a `Venue` marker impl and checks the id against `module.toml`
//! at expansion. No component world, so a keeper linking the client
//! slice never pulls adapter bindgen.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{ImplItem, ItemImpl, LitStr, Token, Type};

/// `id = "cow", body = CowIntentBody`: the venue id and the body schema
/// the marker binds.
struct Args {
    id: LitStr,
    body: Type,
}

impl Parse for Args {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut id = None;
        let mut body = None;
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "id" => id = Some(input.parse()?),
                "body" => body = Some(input.parse()?),
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown argument `{other}`, expected `id` or `body`"),
                    ));
                }
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(Self {
            id: id.ok_or_else(|| syn::Error::new(Span::call_site(), "missing `id = \"...\"`"))?,
            body: body
                .ok_or_else(|| syn::Error::new(Span::call_site(), "missing `body = Type`"))?,
        })
    }
}

/// Expand `#[videre_sdk::venue(id, body)]` on an `impl Venue for Marker
/// {}` block: inject `const ID`/`type Body` from the args and assert the
/// id equals the crate manifest's `[module] name`.
pub fn expand(attr: TokenStream, input: &ItemImpl) -> Result<TokenStream, syn::Error> {
    let args: Args = syn::parse2(attr)?;
    let trait_path = marker_shape(input)?;
    // Read the crate manifest at expansion and hold the id to its
    // registered name, alloy-`sol!`-style. Any mismatch is a
    // compile_error, so the marker and the adapter it types cannot drift.
    let manifest_path = manifest_id_check(&args.id)?;
    Ok(emit(input, trait_path, &args, &manifest_path))
}

/// Emit the filled marker impl. The source body's provided-method
/// overrides are re-emitted verbatim: drop them and a venue's
/// `classify_denied` silently reverts to the coarse default.
fn emit(input: &ItemImpl, trait_path: &syn::Path, args: &Args, manifest_path: &str) -> TokenStream {
    let self_ty = &input.self_ty;
    let id = &args.id;
    let body = &args.body;
    let overrides = &input.items;
    quote! {
        // Rebuild anchor: an edited `[module] name` re-runs the check.
        const _: &[u8] = ::core::include_bytes!(#manifest_path);

        impl #trait_path for #self_ty {
            const ID: ::videre_sdk::client::VenueId =
                ::videre_sdk::client::VenueId::from_static(#id);
            type Body = #body;
            #(#overrides)*
        }
    }
}

/// Check the impl is a non-generic inherent-free `impl Venue for Marker`
/// whose body leaves `ID` and `Body` to the attribute, and return the
/// trait path to re-emit.
fn marker_shape(input: &ItemImpl) -> Result<&syn::Path, syn::Error> {
    let Some((None, trait_path, _)) = &input.trait_ else {
        return Err(syn::Error::new_spanned(
            &input.self_ty,
            "#[videre_sdk::venue(id = ..)] must be applied to an `impl Venue for ...` block",
        ));
    };
    if trait_path
        .segments
        .last()
        .is_none_or(|segment| segment.ident != "Venue")
    {
        return Err(syn::Error::new_spanned(
            trait_path,
            "#[videre_sdk::venue(id = ..)] must be applied to an impl of `videre_sdk::client::Venue`",
        ));
    }
    // The attribute owns `ID` and `Body`; anything else in the body is a
    // provided-method override (`classify_denied`) and passes through.
    for item in &input.items {
        let supplied = match item {
            ImplItem::Const(item) => item.ident == "ID",
            ImplItem::Type(item) => item.ident == "Body",
            _ => false,
        };
        if supplied {
            return Err(syn::Error::new_spanned(
                item,
                "#[videre_sdk::venue(id = ..)] supplies `ID` and `Body`; the impl body may only \
                 override a provided method",
            ));
        }
    }
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[videre_sdk::venue(id = ..)] must be applied to a non-generic impl",
        ));
    }
    Ok(trait_path)
}

/// Assert `id` equals the crate manifest's `[module] name`, returning the
/// manifest path for the rebuild anchor.
fn manifest_id_check(id: &LitStr) -> Result<String, syn::Error> {
    let err = |msg: String| syn::Error::new(id.span(), msg);
    let manifest_path = nexum_world::manifest_dir()
        .map_err(&err)?
        .join("module.toml");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        err(format!(
            "could not read {} ({e}); #[videre_sdk::venue(id = ..)] holds the id to the \
             manifest's [module] name, so the manifest must sit next to Cargo.toml",
            manifest_path.display()
        ))
    })?;
    let name = nexum_world::manifest_name(&text)
        .map_err(|e| err(format!("{}: {e}", manifest_path.display())))?;
    if name != id.value() {
        return Err(err(format!(
            "{}: venue id {:?} disagrees with [module] name {name:?}",
            manifest_path.display(),
            id.value(),
        )));
    }
    Ok(manifest_path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use quote::quote;
    use syn::ItemImpl;

    use super::{Args, emit, marker_shape};

    const OVERRIDING_MARKER: &str = "impl Venue for Marker {
             fn classify_denied(detail: &str) -> RetryAction { RetryAction::DropOnRepeat }
         }";

    fn parse(source: &str) -> ItemImpl {
        syn::parse_str(source).expect("the fixture parses as an impl")
    }

    fn shape_error(source: &str) -> Option<String> {
        marker_shape(&parse(source)).err().map(|e| e.to_string())
    }

    #[test]
    fn a_provided_method_override_passes_the_shape_gate() {
        assert_eq!(shape_error(OVERRIDING_MARKER), None);
    }

    #[test]
    fn a_provided_method_override_reaches_the_emitted_impl() {
        let input = parse(OVERRIDING_MARKER);
        let trait_path = marker_shape(&input).expect("the fixture is a marker impl");
        let args: Args =
            syn::parse2(quote! { id = "marker", body = MarkerBody }).expect("the args parse");

        let emitted = emit(&input, trait_path, &args, "module.toml").to_string();
        assert!(emitted.contains("const ID"), "{emitted}");
        assert!(emitted.contains("type Body = MarkerBody"), "{emitted}");
        assert!(
            emitted.contains("classify_denied") && emitted.contains("DropOnRepeat"),
            "the attribute must not swallow the override: {emitted}",
        );
    }

    #[test]
    fn a_hand_written_id_or_body_is_rejected() {
        assert!(
            shape_error("impl Venue for Marker { const ID: VenueId = X; }")
                .is_some_and(|e| e.contains("supplies `ID` and `Body`")),
        );
        assert!(
            shape_error("impl Venue for Marker { type Body = X; }")
                .is_some_and(|e| e.contains("supplies `ID` and `Body`")),
        );
    }

    #[test]
    fn a_non_venue_impl_is_rejected() {
        assert!(shape_error("impl Other for Marker {}").is_some());
        assert!(shape_error("impl Marker {}").is_some());
        assert!(shape_error("impl<T> Venue for Marker<T> {}").is_some());
    }
}
