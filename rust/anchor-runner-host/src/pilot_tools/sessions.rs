//! The sessions tools the assistant may call.
//!
//! Split out of the single dispatch in `pilot_tools`; the table there stays
//! the only description of what these tools accept.

use super::*;

impl PilotTools {
    pub(super) async fn artifact_read(&self, arguments: &Value) -> Result<Value, ToolError> {
        let run = parameter(arguments, "run")?;
        let node = parameter(arguments, "node")?;
        let path = parameter(arguments, "path")?;
        if !anchor_runtime::identifier::is_name(&run) {
            return Err(failed());
        }
        let commit = self
            .application
            .node_delivery(&run, &node)
            .map_err(|_| failed())?;
        let opened = self
            .application
            .open_delivery_file(&commit.id, &path, &self.workspace_root)
            .map_err(|_| failed())?;
        let (file, _lease) = opened.into_parts();
        let text = text_resource(file)?;
        Ok(json!({"run":run,"node":node,"path":path,"text":text}))
    }

    pub(super) async fn session_wait(&self, _arguments: &Value) -> Result<Value, ToolError> {
        let session = self
            .sessions
            .get(&self.owner, &self.session)
            .map_err(|_| failed())?;
        let retained = self
            .application
            .pilot_session_runs(&session.run_ids)
            .map_err(|_| failed())?;
        Ok(json!({"session":session,"runs":retained}))
    }
}
