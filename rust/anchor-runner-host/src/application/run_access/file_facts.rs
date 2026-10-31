//! Workspace grants and delivery retention share the Host's Kernel writer.
use super::kernel_runs::KernelRuns;
use super::{ApplicationError, RunApplication};
use anchor_kernel::{
    Action, JudgeCtx, Judgement, Principal, PrincipalKind, Reason, Resource, Submission, Validator,
    Verification,
};
use anchor_runtime::graph::scheduler::{HOST_ID, kernel_error, submit_with_retry};
use serde_json::Value;

pub(super) const FILE_VALIDATOR: &str = "anchor/host-files";
pub(super) struct HostFiles;

impl Validator for HostFiles {
    fn judge(&self, ctx: &JudgeCtx, submission: &Submission) -> Judgement {
        if ctx.principal.id != HOST_ID
            || ctx.principal.kind != PrincipalKind::Code
            || submission.action != Action::Write
            || !submission.content.is_object()
        {
            return Judgement::refused(Reason::ContentInvalid);
        }
        Judgement::accept(submission.content.clone(), Verification::Mechanical)
    }
}

impl RunApplication {
    pub(crate) fn file_fact(&self, id: &str) -> Result<Option<Value>, ApplicationError> {
        let Some(runs) = self.kernel_runs(false)? else {
            return Ok(None);
        };
        Ok(runs
            .kernel
            .load(&format!("files/{id}"))
            .map_err(kernel_error)?
            .filter(|resource| resource.validator == FILE_VALIDATOR && !resource.state.is_null())
            .map(|resource| resource.state))
    }

    pub(crate) fn put_file_fact(&self, id: &str, state: Value) -> Result<(), ApplicationError> {
        let runs = self.kernel_runs(true)?.expect("Kernel store initialized");
        let _write = runs.file_facts.lock().unwrap();
        runs.put_file_fact(id, state)
    }
}

impl KernelRuns {
    /// Caller holds file_facts across any corresponding filesystem operation.
    pub(super) fn put_file_fact(&self, id: &str, state: Value) -> Result<(), ApplicationError> {
        let id = format!("files/{id}");
        let resource = match self.kernel.load(&id).map_err(kernel_error)? {
            Some(resource) => resource,
            None => {
                let resource = Resource::new(&id, FILE_VALIDATOR, Value::Null);
                self.kernel.create(resource.clone()).map_err(kernel_error)?;
                resource
            }
        };
        if resource.state == state {
            return Ok(());
        }
        let verdict = submit_with_retry(
            &self.kernel,
            &Principal::new(HOST_ID, PrincipalKind::Code),
            0,
            Submission::new(id, resource.version, Action::Write).content(state),
            None,
        )?;
        if !verdict.is_accepted() {
            return Err(ApplicationError::Conflict(format!(
                "file fact refused: {:?}",
                verdict.reason
            )));
        }
        Ok(())
    }
}
