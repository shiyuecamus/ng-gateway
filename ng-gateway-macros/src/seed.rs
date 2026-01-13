use darling::{FromAttributes, FromMeta};
use heck::ToSnakeCase;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Error, Result};

/// Common metadata fields for both Seedable and Unseedable
#[derive(FromMeta, Clone, Debug)]
struct CommonMeta {
    name: Option<String>,
    #[darling(default)]
    order: Option<syn::Path>,
    create_table: syn::Path,
    #[darling(default)]
    create_indexes: Option<syn::Path>,
}

/// Attribute arguments for #[seedable(...)]
#[derive(FromMeta, Clone, Debug)]
struct SeedableMeta {
    #[darling(flatten)]
    common: CommonMeta,
    model: syn::Path,
    #[darling(default)]
    seed_data: Option<syn::Path>,
}

/// Attribute arguments for #[unseedable(...)]
#[derive(FromMeta, Clone, Debug)]
struct UnseedableMeta {
    #[darling(flatten)]
    common: CommonMeta,
}

/// Attribute wrapper for #[seedable(...)]
#[derive(FromAttributes, Debug)]
#[darling(attributes(seedable))]
struct SeedableOpts {
    meta: SeedableMeta,
}

/// Attribute wrapper for #[unseedable(...)]
#[derive(FromAttributes, Debug)]
#[darling(attributes(unseedable))]
struct UnseedableOpts {
    meta: UnseedableMeta,
}

/// Helper struct to generate common implementation parts
struct CommonImpl {
    ident: syn::Ident,
    name: String,
    order: TokenStream,
    has_update_col: bool,
    create_table_fn: syn::Path,
    create_indexes_impl: TokenStream,
}

impl CommonImpl {
    /// Create CommonImpl from DeriveInput and CommonMeta
    fn new(input: &DeriveInput, common: &CommonMeta) -> Result<Self> {
        let enum_data = match &input.data {
            Data::Enum(enum_data) => enum_data,
            _ => {
                return Err(Error::new_spanned(
                    input,
                    "This derive macro can only be used with enums",
                ))
            }
        };

        let name = common
            .name
            .clone()
            .unwrap_or_else(|| input.ident.to_string().to_snake_case());

        let order = common
            .order
            .as_ref()
            .map(|p| quote!(#p))
            .unwrap_or_else(|| quote!(0));

        let has_update_col = enum_data.variants.iter().any(|v| v.ident == "UpdatedAt");

        let create_indexes_impl = if let Some(create_fn) = &common.create_indexes {
            quote! {
                #create_fn(backend)
            }
        } else {
            quote! {
                None
            }
        };

        Ok(Self {
            ident: input.ident.clone(),
            name,
            order,
            has_update_col,
            create_table_fn: common.create_table.clone(),
            create_indexes_impl,
        })
    }

    /// Generate the common NGInitializer implementation
    fn generate_ng_initializer(&self, seeding_data_impl: TokenStream) -> TokenStream {
        let Self {
            ident,
            ref name,
            ref order,
            has_update_col,
            ref create_table_fn,
            ref create_indexes_impl,
        } = self;

        quote! {
            #[async_trait::async_trait]
            impl NGInitializer for #ident {
                fn order(&self) -> i32 {
                    #order
                }

                fn name(&self) -> &str {
                    #name
                }

                fn has_update_col(&self) -> bool {
                    #has_update_col
                }

                fn to_create_table_stmt(&self, backend: sea_orm::DatabaseBackend) -> sea_orm::sea_query::TableCreateStatement {
                    #create_table_fn(backend)
                }

                fn to_drop_table_stmt(&self, _: sea_orm::DatabaseBackend) -> sea_orm::sea_query::TableDropStatement {
                    use sea_orm_migration::prelude::*;
                    Table::drop().table(Self::Table).if_exists().to_owned()
                }

                fn to_create_indexes_stmt(
                    &self,
                    backend: sea_orm::DatabaseBackend,
                ) -> Option<Vec<sea_orm::sea_query::IndexCreateStatement>> {
                    #create_indexes_impl
                }

                async fn seeding_data(
                    &self,
                    transaction: &sea_orm::DatabaseTransaction,
                    ctx: &mut InitContext,
                ) -> Result<(), sea_orm::DbErr> {
                    #seeding_data_impl
                }
            }
        }
    }
}

pub(crate) fn expand_derive_seedable_initializer(input: DeriveInput) -> Result<TokenStream> {
    let opts = SeedableOpts::from_attributes(&input.attrs)
        .map_err(|e| Error::new_spanned(&input, e.to_string()))?;

    let common = CommonImpl::new(&input, &opts.meta.common)?;
    let model_type = &opts.meta.model;
    let ident = &input.ident;

    // Generate seeding data implementation for seedable
    let get_seed_data_impl = if let Some(get_fn) = &opts.meta.seed_data {
        quote! {
            #get_fn(ctx).await
        }
    } else {
        quote! {
            Ok(None)
        }
    };

    let seeding_impl = quote! {
        self.seed_data(transaction, ctx).await
    };

    let seedable_specific = quote! {
        impl SeedableInitializerTrait<#model_type> for #ident
        where
            Self: DataSeederTrait<#model_type>,
            #model_type: Clone + SeedableTrait,
        {}

        #[async_trait::async_trait]
        impl DataSeederTrait<#model_type> for #ident {
            async fn get_seed_data(&self, ctx: &mut InitContext) -> Result<Option<Vec<#model_type>>, DbErr> {
                #get_seed_data_impl
            }
        }
    };

    let ng_initializer = common.generate_ng_initializer(seeding_impl);

    Ok(quote! {
        #seedable_specific
        #ng_initializer
    })
}

pub(crate) fn expand_derive_unseedable_initializer(input: DeriveInput) -> Result<TokenStream> {
    let opts = UnseedableOpts::from_attributes(&input.attrs)
        .map_err(|e| Error::new_spanned(&input, e.to_string()))?;

    let common = CommonImpl::new(&input, &opts.meta.common)?;

    // Generate empty seeding data implementation for unseedable
    let seeding_impl = quote! {
        Ok(())
    };

    Ok(common.generate_ng_initializer(seeding_impl))
}
