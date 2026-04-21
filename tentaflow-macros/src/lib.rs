// =============================================================================
// Plik: tentaflow-macros/src/lib.rs
// Opis: Proc-macro atrybuty dla handlerow MessageBody:
//         - #[handler(variant = "NodeListRequest", since = (1, 0))]
//         - #[policy(UserSession)]
//         - #[observed]
//       Handler fn = signatura
//         fn name(ctx: &HandlerCtx) -> Result<MessageBody, ProtocolError>
//       Macro generuje static HandlerMeta + inventory::submit! entry.
//       Compile-gate: #[handler] referuje symbole ktore tworza #[policy]
//       i #[observed] → brak atrybutu = E0425 unresolved path.
// =============================================================================

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{parse::Parse, parse::ParseStream, parse_macro_input, Ident, ItemFn, LitInt, LitStr, Token};

// =============================================================================
// #[handler(variant = "NodeListRequest", since = (1, 0))]
// =============================================================================

/// Parsed argumenty `#[handler(variant = "X", since = (major, minor))]`.
struct HandlerArgs {
    variant: String,
    since_major: u8,
    since_minor: u8,
}

impl Parse for HandlerArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut variant: Option<String> = None;
        let mut since: Option<(u8, u8)> = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "variant" => {
                    let s: LitStr = input.parse()?;
                    variant = Some(s.value());
                }
                "since" => {
                    let content;
                    syn::parenthesized!(content in input);
                    let major: LitInt = content.parse()?;
                    content.parse::<Token![,]>()?;
                    let minor: LitInt = content.parse()?;
                    since = Some((major.base10_parse()?, minor.base10_parse()?));
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown key '{}' — expected 'variant' or 'since'", other),
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        let variant = variant
            .ok_or_else(|| syn::Error::new(input.span(), "missing 'variant = \"Name\"'"))?;
        let (major, minor) =
            since.ok_or_else(|| syn::Error::new(input.span(), "missing 'since = (M, m)'"))?;

        Ok(Self {
            variant,
            since_major: major,
            since_minor: minor,
        })
    }
}

/// Rejestruje funkcje jako handler MessageBody variantu.
///
/// Obsluguje zarowno `fn` (sync) jak i `async fn` (async) — macro generuje
/// wrapper `__tentaflow_dispatch_<fn>` ktory zawsze zwraca boxed future
/// o zunifikowanej signaturze `HandlerDispatchFn`. Sync fn sa wolane
/// synchronicznie i owijane w `async move { ... }`; async fn sa po prostu
/// `.await`-owane. Brak `block_on` / `Handle::current`.
///
/// Wymaga jednoczesnie obecnosci `#[policy]` i `#[observed]` na tej samej funkcji —
/// bez nich expansion generuje referencje do nieistniejacych symboli (compile error).
#[proc_macro_attribute]
pub fn handler(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as HandlerArgs);
    let input_fn = parse_macro_input!(item as ItemFn);
    let fn_name = &input_fn.sig.ident;
    let variant = &args.variant;
    let since_major = args.since_major;
    let since_minor = args.since_minor;
    let is_async = input_fn.sig.asyncness.is_some();

    // Symbole generowane przez #[policy] / #[observed]. Ich brak = compile error.
    let policy_marker = format_ident!("__tentaflow_policy_{}", fn_name);
    let observed_marker = format_ident!("__tentaflow_observed_{}", fn_name);
    let registration_fn = format_ident!("__tentaflow_register_{}", fn_name);
    let dispatch_wrapper = format_ident!("__tentaflow_dispatch_{}", fn_name);
    let metric_name = format!("tentaflow_ws_handler_{}", variant);

    // Wewnatrz async bloku wolamy orig fn synchronicznie lub z .await.
    let call_expr = if is_async {
        quote! { #fn_name(__req, __ctx).await }
    } else {
        quote! { #fn_name(__req, __ctx) }
    };

    let output = quote! {
        #input_fn

        // Compile-gate: te dwa identyfikatory powstaja tylko jesli na tej samej
        // fn zaaplikowano odpowiednio #[policy] i #[observed]. Brak = E0425.
        #[allow(non_snake_case, dead_code)]
        const _: () = {
            let _check_policy = #policy_marker;
            let _check_observed = #observed_marker;
        };

        // Zunifikowany wrapper — zawsze zwraca boxed future zgodny z HandlerDispatchFn.
        // Signatura `for<'a> fn(&'a MessageBody, &'a HandlerContext) -> HandlerFuture<'a>`.
        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #dispatch_wrapper<'a>(
            __req: &'a ::tentaflow_protocol::MessageBody,
            __ctx: &'a ::tentaflow_core::dispatch::HandlerContext,
        ) -> ::tentaflow_core::dispatch::HandlerFuture<'a> {
            ::std::boxed::Box::pin(async move { #call_expr })
        }

        // Inventory registration. Wola sie raz przy load, linker zbiera wszystkie entries.
        ::inventory::submit! {
            ::tentaflow_core::dispatch::HandlerMeta {
                variant_name: #variant,
                since_major: #since_major,
                since_minor: #since_minor,
                required_auth: #policy_marker,
                metric_name: #metric_name,
                dispatch_fn: #dispatch_wrapper,
            }
        }

        // Registration fn jest placeholder/ no-op — handler widzialny przez inventory.
        // Trzymamy dla debug/diagnostics.
        #[doc(hidden)]
        #[allow(non_snake_case)]
        pub fn #registration_fn() -> &'static str {
            #variant
        }
    };

    output.into()
}

// =============================================================================
// #[policy(UserSession)]
// =============================================================================

struct PolicyArgs {
    variant: Ident,
}

impl Parse for PolicyArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Akceptujemy: UserSession | ApiKey | Anonymous | MeshTrust
        let variant: Ident = input.parse()?;
        Ok(Self { variant })
    }
}

/// Attach session auth requirement do handler fn.
/// Wartosc jest `SessionAuthKind` enum w tentaflow_core::dispatch.
#[proc_macro_attribute]
pub fn policy(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as PolicyArgs);
    let input_fn = parse_macro_input!(item as ItemFn);
    let fn_name = &input_fn.sig.ident;
    let policy_marker = format_ident!("__tentaflow_policy_{}", fn_name);
    let kind = &args.variant;

    let output = quote! {
        #input_fn

        #[allow(non_upper_case_globals)]
        const #policy_marker: ::tentaflow_core::dispatch::SessionAuthKind =
            ::tentaflow_core::dispatch::SessionAuthKind::#kind;
    };

    output.into()
}

// =============================================================================
// #[observed]
// =============================================================================

/// Stampuje handler metadata ze observation (tracing + metrics) byl dodany.
/// W bootstrap wersji tworzy tylko marker symbol; logika tracingu jest
/// generowana w #[handler] expansion (patrz metric_name w HandlerMeta).
///
/// Compile-gate: #[handler] referuje __tentaflow_observed_{fn_name} → brak = error.
#[proc_macro_attribute]
pub fn observed(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    let fn_name = &input_fn.sig.ident;
    let observed_marker = format_ident!("__tentaflow_observed_{}", fn_name);

    let output = quote! {
        #input_fn

        #[allow(non_upper_case_globals, dead_code)]
        const #observed_marker: bool = true;
    };

    output.into()
}

// =============================================================================
// Unit-like test helper (jesli potrzebny do trybuild w przyszlosci)
// =============================================================================

// NOTE: proc-macro crate nie moze miec #[test] modules directly wolanych z cargo test
// — test expansion logic wymaga trybuild. Zostawiamy poza scope bootstrap.
fn _unused() -> TokenStream2 {
    quote! {}
}
