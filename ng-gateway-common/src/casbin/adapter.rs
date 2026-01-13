use async_trait::async_trait;
use casbin::error::AdapterError;
use casbin::{Adapter, Filter, Model};
use ng_gateway_models::{
    domain::prelude::NewCasbin,
    entities::prelude::{Casbin, CasbinColumn, CasbinModel},
};
use sea_orm::{
    ColumnTrait, Condition, DatabaseConnection, EntityTrait, IntoActiveModel, PaginatorTrait,
    QueryFilter, TransactionTrait,
};

type Result<T> = std::result::Result<T, casbin::Error>;

/// Casbin adapter for database operations
#[derive(Clone, Debug)]
pub struct NGCasbinDBAdapter {
    is_filtered: bool,
    db: DatabaseConnection,
}

impl NGCasbinDBAdapter {
    pub fn new(db: DatabaseConnection) -> Self {
        Self {
            is_filtered: false,
            db,
        }
    }

    /// Convert a CasbinModel to a policy rule
    fn to_policy_rule(&self, casbin: &CasbinModel) -> Option<Vec<String>> {
        // Skip if ptype is empty
        if casbin.ptype.is_none() || casbin.ptype.as_deref().unwrap_or("").is_empty() {
            return None;
        }

        // Collect all non-empty values into array for better performance
        let values = [
            casbin.v0.as_deref(),
            casbin.v1.as_deref(),
            casbin.v2.as_deref(),
            casbin.v3.as_deref(),
            casbin.v4.as_deref(),
            casbin.v5.as_deref(),
        ];

        // Use itertools for better performance on iterator chaining
        let rule: Vec<String> = values
            .into_iter()
            .take_while(Option::is_some)
            .flatten()
            .take_while(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect();

        (!rule.is_empty()).then_some(rule)
    }

    /// Add rule to model if valid
    fn add_rule_to_model(
        &self,
        model: &mut dyn Model,
        ptype: &str,
        rule: Vec<String>,
    ) -> Result<()> {
        // Get first character of ptype as section
        let sec = ptype
            .chars()
            .next()
            .map(String::from)
            .ok_or(Self::to_adapter_error(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid ptype",
            )))?;

        // Add rule to model if section and ptype exist
        if let Some(t1) = model.get_mut_model().get_mut(&sec) {
            if let Some(t2) = t1.get_mut(ptype) {
                t2.get_mut_policy().insert(rule);
            }
        }

        Ok(())
    }

    async fn to_filtered_policy(&self, filter: &Filter<'_>) -> Result<Vec<CasbinModel>> {
        let (g_filter, p_filter) = self.filtered_where_values(filter);
        let casbin_rule = Casbin::find()
            .filter(
                Condition::any()
                    .add(
                        Condition::all()
                            .add(CasbinColumn::Ptype.like("p%"))
                            .add(CasbinColumn::V0.contains(g_filter[0]))
                            .add(CasbinColumn::V1.contains(g_filter[1]))
                            .add(CasbinColumn::V2.contains(g_filter[2]))
                            .add(CasbinColumn::V3.contains(g_filter[3]))
                            .add(CasbinColumn::V4.contains(g_filter[4]))
                            .add(CasbinColumn::V5.contains(g_filter[5])),
                    )
                    .add(
                        Condition::all()
                            .add(CasbinColumn::Ptype.like("g%"))
                            .add(CasbinColumn::V0.contains(p_filter[0]))
                            .add(CasbinColumn::V1.contains(p_filter[1]))
                            .add(CasbinColumn::V2.contains(p_filter[2]))
                            .add(CasbinColumn::V3.contains(p_filter[3]))
                            .add(CasbinColumn::V4.contains(p_filter[4]))
                            .add(CasbinColumn::V5.contains(p_filter[5])),
                    ),
            )
            .all(&self.db)
            .await
            .map_err(Self::to_adapter_error)?;
        Ok(casbin_rule)
    }

