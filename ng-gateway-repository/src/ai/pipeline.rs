use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::domain::prelude::{PageResult, PipelineInfo, PipelinePageParams};
use ng_gateway_models::entities::prelude::{
    AlarmRule, AlarmRuleColumn, AlarmRuleModel, Pipeline, PipelineActiveModel, PipelineColumn,
    PipelineModel, PipelineStage, PipelineStageColumn, PipelineStageModel,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QueryTrait,
};
use std::collections::HashMap;

/// Repository for AI pipeline persistence.
pub struct PipelineRepository;

impl PipelineRepository {
    /// Find one pipeline row by unique `pipeline_key`.
    pub async fn find_by_key(pipeline_key: &str) -> StorageResult<Option<PipelineModel>> {
        let db = get_db_connection().await?;
        Ok(Pipeline::find()
            .filter(PipelineColumn::Key.eq(pipeline_key))
            .one(&db)
            .await?)
    }

    /// List all pipeline rows.
    pub async fn list_all() -> StorageResult<Vec<PipelineModel>> {
        let db = get_db_connection().await?;
        Ok(Pipeline::find()
            .order_by_asc(PipelineColumn::Id)
            .all(&db)
            .await?)
    }

    /// Paginate pipeline rows with optional filters.
    pub async fn page(params: PipelinePageParams) -> StorageResult<PageResult<PipelineInfo>> {
        let db = get_db_connection().await?;
        let base = Pipeline::find()
            .apply_if(params.name.as_ref(), |q, name| {
                q.filter(PipelineColumn::Name.like(format!("%{name}%")))
            })
            .apply_if(params.status, |q, status| {
                q.filter(PipelineColumn::Status.eq(status))
            })
            .apply_if(params.time_range.start_time, |q, start_time| {
                q.filter(PipelineColumn::CreatedAt.gte(start_time.naive_utc()))
            })
            .apply_if(params.time_range.end_time, |q, end_time| {
                q.filter(PipelineColumn::CreatedAt.lte(end_time.naive_utc()))
            })
            .order_by_asc(PipelineColumn::Id);
        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let paginator = base
            .into_partial_model::<PipelineInfo>()
            .paginate(&db, page_size);
        let total = paginator.num_items().await?;
        let mut records = paginator.fetch_page(page.saturating_sub(1)).await?;

        // Batch load page-scope stage/rule details to avoid N+1 queries.
        let pipeline_ids: Vec<i32> = records.iter().map(|pipeline| pipeline.id).collect();
        if !pipeline_ids.is_empty() {
            let stage_rows = PipelineStage::find()
                .filter(PipelineStageColumn::PipelineId.is_in(pipeline_ids.iter().copied()))
                .order_by_asc(PipelineStageColumn::PipelineId)
                .order_by_asc(PipelineStageColumn::StageOrder)
                .all(&db)
                .await?;
            let rule_rows = AlarmRule::find()
                .filter(AlarmRuleColumn::PipelineId.is_in(pipeline_ids.iter().copied()))
                .order_by_asc(AlarmRuleColumn::PipelineId)
                .order_by_asc(AlarmRuleColumn::RuleOrder)
                .all(&db)
                .await?;

            let mut stages_by_pipeline: HashMap<i32, Vec<_>> =
                HashMap::with_capacity(pipeline_ids.len());
            for stage in stage_rows {
                stages_by_pipeline
                    .entry(stage.pipeline_id)
                    .or_default()
                    .push(stage.into());
            }

            let mut rules_by_pipeline: HashMap<i32, Vec<_>> =
                HashMap::with_capacity(pipeline_ids.len());
            for rule in rule_rows {
                rules_by_pipeline
                    .entry(rule.pipeline_id)
                    .or_default()
                    .push(rule.into());
            }

            for pipeline in &mut records {
                pipeline.stages = stages_by_pipeline.remove(&pipeline.id).unwrap_or_default();
                pipeline.alarm_rules = rules_by_pipeline.remove(&pipeline.id).unwrap_or_default();
            }
        }

        Ok(PageResult {
            records,
            total,
            pages: if page_size > 0 {
                total.div_ceil(page_size)
            } else {
                0
            },
            page,
            page_size,
        })
    }

