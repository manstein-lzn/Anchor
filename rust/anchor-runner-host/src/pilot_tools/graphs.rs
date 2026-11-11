//! The graphs tools the assistant may call.
//!
//! Split out of the single dispatch in `pilot_tools`; the table there stays
//! the only description of what these tools accept.

use super::*;

#[cfg(test)]
mod convergence_tests;

pub(super) fn definition_parameter() -> Value {
    let example = json!({
        "entry":"hello",
        "ops":{"hello":{"run":"printf 'hello\\n' > hello.txt","writes":["hello.txt"]}},
        "nodes":[{"id":"hello","op":"hello"}],
        "edges":[]
    });
    json!({
        "type":"object",
        "description":format!(
            "Graph authoring JSON. ops and agents are objects keyed by definition name. Agent model references: {}; omit model for default Flash. instructions is optional; agents must finish by calling final_result with files (use [] for text only). nodes is a list of {{id,op}} or {{id,agent}} referencing those names; entry is a node ID; edges is a list of {{from,to}}. Op run is a shell command string, not an argv array. Put reads and writes on the op/agent definition. Put installed Plugin IDs in an AgentNode's plugins list. Task networking belongs to each node's Plugin reference {{id:network,network:auto|enabled|denied}}; auto uses selected Plugin requirements, denied always wins. Op.run accepts only this network reference, not ordinary Plugins. Each node has its own stable /workspace, not a shared directory. Agent final_result.files selects delivered paths relative to /workspace; writes lists required deliveries (Op run delivers writes directly). Downstream nodes receive committed artifacts read-only at /in/<source-node>; copy needed files into their own /workspace to edit. reads declares inputs, not workspace sharing. Minimal definition: {example}",
            crate::model_configuration::MODEL_REFERENCES
        ),
        "examples":[example]
    })
}

impl PilotTools {
    pub(super) async fn graph_create(&self, arguments: &Value) -> Result<Value, ToolError> {
        let graph = parameter(arguments, "name")?;
        self.refuse_self_graph(&graph)?;
        self.require_active_turn()?;
        self.application
            .create_graph(&graph, arguments.get("definition").cloned())
            .await
            .map_err(|failure| ToolError::Failed(format!("Graph creation rejected: {failure}")))
    }

    pub(super) async fn graph_update(&self, arguments: &Value) -> Result<Value, ToolError> {
        let graph = parameter(arguments, "graph")?;
        // A self-target is refused before anything else: the Graph that
        // defines the assistant is not something the assistant may
        // rewrite, regardless of Turn state.
        self.refuse_self_graph(&graph)?;
        self.require_active_turn()?;
        let definition = arguments
            .get("definition")
            .filter(|value| value.is_object())
            .cloned()
            .ok_or_else(|| ToolError::Failed("definition must be an object".into()))?;
        self.application
            .update_graph_from_agent(&graph, definition)
            .await
            .map_err(|failure| ToolError::Failed(format!("Graph update rejected: {failure}")))
    }

    pub(super) async fn graph_run(&self, arguments: &Value) -> Result<Value, ToolError> {
        self.require_active_turn()?;
        let graph = parameter(arguments, "graph")?;
        let objective = match arguments.get("objective") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            _ => return Err(ToolError::Failed("objective must be a string".into())),
        };
        let run = self
            .application
            .admit_graph_with_options(
                &graph,
                arguments.get("input").cloned().unwrap_or_else(|| json!({})),
                AdmissionOptions {
                    objective,
                    trigger: RunTrigger::default(),
                    pilot: Some(PilotRunSource {
                        owner: self.owner.clone(),
                        session: self.session.clone(),
                        turn: self.turn.clone(),
                    }),
                    oauth_owner: Some(crate::api::oauth::binding_owner(&self.owner)),
                    workspace_bindings: Default::default(),
                    workspace_root: None,
                },
                None,
                None,
            )
            .await
            .map_err(|failure| ToolError::Failed(format!("Run admission rejected: {failure:?}")))?;
        let mut result = json!({"run":run,"graph":graph,"session":self.session,"accepted":true});
        if self
            .sessions
            .associate_run(&self.owner, &self.session, &self.turn, &run)
            .is_err()
        {
            result["association_error"] = json!(
                "Run accepted; Session association pending reconciliation. Do not retry graph_run; inspect this Run ID."
            );
        }
        Ok(result)
    }

    pub(super) async fn graph_list(&self, _arguments: &Value) -> Result<Value, ToolError> {
        let listing = self.application.list_graphs().await.map_err(|_| failed())?;
        let graphs = listing["graphs"]
            .as_array()
            .ok_or_else(failed)?
            .iter()
            .map(|graph| json!({"graph":graph["graph"],"active_runs":graph["active_runs"]}))
            .collect::<Vec<_>>();
        Ok(json!({"graphs":graphs}))
    }

    pub(super) async fn graph_read(&self, arguments: &Value) -> Result<Value, ToolError> {
        let graph = parameter(arguments, "graph")?;
        let value = self.application.read_graph(&graph).map_err(|_| failed())?;
        Ok(json!({"graph":graph,"definition":value["definition"]}))
    }

    pub(super) async fn graph_validate(&self, arguments: &Value) -> Result<Value, ToolError> {
        let definition = arguments
            .get("definition")
            .filter(|value| value.is_object())
            .ok_or_else(|| rejected("definition must be an object"))?;
        // Kernel diagnostics describe only this authoring input. Keep them
        // actionable without exposing errors from private Host/Plugin state.
        let snapshot = match self.application.inspect_graph_definition(definition) {
            Ok(snapshot) => snapshot,
            Err(crate::application::graphs::GraphValidationFailure::Authoring(failure)) => {
                return Ok(json!({"valid":false,"error":failure.to_string()}));
            }
            Err(crate::application::graphs::GraphValidationFailure::Unavailable(_)) => {
                return Ok(json!({"valid":false,"error":"invalid Graph or unavailable Plugin"}));
            }
        };
        Ok(
            json!({"valid":true,"entry":snapshot.entry,"nodes":snapshot.nodes.iter().map(|node| &node.id).collect::<Vec<_>>()}),
        )
    }
}
