//! Channel input authority comes from the admitted Run, never model input paths.
use super::*;

impl HostIoResolver {
    pub(super) fn channel_inputs(
        &self,
        key: &InvocationKey,
    ) -> Result<crate::channel_inputs::ChannelNodeInputs, String> {
        let root = self.local_inputs.state_root();
        let Some(metadata) = crate::application::metadata::load(root, &key.run_id)
            .map_err(|error| format!("channel input metadata: {error:?}"))?
        else {
            return Ok(crate::channel_inputs::ChannelNodeInputs {
                mount: None,
                images: Vec::new(),
            });
        };
        let record = self
            .run_store
            .load(&key.run_id)
            .map_err(|error| error.to_string())?
            .ok_or("channel inputs require an admitted Run")?;
        if record.graph_digest != key.graph_digest
            || !record
                .snapshot
                .nodes
                .iter()
                .any(|node| node.id == key.node_id)
        {
            return Err("channel inputs do not belong to the requested Run/node".into());
        }
        crate::channel_inputs::node_inputs(root, &metadata, key)
    }
}
