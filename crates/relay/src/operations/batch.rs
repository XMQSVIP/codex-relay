//! Register imported accounts with their requested settings.
use super::Operations;
use crate::error::Result;
use uuid::Uuid;
impl Operations {
    pub(crate) fn register_import(
        &self,
        id: Uuid,
        name: String,
        input: super::AccountInput,
        groups: Vec<Uuid>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.settings.clone();
        if groups.is_empty()
            || groups.len() > 64
            || groups
                .iter()
                .any(|id| !next.groups.iter().any(|g| g.id == *id))
            || !(1..=256).contains(&input.max_concurrency)
        {
            return Err(crate::error::Error::new(
                "INVALID_INPUT",
                "导入分组或并发设置无效。",
            ));
        }
        next.accounts.push(super::AccountPolicy {
            id,
            name,
            group_ids: groups,
            enabled: false,
            max_concurrency: input.max_concurrency,
            proxy: Default::default(),
            location: Default::default(),
            route: None,
            deleting: false,
            disabled_features: vec![],
        });
        Self::save(&mut inner, next, input.revision)
    }
}
