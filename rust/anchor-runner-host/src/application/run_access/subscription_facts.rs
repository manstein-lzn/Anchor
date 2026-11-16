//! Schedule sharing and subscription relationships use the existing Kernel writer.
use super::{ApplicationError, RunApplication};
use anchor_kernel::Store;
use anchor_runtime::graph::scheduler::kernel_error;
use serde_json::Value;

pub(super) const VALIDATOR: &str = "anchor/schedule-subscriptions";

impl RunApplication {
    pub(in crate::application) fn subscription_fact(
        &self,
        id: &str,
    ) -> Result<Option<Value>, ApplicationError> {
        let Some(runs) = self.kernel_runs(false)? else {
            return Ok(None);
        };
        Ok(runs
            .kernel
            .load(&format!("schedule-subscriptions/{id}"))
            .map_err(kernel_error)?
            .filter(|resource| resource.validator == VALIDATOR && !resource.state.is_null())
            .map(|resource| resource.state))
    }

    pub(in crate::application) fn subscription_facts(
        &self,
        prefix: &str,
    ) -> Result<Vec<Value>, ApplicationError> {
        let Some(runs) = self.kernel_runs(false)? else {
            return Ok(Vec::new());
        };
        runs.kernel
            .store()
            .list(&format!("schedule-subscriptions/{prefix}"))
            .map_err(super::storage)?
            .into_iter()
            .map(|id| {
                Ok(runs
                    .kernel
                    .load(&id)
                    .map_err(kernel_error)?
                    .filter(|resource| resource.validator == VALIDATOR && !resource.state.is_null())
                    .map(|resource| resource.state))
            })
            .collect::<Result<Vec<_>, ApplicationError>>()
            .map(|items| items.into_iter().flatten().collect())
    }

    pub(in crate::application) fn put_subscription_fact(
        &self,
        id: &str,
        state: Value,
    ) -> Result<(), ApplicationError> {
        let runs = self.kernel_runs(true)?.expect("Kernel store initialized");
        let _write = runs.file_facts.lock().unwrap();
        runs.put_host_fact(&format!("schedule-subscriptions/{id}"), VALIDATOR, state)
    }
}
