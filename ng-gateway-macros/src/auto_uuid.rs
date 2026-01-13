use proc_macro::TokenStream;
use quote::quote;

/// Procedural macro that implements ActiveModelBehavior trait for models
/// with automatic UUID generation for the id field during insertion
pub(crate) fn auto_uuid(_: TokenStream) -> TokenStream {
    let expanded = quote! {
        #[async_trait::async_trait]
        impl sea_orm::ActiveModelBehavior for ActiveModel {
            async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, sea_orm::DbErr>
            where
                C: sea_orm::ConnectionTrait,
            {
                if insert && self.id.is_not_set() {
                    self.id = sea_orm::ActiveValue::Set(uuid::Uuid::new_v4());
                }
                Ok(self)
            }
        }
    };

    TokenStream::from(expanded)
}