    /// Insert one pipeline row.
    pub async fn create<C>(
        pipeline: PipelineActiveModel,
        db: Option<&C>,
    ) -> StorageResult<PipelineModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(pipeline.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(pipeline.insert(&conn).await?)
            }
        }
    }

    /// Update one pipeline row.
    pub async fn update<C>(
        pipeline: PipelineActiveModel,
        db: Option<&C>,
    ) -> StorageResult<PipelineModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(pipeline.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(pipeline.update(&conn).await?)
            }
        }
    }

    /// Delete one pipeline row by `pipeline_key`.
    pub async fn delete_by_key<C>(pipeline_key: &str, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Pipeline::delete_many()
                    .filter(PipelineColumn::Key.eq(pipeline_key))
                    .exec(conn)
                    .await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Pipeline::delete_many()
                    .filter(PipelineColumn::Key.eq(pipeline_key))
                    .exec(&conn)
                    .await?;
            }
        }
        Ok(())
    }

    /// Find one pipeline by primary key.
    pub async fn find_by_id(pipeline_id: i32) -> StorageResult<Option<PipelineModel>> {
        let db = get_db_connection().await?;
        Ok(Pipeline::find_by_id(pipeline_id).one(&db).await?)
    }

    /// Delete one pipeline row by primary key.
    pub async fn delete_by_id(pipeline_id: i32) -> StorageResult<()> {
        let db = get_db_connection().await?;
        Pipeline::delete_many()
            .filter(PipelineColumn::Id.eq(pipeline_id))
            .exec(&db)
            .await?;
        Ok(())
    }

    /// Load all pipelines with their stages and alarm rules in 3 queries.
    ///
    /// Uses batch-load pattern (same as `page()`) instead of N+1 per-pipeline
    /// queries, returning `(pipeline, stages, rules)` tuples ready for
    /// `PipelineInfo::with_relations`.
    pub async fn list_all_with_relations<C>(
        db: Option<&C>,
    ) -> StorageResult<Vec<(PipelineModel, Vec<PipelineStageModel>, Vec<AlarmRuleModel>)>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Self::list_all_with_relations_inner(conn).await,
            None => {
                let conn = get_db_connection().await?;
                Self::list_all_with_relations_inner(&conn).await
            }
        }
    }

    async fn list_all_with_relations_inner<C: ConnectionTrait>(
        db: &C,
    ) -> StorageResult<Vec<(PipelineModel, Vec<PipelineStageModel>, Vec<AlarmRuleModel>)>> {
        let pipelines = Pipeline::find()
            .order_by_asc(PipelineColumn::Id)
            .all(db)
            .await?;

        if pipelines.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<i32> = pipelines.iter().map(|p| p.id).collect();

        let stage_rows = PipelineStage::find()
            .filter(PipelineStageColumn::PipelineId.is_in(ids.iter().copied()))
            .order_by_asc(PipelineStageColumn::PipelineId)
            .order_by_asc(PipelineStageColumn::StageOrder)
            .all(db)
            .await?;

        let rule_rows = AlarmRule::find()
            .filter(AlarmRuleColumn::PipelineId.is_in(ids.iter().copied()))
            .order_by_asc(AlarmRuleColumn::PipelineId)
            .order_by_asc(AlarmRuleColumn::RuleOrder)
            .all(db)
            .await?;

        let mut stages_map: HashMap<i32, Vec<PipelineStageModel>> =
            HashMap::with_capacity(ids.len());
        for s in stage_rows {
            stages_map.entry(s.pipeline_id).or_default().push(s);
        }

        let mut rules_map: HashMap<i32, Vec<AlarmRuleModel>> = HashMap::with_capacity(ids.len());
        for r in rule_rows {
            rules_map.entry(r.pipeline_id).or_default().push(r);
        }

        Ok(pipelines
            .into_iter()
            .map(|p| {
                let pid = p.id;
                let stages = stages_map.remove(&pid).unwrap_or_default();
                let rules = rules_map.remove(&pid).unwrap_or_default();
                (p, stages, rules)
            })
            .collect())
    }

    /// Load one pipeline with all relations by primary key.
    ///
    /// Issues 3 parallel-safe queries: pipeline + stages + rules.
    pub async fn find_by_id_with_relations(
        pipeline_id: i32,
    ) -> StorageResult<Option<(PipelineModel, Vec<PipelineStageModel>, Vec<AlarmRuleModel>)>> {
        let db = get_db_connection().await?;
        let pipeline = Pipeline::find_by_id(pipeline_id).one(&db).await?;

        match pipeline {
            None => Ok(None),
            Some(p) => {
                let stages = PipelineStage::find()
                    .filter(PipelineStageColumn::PipelineId.eq(pipeline_id))
                    .order_by_asc(PipelineStageColumn::StageOrder)
                    .all(&db)
                    .await?;
                let rules = AlarmRule::find()
                    .filter(AlarmRuleColumn::PipelineId.eq(pipeline_id))
                    .order_by_asc(AlarmRuleColumn::RuleOrder)
                    .all(&db)
                    .await?;
                Ok(Some((p, stages, rules)))
            }
        }
    }
}
