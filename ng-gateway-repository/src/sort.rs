//! Lightweight helpers for applying [`SortParams`] to SeaORM queries.
//!
//! Each resource repository maintains its own field-name whitelist and
//! calls `resolve_sort` to translate the external `sortBy` value into a
//! concrete SeaORM column. This module only provides shared primitives;
//! the mapping itself stays in each repository for type safety.

use ng_gateway_models::domain::prelude::SortParams;
use sea_orm::{Order, QueryOrder, Select};

/// Apply a primary sort column and a stable tie-breaker to a SeaORM `Select`.
///
/// The tie-breaker (`id_col`) is always appended as `ASC` to guarantee
/// deterministic pagination when the primary column contains duplicates.
pub fn apply_sort_with_tiebreaker<E, C>(
    query: Select<E>,
    primary_col: C,
    order: Order,
    id_col: C,
) -> Select<E>
where
    E: sea_orm::EntityTrait,
    C: sea_orm::ColumnTrait + Copy,
{
    query.order_by(primary_col, order).order_by_asc(id_col)
}

/// Resolve the effective [`Order`] from [`SortParams`], defaulting to
/// `default_order` when the caller omits `sortOrder`.
#[inline]
pub fn effective_order(sort: &SortParams, default_order: Order) -> Order {
    sort.sort_order
        .map(|o| o.into_sea_order())
        .unwrap_or(default_order)
}