    /// get filter values
    fn filtered_where_values<'b>(&self, filter: &Filter<'b>) -> ([&'b str; 6], [&'b str; 6]) {
        let mut g_filter: [&'b str; 6] = ["%", "%", "%", "%", "%", "%"];
        let mut p_filter: [&'b str; 6] = ["%", "%", "%", "%", "%", "%"];
        for (idx, val) in filter.g.iter().enumerate() {
            if val != &"" {
                g_filter[idx] = val;
            }
        }
        for (idx, val) in filter.p.iter().enumerate() {
            if val != &"" {
                p_filter[idx] = val;
            }
        }
        (g_filter, p_filter)
    }

    /// Convert database error to casbin error
    fn to_adapter_error<E: std::error::Error + Send + Sync + 'static>(err: E) -> casbin::Error {
        casbin::Error::from(AdapterError(Box::new(err)))
    }

    /// Convert a CasbinModel to a database record
    fn to_casbin_rule(&self, ptype: &str, rule: &[String]) -> NewCasbin {
        let mut model = NewCasbin {
            ptype: Some(ptype.to_string()),
            v0: None,
            v1: None,
            v2: None,
            v3: None,
            v4: None,
            v5: None,
        };

        // Efficiently assign values
        for (i, v) in rule.iter().enumerate() {
            match i {
                0 => model.v0 = Some(v.to_string()),
                1 => model.v1 = Some(v.to_string()),
                2 => model.v2 = Some(v.to_string()),
                3 => model.v3 = Some(v.to_string()),
                4 => model.v4 = Some(v.to_string()),
                5 => model.v5 = Some(v.to_string()),
                _ => break,
            }
        }
        model
    }

    async fn save_policies(&self, rules: Vec<NewCasbin>) -> Result<()> {
        let txn = self.db.begin().await.map_err(Self::to_adapter_error)?;
        for rule in rules {
            let s = Casbin::find()
                .filter(CasbinColumn::Ptype.eq(rule.ptype.as_deref().unwrap_or("")))
                .count(&txn)
                .await
                .map_err(Self::to_adapter_error)?;

            if s > 0 {
                continue;
            }

            Casbin::insert(rule.into_active_model())
                .exec(&txn)
                .await
                .map_err(Self::to_adapter_error)?;
        }
        txn.commit().await.map_err(Self::to_adapter_error)?;
        Ok(())
    }
}

#[async_trait]
impl Adapter for NGCasbinDBAdapter {
    async fn load_policy(&mut self, m: &mut dyn Model) -> Result<()> {
        let rules = Casbin::find()
            .all(&self.db)
            .await
            .map_err(Self::to_adapter_error)?;

        for rule in &rules {
            if let Some(policy) = self.to_policy_rule(rule) {
                if let Some(ptype) = &rule.ptype {
                    self.add_rule_to_model(m, ptype, policy)?;
                }
            }
        }

        Ok(())
    }

