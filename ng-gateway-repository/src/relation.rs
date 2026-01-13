use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::{
    domain::prelude::{RelationDelete, RelationInfo, RelationQuery},
    entities::prelude::{Relation, RelationActiveModel, RelationColumn},
    enums::{
        common::{EntityType, ToFromType},
        relation::RelationType,
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QuerySelect, QueryTrait,
};

pub struct RelationRepository;

impl RelationRepository {
    pub async fn create<C>(relation: RelationActiveModel, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let _ = relation.insert(conn).await?;
            }
            None => {
                let db = get_db_connection().await?;
                let _ = relation.insert(&db).await?;
            }
        }
        Ok(())
    }

    pub async fn batch_create<C>(
        relations: Vec<RelationActiveModel>,
        db: Option<&C>,
    ) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let _ = Relation::insert_many(relations).exec(conn).await?;
            }
            None => {
                let db = get_db_connection().await?;
                let _ = Relation::insert_many(relations).exec(&db).await?;
            }
        }
        Ok(())
    }

    pub async fn batch_delete<C>(query: RelationDelete, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let _ = Relation::delete_many()
                    .apply_if(query.from_id, |q, from_id| {
                        q.filter(RelationColumn::FromId.eq(from_id))
                    })
                    .apply_if(query.from_type, |q, from_type| {
                        q.filter(RelationColumn::FromType.eq(from_type))
                    })
                    .apply_if(query.to_id, |q, to_id| {
                        q.filter(RelationColumn::ToId.eq(to_id))
                    })
                    .apply_if(query.to_type, |q, to_type| {
                        q.filter(RelationColumn::ToType.eq(to_type))
                    })
                    .apply_if(query.r#type, |q, r#type| {
                        q.filter(RelationColumn::Type.eq(r#type))
                    })
                    .exec(conn)
                    .await?;
            }
            None => {
                let db = get_db_connection().await?;
                let _ = Relation::delete_many()
                    .apply_if(query.from_id, |q, from_id| {
                        q.filter(RelationColumn::FromId.eq(from_id))
                    })
                    .apply_if(query.from_type, |q, from_type| {
                        q.filter(RelationColumn::FromType.eq(from_type))
                    })
                    .apply_if(query.to_id, |q, to_id| {
                        q.filter(RelationColumn::ToId.eq(to_id))
                    })
                    .apply_if(query.to_type, |q, to_type| {
                        q.filter(RelationColumn::ToType.eq(to_type))
                    })
                    .apply_if(query.r#type, |q, r#type| {
                        q.filter(RelationColumn::Type.eq(r#type))
                    })
                    .exec(&db)
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn find_ids_by(
        to_from_type: ToFromType,
        id: i32,
        id_type: EntityType,
        relation_entity_type: EntityType,
        relation_type: Option<RelationType>,
    ) -> StorageResult<Vec<i32>> {
        let db = get_db_connection().await?;
        let mut query = Relation::find();
        use sea_orm::{DeriveColumn, EnumIter};
        #[derive(Copy, Clone, Debug, EnumIter, DeriveColumn)]
        enum QueryAs {
            ToId,
            FromId,
        }
        match to_from_type {
            ToFromType::To => {
                query = query
                    .select_only()
                    .column_as(RelationColumn::ToId, QueryAs::ToId)
                    .filter(RelationColumn::FromId.eq(id))
                    .filter(RelationColumn::FromType.eq(id_type))
                    .filter(RelationColumn::ToType.eq(relation_entity_type))
                    .apply_if(relation_type, |q, relation_type| {
                        q.filter(RelationColumn::Type.eq(relation_type))
                    });
            }
            ToFromType::From => {
                query = query
                    .select_only()
                    .column_as(RelationColumn::FromId, QueryAs::FromId)
                    .filter(RelationColumn::ToId.eq(id))
                    .filter(RelationColumn::ToType.eq(id_type))
                    .filter(RelationColumn::FromType.eq(relation_entity_type))
                    .apply_if(relation_type, |q, relation_type| {
                        q.filter(RelationColumn::Type.eq(relation_type))
                    });
            }
        }
        Ok(query.into_values::<_, QueryAs>().all(&db).await?)
    }

    pub async fn find_one_by_query(query: RelationQuery) -> StorageResult<Option<RelationInfo>> {
        let db = get_db_connection().await?;
        Ok(Relation::find()
            .apply_if(query.from_id, |q, from_id| {
                q.filter(RelationColumn::FromId.eq(from_id))
            })
            .apply_if(query.from_type, |q, from_type| {
                q.filter(RelationColumn::FromType.eq(from_type))
            })
            .apply_if(query.to_id, |q, to_id| {
                q.filter(RelationColumn::ToId.eq(to_id))
            })
            .apply_if(query.to_type, |q, to_type| {
                q.filter(RelationColumn::ToType.eq(to_type))
            })
            .into_partial_model::<RelationInfo>()
            .one(&db)
            .await?)
    }

    pub async fn find_by_query(query: RelationQuery) -> StorageResult<Vec<RelationInfo>> {
        let db = get_db_connection().await?;
        Ok(Relation::find()
            .apply_if(query.from_id, |q, from_id| {
                q.filter(RelationColumn::FromId.eq(from_id))
            })
            .apply_if(query.from_type, |q, from_type| {
                q.filter(RelationColumn::FromType.eq(from_type))
            })
            .apply_if(query.to_id, |q, to_id| {
                q.filter(RelationColumn::ToId.eq(to_id))
            })
            .apply_if(query.to_type, |q, to_type| {
                q.filter(RelationColumn::ToType.eq(to_type))
            })
            .into_partial_model::<RelationInfo>()
            .all(&db)
            .await?)
    }

    pub async fn count_by_query(query: RelationQuery) -> StorageResult<u64> {
        let db = get_db_connection().await?;
        Ok(Relation::find()
            .apply_if(query.from_id, |q, from_id| {
                q.filter(RelationColumn::FromId.eq(from_id))
            })
            .apply_if(query.from_type, |q, from_type| {
                q.filter(RelationColumn::FromType.eq(from_type))
            })
            .apply_if(query.to_id, |q, to_id| {
                q.filter(RelationColumn::ToId.eq(to_id))
            })
            .apply_if(query.to_type, |q, to_type| {
                q.filter(RelationColumn::ToType.eq(to_type))
            })
            .count(&db)
            .await?)
    }
}
