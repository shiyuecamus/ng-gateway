use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

/// Derives NGEvent trait implementation for structs
///
/// This macro automatically implements the NGEvent trait for the target struct
pub fn derive_ng_event(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let expanded = quote! {
        impl NGEvent for #name {}
    };

    TokenStream::from(expanded)
}