    async fn load_filtered_policy<'b>(
        &mut self,
        m: &mut dyn Model,
        f: Filter<'b>,
    ) -> casbin::Result<()> {
        let rules = self.to_filtered_policy(&f).await?;
        self.is_filtered = true;

        for casbin_rule in &rules {
            if let Some(policy) = self.to_policy_rule(casbin_rule) {
                let ptype = casbin_rule
                    .ptype
                    .as_ref()
                    .expect("Policy type should not be empty");
                if let Some(ref sec) = ptype.chars().next().map(String::from) {
                    if let Some(t1) = m.get_mut_model().get_mut(sec) {
                        if let Some(t2) = t1.get_mut(ptype) {
                            t2.get_mut_policy().insert(policy);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn save_policy(&mut self, m: &mut dyn Model) -> Result<()> {
        // Collect all policies
        let mut rules = Vec::new();

        if let Some(ast_map) = m.get_model().get("p") {
            for (ptype, ast) in ast_map {
                for policy in ast.get_policy() {
                    rules.push(self.to_casbin_rule(ptype, policy));
                }
            }
        }

        if let Some(ast_map) = m.get_model().get("g") {
            for (ptype, ast) in ast_map {
                for policy in ast.get_policy() {
                    rules.push(self.to_casbin_rule(ptype, policy));
                }
            }
        }

        if rules.is_empty() {
            return Ok(());
        }

        self.save_policies(rules).await
    }

    async fn clear_policy(&mut self) -> Result<()> {
        Casbin::delete_many()
            .exec(&self.db)
            .await
            .map_err(Self::to_adapter_error)?;
        Ok(())
    }

    fn is_filtered(&self) -> bool {
        self.is_filtered
    }

    async fn add_policy(&mut self, _sec: &str, ptype: &str, rule: Vec<String>) -> Result<bool> {
        let casbin_rule = self.to_casbin_rule(ptype, &rule);
        self.save_policies(vec![casbin_rule]).await?;

        Ok(true)
    }

    async fn add_policies(
        &mut self,
        _sec: &str,
        ptype: &str,
        rules: Vec<Vec<String>>,
    ) -> Result<bool> {
        let casbin_rules: Vec<_> = rules
            .iter()
            .map(|rule| self.to_casbin_rule(ptype, rule))
            .collect();

        if !casbin_rules.is_empty() {
            self.save_policies(casbin_rules).await?;
        }

        Ok(true)
    }

    async fn remove_policy(&mut self, _sec: &str, ptype: &str, rule: Vec<String>) -> Result<bool> {
        let mut condition = Condition::all().add(CasbinColumn::Ptype.eq(ptype));

        // Build condition dynamically based on rule length
        for (i, v) in rule.iter().enumerate() {
            match i {
                0 => condition = condition.add(CasbinColumn::V0.eq(v)),
                1 => condition = condition.add(CasbinColumn::V1.eq(v)),
                2 => condition = condition.add(CasbinColumn::V2.eq(v)),
                3 => condition = condition.add(CasbinColumn::V3.eq(v)),
                4 => condition = condition.add(CasbinColumn::V4.eq(v)),
                5 => condition = condition.add(CasbinColumn::V5.eq(v)),
                _ => break,
            }
        }

        let result = Casbin::delete_many()
            .filter(condition)
            .exec(&self.db)
            .await
            .map_err(Self::to_adapter_error)?;

        Ok(result.rows_affected > 0)
    }

    async fn remove_policies(
        &mut self,
        sec: &str,
        ptype: &str,
        rules: Vec<Vec<String>>,
    ) -> Result<bool> {
        let mut affected = false;
        for rule in rules {
            if self.remove_policy(sec, ptype, rule).await? {
                affected = true;
            }
        }
        Ok(affected)
    }

    async fn remove_filtered_policy(
        &mut self,
        _sec: &str,
        ptype: &str,
        field_index: usize,
        field_values: Vec<String>,
    ) -> Result<bool> {
        let mut condition = Condition::all().add(CasbinColumn::Ptype.eq(ptype));

        // Build condition dynamically based on field_index and values
        for (i, v) in field_values.iter().enumerate() {
            if !v.is_empty() {
                match field_index + i {
                    0 => condition = condition.add(CasbinColumn::V0.eq(v)),
                    1 => condition = condition.add(CasbinColumn::V1.eq(v)),
                    2 => condition = condition.add(CasbinColumn::V2.eq(v)),
                    3 => condition = condition.add(CasbinColumn::V3.eq(v)),
                    4 => condition = condition.add(CasbinColumn::V4.eq(v)),
                    5 => condition = condition.add(CasbinColumn::V5.eq(v)),
                    _ => break,
                }
            }
        }

        let result = Casbin::delete_many()
            .filter(condition)
            .exec(&self.db)
            .await
            .map_err(Self::to_adapter_error)?;

        Ok(result.rows_affected > 0)
    }
}
