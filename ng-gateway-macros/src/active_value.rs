use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

/// Implements the IntoActiveValue trait for enums
/// This macro automatically generates the implementation of IntoActiveValue for the target enum
pub(crate) fn derive_into_active_value(input: TokenStream) -> TokenStream {
    // Parse the input tokens into a syntax tree
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    // Generate the implementation
    let expanded = quote! {
        impl sea_orm::IntoActiveValue<#name> for #name {
            fn into_active_value(self) -> sea_orm::ActiveValue<#name> {
                sea_orm::ActiveValue::Set(self)
            }
        }
    };

    // Convert back to token stream and return
    TokenStream::from(expanded)
}
