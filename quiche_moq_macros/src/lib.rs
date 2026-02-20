use proc_macro::TokenStream;
use proc_macro2::{Ident, TokenStream as TokenStream2};
use quote::quote;
use syn::{FnArg, ImplItem, ItemImpl, Pat, Type, parse_macro_input};

/// Returns `Some(field_name)` if this param is a trailing connection ref:
/// a param named "wt", "h3", or "quic" whose type is `&mut _::Connection`.
fn conn_field(arg: &FnArg) -> Option<Ident> {
    let FnArg::Typed(pt) = arg else { return None };
    let Pat::Ident(pi) = pt.pat.as_ref() else { return None };
    let name = pi.ident.to_string();
    if !matches!(name.as_str(), "wt" | "h3" | "quic") {
        return None;
    }
    let Type::Reference(tr) = pt.ty.as_ref() else { return None };
    tr.mutability?;
    let Type::Path(tp) = tr.elem.as_ref() else { return None };
    let last = tp.path.segments.last()?;
    if last.ident != "Connection" {
        return None;
    }
    Some(pi.ident.clone())
}

/// Splits params (excluding self) into (business_params, conn_field_names).
/// Trailing params matching `conn_field` are stripped; the rest are business params.
fn split_params(
    inputs: &syn::punctuated::Punctuated<FnArg, syn::token::Comma>,
) -> (Vec<FnArg>, Vec<Ident>) {
    let params: Vec<FnArg> = inputs.iter().skip(1).cloned().collect();
    let split = params
        .iter()
        .rposition(|p| conn_field(p).is_none())
        .map(|i| i + 1)
        .unwrap_or(0);
    let business = params[..split].to_vec();
    let conn: Vec<_> = params[split..]
        .iter()
        .filter_map(conn_field)
        .collect();
    (business, conn)
}

/// Extracts the bare identifier from a function parameter pattern (drops `mut`).
fn param_ident(arg: &FnArg) -> Option<&Ident> {
    let FnArg::Typed(pt) = arg else { return None };
    let Pat::Ident(pi) = pt.pat.as_ref() else { return None };
    Some(&pi.ident)
}

#[proc_macro_attribute]
pub fn generate_moq_handle(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemImpl);

    let mut methods: Vec<TokenStream2> = Vec::new();

    for impl_item in &input.items {
        let ImplItem::Fn(method) = impl_item else { continue };

        // Only fully public methods
        if !matches!(method.vis, syn::Visibility::Public(_)) {
            continue;
        }

        let sig = &method.sig;

        // First param must be a self receiver (&self or &mut self)
        let Some(first) = sig.inputs.first() else { continue };
        let FnArg::Receiver(recv) = first else { continue };
        let self_tok = if recv.mutability.is_some() {
            quote! { &mut self }
        } else {
            quote! { &self }
        };

        let (business_params, conn_fields) = split_params(&sig.inputs);

        let name = &sig.ident;
        let generics = &sig.generics;
        let where_clause = &sig.generics.where_clause;
        let ret = &sig.output;

        // Business arg names for the forwarding call
        let business_args: Vec<_> = business_params
            .iter()
            .filter_map(param_ident)
            .collect();

        // Connection args: self.wt, self.h3, self.quic (in original param order)
        let conn_args: Vec<TokenStream2> = conn_fields
            .iter()
            .map(|f| quote! { self.#f })
            .collect();

        methods.push(quote! {
            pub fn #name #generics (#self_tok, #(#business_params),*) #ret #where_clause {
                self.session.#name(#(#business_args,)* #(#conn_args),*)
            }
        });
    }

    quote! {
        #input

        #[doc(hidden)]
        #[macro_export]
        macro_rules! moq_handle_impl {
            () => {
                #(#methods)*
            }
        }
    }
    .into()
}
