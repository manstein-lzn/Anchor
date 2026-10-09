//! Channel input authority comes from the admitted Run, never model input paths.
use super::*;

impl HostIoResolver {
    pub(super) fn pending_channel_inputs(
        &self,
        key: &InvocationKey,
    ) -> Result<Vec<crate::channel_inputs::ChannelNodeInputs>, String> {
        let root = self.local_inputs.state_root();
        let Some(metadata) = crate::application::metadata::load(root, &key.run_id)
            .map_err(|error| format!("channel input metadata: {error:?}"))?
        else {
            return Ok(Vec::new());
        };
        let Some(source) = metadata.assistant.as_ref() else {
            return Ok(Vec::new());
        };
        crate::assistant::pending_inputs(root, key)?
            .into_iter()
            .map(|input| {
                let previous = input
                    .relation
                    .run_id
                    .as_ref()
                    .map(|run| crate::application::metadata::load(root, run))
                    .transpose()
                    .map_err(|error| format!("pending channel input metadata: {error:?}"))?
                    .flatten();
                let selected = match previous {
                    Some(previous) if previous.assistant.is_none() => {
                        if previous.channel.as_ref().is_none_or(|channel| {
                            channel.owner != source.owner
                                || channel.session != source.session
                                || channel.inbound != input.relation.inbound_id
                                || channel.turn != input.relation.turn_id
                        }) {
                            return Err(
                                "pending channel input belongs to another conversation".into()
                            );
                        }
                        previous
                    }
                    Some(previous)
                        if previous.assistant.as_ref().is_none_or(|binding| {
                            binding.owner != source.owner || binding.session != source.session
                        }) =>
                    {
                        return Err(
                            "pending assistant input belongs to another conversation".into()
                        );
                    }
                    _ => crate::assistant::attachment_metadata_for_input(&metadata, &input),
                };
                let selected_key = InvocationKey {
                    run_id: selected.run_id.clone(),
                    graph_digest: selected.graph_digest.clone(),
                    ..key.clone()
                };
                let mut inputs =
                    crate::channel_inputs::node_inputs(root, &selected, &selected_key)?;
                if let Some(mount) = &mut inputs.mount {
                    mount.destination =
                        format!("/in/channel-pending/{}", input.relation.turn_id).into();
                }
                Ok(inputs)
            })
            .collect()
    }

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
        if metadata.assistant.is_some() {
            let selected = crate::assistant::attachment_metadata(root, &metadata, key)?;
            let selected_key = InvocationKey {
                run_id: selected.run_id.clone(),
                ..key.clone()
            };
            return crate::channel_inputs::node_inputs(root, &selected, &selected_key);
        }
        crate::channel_inputs::node_inputs(root, &metadata, key)
    }
}
