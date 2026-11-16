//! Node execution port adaptation; preparation owns the fallible stages.
use super::*;

impl NodeHostResolver for HostIoResolver {
    fn input_guidance(
        &self,
        request: &NodeExecutionRequest,
    ) -> Result<crate::harness_guidance::InputGuidance, String> {
        self.tool_assembly.input_guidance(request)
    }
    fn prompt_images(&self, request: &NodeExecutionRequest) -> Result<Vec<NodeImage>, String> {
        let mut images = self.channel_inputs(&request.key)?.images;
        images.extend(
            self.pending_channel_inputs(&request.key)?
                .into_iter()
                .flat_map(|inputs| inputs.images),
        );
        Ok(images
            .into_iter()
            .map(|image| NodeImage {
                data: image.bytes,
                media_type: image.media_type,
            })
            .collect())
    }
    fn conversation_hint(
        &self,
        request: &NodeExecutionRequest,
    ) -> Result<Option<NodeConversationHint>, String> {
        self.conversation(&request.key).map(|conversation| {
            conversation.map(|value| NodeConversationHint {
                key: value.key,
                cross_run: value.cross_run,
            })
        })
    }

    fn resolve_plugins(&self, ids: &[String]) -> Result<Vec<PluginBinding>, String> {
        self.resolve_selected_plugins(ids, &BTreeMap::new(), None)
    }

    fn workspace(&self, request: &NodeExecutionRequest) -> Result<PathBuf, String> {
        self.prepare_node_workspace(&request.key, &request.input_commits)
    }

    fn tools<'a>(&'a self, request: &'a NodeExecutionRequest) -> ToolResolution<'a> {
        self.prepare_tools(request)
    }
}

impl HostIoResolver {
    pub(super) fn resolve_selected_plugins(
        &self,
        ids: &[String],
        revisions: &BTreeMap<String, String>,
        lease: Option<&anchor_library::InstallLease>,
    ) -> Result<Vec<PluginBinding>, String> {
        let catalog = crate::plugin_library::Catalog::new(self.plugin_library_root()?);
        ids.iter()
            .map(|id| {
                self.tools
                    .resolve_fixture_plugins(std::slice::from_ref(id))
                    .ok()
                    .and_then(|mut bindings| bindings.pop())
                    .map(Ok)
                    .unwrap_or_else(|| {
                        catalog
                            .binding_for_revision(id, revisions.get(id).map(String::as_str), lease)
                            .map_err(|error| error.to_string())
                    })
            })
            .collect()
    }
}
