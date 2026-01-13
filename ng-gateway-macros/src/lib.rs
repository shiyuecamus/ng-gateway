mod active_value;
mod auto_uuid;
mod event;
mod seed;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

/// Derives SeedableInitializer trait implementation for structs
///
/// This macro automatically implements the SeedableInitializer trait for the target struct
#[proc_macro_derive(SeedableInitializer, attributes(seedable))]
pub fn derive_seedable_initializer(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match seed::expand_derive_seedable_initializer(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derives UnseedableInitializer trait implementation for structs
///
/// This macro automatically implements the UnseedableInitializer trait for the target struct
#[proc_macro_derive(UnseedableInitializer, attributes(unseedable))]
pub fn derive_unseedable_initializer(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match seed::expand_derive_unseedable_initializer(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derives IntoActiveValue trait implementation for enums
///
/// This macro automatically implements the IntoActiveValue trait for enums
/// that are used with sea-orm.
#[proc_macro_derive(IntoActiveValue)]
pub fn derive_into_active_value(input: TokenStream) -> TokenStream {
    active_value::derive_into_active_value(input)
}

/// Derives Event trait implementation for structs
///
/// This macro automatically implements the NGEvent trait for the target struct
#[proc_macro_derive(Event)]
pub fn derive_ng_event(input: TokenStream) -> TokenStream {
    event::derive_ng_event(input)
}

/// Automatically implements ActiveModelBehavior for sea-orm models
/// with UUID generation for the id field
///
/// This macro generates the boilerplate code for automatic UUID generation
/// during entity creation
#[proc_macro_derive(AutoUuid)]
pub fn auto_uuid(input: TokenStream) -> TokenStream {
    auto_uuid::auto_uuid(input)
}
