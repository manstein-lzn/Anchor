//! Compiler for editable Python-compatible `graph.json` definitions.
//!
//! The authoring document deliberately stays outside `GraphSnapshot`: editor
//! metadata and graph modules are consumed here and only the flat executable
//! form crosses into runtime admission.

use super::*;
use serde_json::{Map, json};

const AUTHOR_FIELDS: &[&str] = &[
    "objective",
    "input",
    "entry",
    "max_rounds",
    "agents",
    "ops",
    "graphs",
    "nodes",
    "edges",
    "layout",
    "_module_rounds",
];
const BODY_FIELDS: &[&str] = &["entry", "exit", "max_rounds", "nodes", "edges"];

#[derive(Debug, Clone)]
struct AuthorNode {
    id: String,
    kind: AuthorNodeKind,
    with: Option<String>,
    plugins: Vec<String>,
    max_rounds: Option<u32>,
}

#[derive(Debug, Clone)]
enum AuthorNodeKind {
    Agent(String),
    Op(String),
    Graph(String),
}

#[derive(Debug, Clone)]
struct AuthorEdge {
    from: String,
    to: String,
}

#[derive(Debug, Clone)]
struct AuthorBody {
    entry: Option<String>,
    exit: Option<String>,
    max_rounds: Option<u32>,
    nodes: Vec<AuthorNode>,
    edges: Vec<AuthorEdge>,
}

#[derive(Debug, Clone, Default)]
struct Expansion {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    max_rounds: BTreeMap<String, u32>,
    module_rounds: BTreeMap<String, u32>,
    scopes: BTreeMap<String, String>,
    entry: Option<String>,
    exit: Option<String>,
}

impl GraphSnapshot {
    /// Compile an editable Python-compatible graph definition to the flat
    /// Runtime snapshot. `layout` is editor state and is intentionally ignored.
    /// Unknown execution fields are errors instead of being silently dropped.
    pub fn from_authoring(value: Value) -> Result<Self, GraphError> {
        compile_authoring(value)
    }
}

fn invalid(message: impl Into<String>) -> GraphError {
    GraphError::InvalidSnapshot(message.into())
}

fn object<'a>(value: &'a Value, where_: &str) -> Result<&'a Map<String, Value>, GraphError> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{where_} must be a JSON object")))
}

fn reject_unknown(
    fields: &Map<String, Value>,
    allowed: &[&str],
    where_: &str,
) -> Result<(), GraphError> {
    if let Some(key) = fields.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(format!("{where_}: unknown field `{key}`")));
    }
    Ok(())
}

fn required_string(value: Option<&Value>, where_: &str) -> Result<String, GraphError> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("{where_} must be a non-empty string")))
}

fn optional_name(value: Option<&Value>, where_: &str) -> Result<Option<String>, GraphError> {
    value
        .map(|value| required_string(Some(value), where_))
        .transpose()
}

fn positive_u32(value: &Value, where_: &str) -> Result<u32, GraphError> {
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .filter(|number| *number > 0)
        .ok_or_else(|| invalid(format!("{where_} must be a positive 32-bit integer")))
}

fn optional_rounds(value: Option<&Value>, where_: &str) -> Result<Option<u32>, GraphError> {
    value.map(|value| positive_u32(value, where_)).transpose()
}

fn optional_bool(value: Option<&Value>, default: bool, where_: &str) -> Result<bool, GraphError> {
    value
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| invalid(format!("{where_} must be a boolean")))
        })
        .unwrap_or(Ok(default))
}

fn files(value: Option<&Value>, where_: &str) -> Result<Vec<String>, GraphError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| invalid(format!("{where_} must be a list of file names")))?;
    let mut result = Vec::new();
    for value in values {
        let name = value
            .as_str()
            .ok_or_else(|| invalid(format!("{where_} must be a list of file names")))?;
        if name.is_empty() || name.starts_with('/') || name.split('/').any(|part| part == "..") {
            return Err(invalid(format!(
                "{where_} names {name:?}, which is not a path inside the workspace"
            )));
        }
        if !result.iter().any(|prior| prior == name) {
            result.push(name.to_owned());
        }
    }
    Ok(result)
}

fn plugin_refs(value: Option<&Value>, where_: &str) -> Result<Vec<String>, GraphError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| invalid(format!("{where_} must be a list of Plugin references")))?;
    let mut result = Vec::new();
    for value in values {
        let name = value.as_str().filter(|name| {
            let mut chars = name.chars();
            chars
                .next()
                .is_some_and(|first| first.is_ascii_alphanumeric())
                && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        });
        let Some(name) = name else {
            return Err(invalid(format!(
                "{where_} contains an invalid Plugin reference"
            )));
        };
        if !result.iter().any(|prior| prior == name) {
            result.push(name.to_owned());
        }
    }
    Ok(result)
}

fn validate_time(value: Option<&Value>, where_: &str) -> Result<Option<f64>, GraphError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let seconds = value
        .as_f64()
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .ok_or_else(|| invalid(format!("{where_} must be a positive finite number")))?;
    Ok(Some(seconds))
}

fn parse_agents(value: Option<&Value>) -> Result<BTreeMap<String, AgentDefinition>, GraphError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let agents = object(value, "agents")?;
    let mut result = BTreeMap::new();
    for (name, value) in agents {
        let where_ = format!("agent {name:?}");
        if name.is_empty() {
            return Err(invalid(format!("{where_}: a name must be non-empty")));
        }
        let spec = object(value, &where_)?;
        reject_unknown(
            spec,
            &[
                "model",
                "instructions",
                "network",
                "max_steps",
                "wall_time_limit_seconds",
                "reads",
                "writes",
            ],
            &where_,
        )?;
        if spec.contains_key("plugins") {
            return Err(invalid(format!(
                "{where_}: put plugins on the AgentNode, not the shared role"
            )));
        }
        let model = required_string(spec.get("model"), &format!("{where_}.model"))?;
        let instructions = spec
            .get("instructions")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid(format!("{where_}.instructions must be a string")))
            })
            .transpose()?
            .unwrap_or_default();
        let max_steps = match spec.get("max_steps") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_u64().ok_or_else(|| {
                invalid(format!(
                    "{where_}.max_steps must be a non-negative integer or null"
                ))
            })?),
        };
        result.insert(
            name.clone(),
            AgentDefinition {
                model,
                instructions,
                network: optional_bool(spec.get("network"), false, &format!("{where_}.network"))?,
                max_steps,
                wall_time_limit_seconds: Some(
                    validate_time(
                        spec.get("wall_time_limit_seconds"),
                        &format!("{where_}.wall_time_limit_seconds"),
                    )?
                    .unwrap_or(3600.0),
                ),
                reads: files(spec.get("reads"), &format!("{where_}.reads"))?,
                writes: files(spec.get("writes"), &format!("{where_}.writes"))?,
            },
        );
    }
    Ok(result)
}

fn parse_ops(value: Option<&Value>) -> Result<BTreeMap<String, Value>, GraphError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let ops = object(value, "ops")?;
    let mut result = BTreeMap::new();
    for (name, value) in ops {
        let where_ = format!("op {name:?}");
        if name.is_empty() {
            return Err(invalid(format!("{where_}: a name must be non-empty")));
        }
        let spec = object(value, &where_)?;
        let executions = ["run", "call", "fanout", "join"]
            .iter()
            .filter(|key| spec.contains_key(**key))
            .count();
        if executions != 1 {
            return Err(invalid(format!(
                "{where_} needs exactly one of `run`, `call`, `fanout` or `join`"
            )));
        }
        let is_parallel_control = spec.contains_key("fanout") || spec.contains_key("join");
        let allowed = if is_parallel_control {
            &[
                "fanout",
                "join",
                "reads",
                "writes",
                "network",
                "wall_time_limit_seconds",
            ][..]
        } else {
            &[
                "run",
                "call",
                "reads",
                "writes",
                "network",
                "wall_time_limit_seconds",
            ][..]
        };
        reject_unknown(spec, allowed, &where_)?;
        let reads = files(spec.get("reads"), &format!("{where_}.reads"))?;
        let mut writes = files(spec.get("writes"), &format!("{where_}.writes"))?;
        let network = optional_bool(spec.get("network"), false, &format!("{where_}.network"))?;
        validate_time(
            spec.get("wall_time_limit_seconds"),
            &format!("{where_}.wall_time_limit_seconds"),
        )?;
        if let Some(run) = spec.get("run")
            && run.as_str().is_none_or(|run| run.trim().is_empty())
        {
            return Err(invalid(format!(
                "{where_}.run must be a non-empty command string"
            )));
        }
        if let Some(fanout) = spec.get("fanout") {
            let fanout = object(fanout, &format!("{where_}.fanout"))?;
            reject_unknown(fanout, &["join"], &format!("{where_}.fanout"))?;
            required_string(fanout.get("join"), &format!("{where_}.fanout.join"))?;
        }
        if let Some(join) = spec.get("join") {
            let join = object(join, &format!("{where_}.join"))?;
            reject_unknown(join, &[], &format!("{where_}.join"))?;
        }
        if spec.contains_key("fanout") {
            append_unique(&mut writes, "fanout.json");
        } else if spec.contains_key("join") {
            append_unique(&mut writes, "join.json");
        } else if let Some(call) = spec.get("call") {
            let call = object(call, &format!("{where_}.call"))?;
            reject_unknown(
                call,
                &[
                    "graph",
                    "mode",
                    "input",
                    "input_map",
                    "files",
                    "result",
                    "session",
                ],
                &format!("{where_}.call"),
            )?;
            append_unique(&mut writes, "call.json");
            if let Some(result_files) = call
                .get("result")
                .and_then(|result| result.get("files"))
                .and_then(Value::as_array)
            {
                for file in result_files {
                    if let Some(file) = file.as_str() {
                        append_unique(&mut writes, &format!("result/{file}"));
                    }
                }
            }
        }
        let mut normalized = spec.clone();
        normalized.insert("network".into(), Value::Bool(network));
        normalized.insert(
            "wall_time_limit_seconds".into(),
            spec.get("wall_time_limit_seconds")
                .cloned()
                .unwrap_or_else(|| json!(3600)),
        );
        if !reads.is_empty() {
            normalized.insert("reads".into(), json!(reads));
        }
        if !writes.is_empty() {
            normalized.insert("writes".into(), json!(writes));
        }
        // `GraphSnapshot::validate` owns the complete Op.call schema and safety checks.
        result.insert(name.clone(), Value::Object(normalized));
    }
    Ok(result)
}

fn append_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|prior| prior == value) {
        values.push(value.to_owned());
    }
}

fn parse_body(
    value: &Value,
    where_: &str,
    is_module: bool,
    allow_slash: bool,
) -> Result<AuthorBody, GraphError> {
    let body = object(value, where_)?;
    reject_unknown(body, BODY_FIELDS, where_)?;
    if is_module {
        for forbidden in ["agents", "ops", "objective", "input", "graphs"] {
            if body.contains_key(forbidden) {
                return Err(invalid(format!(
                    "{where_} declares `{forbidden}`, which only the file may declare"
                )));
            }
        }
        if body
            .get("exit")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(invalid(format!(
                "{where_} needs an `exit` naming the node whose directory is this module's result"
            )));
        }
    }
    let max_rounds = optional_rounds(body.get("max_rounds"), &format!("{where_}.max_rounds"))?;
    let nodes_value = body
        .get("nodes")
        .and_then(Value::as_array)
        .filter(|nodes| !nodes.is_empty())
        .ok_or_else(|| invalid(format!("{where_} needs at least one node")))?;
    let mut nodes = Vec::new();
    let mut ids = BTreeSet::new();
    for (index, value) in nodes_value.iter().enumerate() {
        let node_where = format!("{where_}.nodes[{index}]");
        let spec = object(value, &node_where)?;
        reject_unknown(
            spec,
            &[
                "id",
                "agent",
                "op",
                "graph",
                "with",
                "plugins",
                "max_rounds",
            ],
            &node_where,
        )?;
        let id = required_string(spec.get("id"), &format!("{node_where}.id"))?;
        if !allow_slash && id.contains('/') {
            return Err(invalid(format!(
                "{node_where}: node id {id:?} contains `/`, reserved for module expansion"
            )));
        }
        if !ids.insert(id.clone()) {
            return Err(invalid(format!(
                "{where_} declares node id {id:?} more than once"
            )));
        }
        let present = ["agent", "op", "graph"]
            .iter()
            .filter(|key| spec.contains_key(**key))
            .count();
        if present != 1 {
            return Err(invalid(format!(
                "{node_where}: node {id:?} needs exactly one of `agent`, `op` or `graph`"
            )));
        }
        let kind = if let Some(agent) = spec.get("agent") {
            AuthorNodeKind::Agent(required_string(
                Some(agent),
                &format!("{node_where}.agent"),
            )?)
        } else if let Some(op) = spec.get("op") {
            AuthorNodeKind::Op(required_string(Some(op), &format!("{node_where}.op"))?)
        } else {
            AuthorNodeKind::Graph(required_string(
                spec.get("graph"),
                &format!("{node_where}.graph"),
            )?)
        };
        let with = spec
            .get("with")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid(format!("{node_where}.with must be a string")))
            })
            .transpose()?;
        if with.is_some() && matches!(kind, AuthorNodeKind::Graph(_)) {
            return Err(invalid(format!(
                "{node_where}: a graph node cannot carry `with`; put it on nodes inside that graph"
            )));
        }
        if with.is_some() && matches!(kind, AuthorNodeKind::Op(_)) {
            return Err(invalid(format!(
                "{node_where}: an op node cannot carry `with`"
            )));
        }
        let plugins = plugin_refs(spec.get("plugins"), &format!("{node_where}.plugins"))?;
        if !plugins.is_empty() && !matches!(kind, AuthorNodeKind::Agent(_)) {
            return Err(invalid(format!(
                "{node_where}: plugins can only be attached to an AgentNode"
            )));
        }
        nodes.push(AuthorNode {
            id,
            kind,
            with,
            plugins,
            max_rounds: optional_rounds(
                spec.get("max_rounds"),
                &format!("{node_where}.max_rounds"),
            )?,
        });
    }
    let edges_value = body
        .get("edges")
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| invalid(format!("{where_}.edges must be a list")))
        })
        .transpose()?;
    let mut edges = Vec::new();
    let mut seen_edges = BTreeSet::new();
    for (index, value) in edges_value.into_iter().flatten().enumerate() {
        let edge_where = format!("{where_}.edges[{index}]");
        let spec = object(value, &edge_where)?;
        reject_unknown(spec, &["from", "to"], &edge_where)?;
        let from = required_string(spec.get("from"), &format!("{edge_where}.from"))?;
        let to = required_string(spec.get("to"), &format!("{edge_where}.to"))?;
        if !ids.contains(&from) || !ids.contains(&to) {
            return Err(invalid(format!(
                "{edge_where} names an unknown node ({from:?} -> {to:?})"
            )));
        }
        if !seen_edges.insert((from.clone(), to.clone())) {
            return Err(invalid(format!(
                "{edge_where} duplicates edge {from:?} -> {to:?}"
            )));
        }
        edges.push(AuthorEdge { from, to });
    }
    Ok(AuthorBody {
        entry: optional_name(body.get("entry"), &format!("{where_}.entry"))?,
        exit: optional_name(body.get("exit"), &format!("{where_}.exit"))?,
        max_rounds,
        nodes,
        edges,
    })
}

fn infer_entry(body: &AuthorBody, where_: &str) -> Result<String, GraphError> {
    let incoming = body
        .edges
        .iter()
        .map(|edge| edge.to.as_str())
        .collect::<BTreeSet<_>>();
    let starts = body
        .nodes
        .iter()
        .filter(|node| !incoming.contains(node.id.as_str()))
        .map(|node| node.id.as_str())
        .collect::<Vec<_>>();
    match starts.as_slice() {
        [entry] => Ok((*entry).to_owned()),
        _ => Err(invalid(format!(
            "{where_}: explicit `entry` required when there is a loop or multiple starts; nodes with no incoming edge: {starts:?}"
        ))),
    }
}

fn validate_references_and_cycles(
    root: &AuthorBody,
    modules: &BTreeMap<String, AuthorBody>,
) -> Result<(), GraphError> {
    let mut state = BTreeMap::<String, u8>::new();
    for node in &root.nodes {
        if let AuthorNodeKind::Graph(target) = &node.kind
            && !modules.contains_key(target)
        {
            return Err(invalid(format!(
                "graph: node {:?} refers to undeclared graph {target:?}",
                node.id
            )));
        }
    }
    fn visit(
        name: &str,
        path: &mut Vec<String>,
        state: &mut BTreeMap<String, u8>,
        modules: &BTreeMap<String, AuthorBody>,
    ) -> Result<(), GraphError> {
        state.insert(name.to_owned(), 1);
        path.push(name.to_owned());
        for node in &modules[name].nodes {
            let AuthorNodeKind::Graph(target) = &node.kind else {
                continue;
            };
            if !modules.contains_key(target) {
                return Err(invalid(format!(
                    "graph {name:?}: node {:?} refers to undeclared graph {target:?}",
                    node.id
                )));
            }
            match state.get(target).copied() {
                Some(1) => {
                    let mut cycle = path.clone();
                    cycle.push(target.clone());
                    return Err(invalid(format!(
                        "graphs contain each other in a cycle: {}",
                        cycle.join(" -> ")
                    )));
                }
                Some(2) => {}
                _ => visit(target, path, state, modules)?,
            }
        }
        path.pop();
        state.insert(name.to_owned(), 2);
        Ok(())
    }
    for name in modules.keys() {
        if state.get(name).copied().unwrap_or_default() == 0 {
            visit(name, &mut Vec::new(), &mut state, modules)?;
        }
    }
    Ok(())
}

fn expand(
    body: &AuthorBody,
    modules: &BTreeMap<String, AuthorBody>,
    prefix: &str,
) -> Result<Expansion, GraphError> {
    let mut result = Expansion::default();
    let ceiling = body.max_rounds;
    let mut sides = BTreeMap::<String, (String, String)>::new();
    for node in &body.nodes {
        let flat = format!("{prefix}{}", node.id);
        match &node.kind {
            AuthorNodeKind::Graph(name) => {
                let inner = expand(&modules[name], modules, &format!("{flat}/"))?;
                let entry = inner
                    .entry
                    .clone()
                    .ok_or_else(|| invalid(format!("graph {name:?} has no entry")))?;
                let exit = inner
                    .exit
                    .clone()
                    .ok_or_else(|| invalid(format!("graph {name:?} has no exit")))?;
                result.nodes.extend(inner.nodes);
                result.edges.extend(inner.edges);
                result.max_rounds.extend(inner.max_rounds);
                result.module_rounds.extend(inner.module_rounds);
                result.scopes.extend(inner.scopes);
                if let Some(limit) = node.max_rounds.or(ceiling) {
                    result.module_rounds.insert(flat.clone(), limit);
                }
                sides.insert(node.id.clone(), (entry, exit));
            }
            AuthorNodeKind::Agent(agent) => {
                result.nodes.push(GraphNode {
                    id: flat.clone(),
                    agent: Some(agent.clone()),
                    op: None,
                    input: node.with.clone().map(Value::String),
                    plugins: node.plugins.clone(),
                    max_rounds: node.max_rounds.or(ceiling),
                });
                result.scopes.insert(flat.clone(), prefix.to_owned());
                if let Some(limit) = node.max_rounds.or(ceiling) {
                    result.max_rounds.insert(flat.clone(), limit);
                }
                sides.insert(node.id.clone(), (flat.clone(), flat));
            }
            AuthorNodeKind::Op(op) => {
                result.nodes.push(GraphNode {
                    id: flat.clone(),
                    agent: None,
                    op: Some(op.clone()),
                    input: None,
                    plugins: Vec::new(),
                    max_rounds: node.max_rounds.or(ceiling),
                });
                result.scopes.insert(flat.clone(), prefix.to_owned());
                if let Some(limit) = node.max_rounds.or(ceiling) {
                    result.max_rounds.insert(flat.clone(), limit);
                }
                sides.insert(node.id.clone(), (flat.clone(), flat));
            }
        }
    }
    for edge in &body.edges {
        let source = sides
            .get(&edge.from)
            .ok_or_else(|| invalid(format!("unknown edge source {:?}", edge.from)))?;
        let target = sides
            .get(&edge.to)
            .ok_or_else(|| invalid(format!("unknown edge target {:?}", edge.to)))?;
        result.edges.push(GraphEdge {
            from_node: source.1.clone(),
            to_node: target.0.clone(),
        });
    }
    let entry = body
        .entry
        .clone()
        .map(Ok)
        .unwrap_or_else(|| infer_entry(body, "graph"))?;
    let entry = sides
        .get(&entry)
        .ok_or_else(|| invalid(format!("graph entry {entry:?} does not name a node")))?
        .0
        .clone();
    result.entry = Some(entry);
    if let Some(exit) = &body.exit {
        result.exit = Some(
            sides
                .get(exit)
                .ok_or_else(|| invalid(format!("graph exit {exit:?} does not name a node")))?
                .1
                .clone(),
        );
    }
    Ok(result)
}

fn specialize_fanouts(
    expansion: &mut Expansion,
    ops: &mut BTreeMap<String, Value>,
) -> Result<(), GraphError> {
    let mut bound = BTreeMap::<(String, String), String>::new();
    for node in &mut expansion.nodes {
        let Some(op_name) = node.op.as_mut() else {
            continue;
        };
        let Some(op) = ops.get(op_name) else {
            continue;
        };
        let Some(join) = op
            .get("fanout")
            .and_then(|fanout| fanout.get("join"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let prefix = expansion.scopes.get(&node.id).cloned().unwrap_or_default();
        if prefix.is_empty() {
            continue;
        }
        let key = (op_name.clone(), prefix.clone());
        if let Some(bound_name) = bound.get(&key) {
            *op_name = bound_name.clone();
            continue;
        }
        let mut specialized = format!("{}@{}", op_name, prefix.trim_end_matches('/'));
        while ops.contains_key(&specialized) {
            specialized.push('@');
        }
        let mut spec = op.clone();
        let fanout = spec
            .get_mut("fanout")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| invalid(format!("fanout op {op_name:?} requires an object payload")))?;
        fanout.insert("join".into(), Value::String(format!("{prefix}{join}")));
        ops.insert(specialized.clone(), spec);
        bound.insert(key, specialized.clone());
        *op_name = specialized;
    }
    Ok(())
}

fn compile_authoring(value: Value) -> Result<GraphSnapshot, GraphError> {
    let raw = object(&value, "a graph")?;
    reject_unknown(raw, AUTHOR_FIELDS, "graph")?;
    raw.get("nodes")
        .and_then(Value::as_array)
        .filter(|nodes| !nodes.is_empty())
        .ok_or_else(|| invalid("a graph needs a non-empty `nodes` list"))?;
    if raw
        .get("agents")
        .and_then(Value::as_object)
        .is_none_or(Map::is_empty)
        && raw
            .get("ops")
            .and_then(Value::as_object)
            .is_none_or(Map::is_empty)
    {
        return Err(invalid(
            "a graph needs a non-empty `agents` or `ops` object",
        ));
    }
    if let Some(input) = raw.get("input")
        && !input.is_object()
    {
        return Err(invalid("graph `input` must be a JSON object"));
    }
    let objective = raw
        .get("objective")
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("graph `objective` must be a string"))
        })
        .transpose()?
        .unwrap_or_default();
    let agents = parse_agents(raw.get("agents"))?;
    let mut ops = parse_ops(raw.get("ops"))?;
    let modules_raw = raw
        .get("graphs")
        .map(|value| object(value, "graphs"))
        .transpose()?;
    let has_modules = modules_raw.is_some_and(|modules| !modules.is_empty());
    let root_value = Value::Object(
        raw.iter()
            .filter(|(key, _)| BODY_FIELDS.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    let root = parse_body(&root_value, "graph", false, !has_modules)?;
    let mut modules = BTreeMap::new();
    if let Some(modules_raw) = modules_raw {
        for (name, value) in modules_raw {
            if name.is_empty() {
                return Err(invalid("graphs contains an empty graph name"));
            }
            modules.insert(
                name.clone(),
                parse_body(value, &format!("graph {name:?}"), true, false)?,
            );
        }
    }
    validate_references_and_cycles(&root, &modules)?;
    for node in root
        .nodes
        .iter()
        .chain(modules.values().flat_map(|body| body.nodes.iter()))
    {
        match &node.kind {
            AuthorNodeKind::Agent(name) if !agents.contains_key(name) => {
                return Err(invalid(format!(
                    "node {:?} references undeclared agent {name:?}",
                    node.id
                )));
            }
            AuthorNodeKind::Op(name) if !ops.contains_key(name) => {
                return Err(invalid(format!(
                    "node {:?} references undeclared op {name:?}",
                    node.id
                )));
            }
            _ => {}
        }
    }
    let mut expansion = expand(&root, &modules, "")?;
    specialize_fanouts(&mut expansion, &mut ops)?;
    let node_order = expansion
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    expansion.edges.sort_by_key(|edge| {
        node_order
            .get(edge.from_node.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });
    let mut snapshot = GraphSnapshot {
        objective,
        input: raw.get("input").cloned().unwrap_or_else(|| json!({})),
        entry: expansion
            .entry
            .take()
            .ok_or_else(|| invalid("graph has no entry"))?,
        agents,
        ops,
        nodes: expansion.nodes,
        edges: expansion.edges,
        module_rounds: expansion.module_rounds,
    };
    if let Some(rounds) = raw.get("_module_rounds") {
        let rounds = object(rounds, "_module_rounds")?;
        let mut provided = BTreeMap::new();
        for (scope, value) in rounds {
            let limit = positive_u32(value, "_module_rounds values")?;
            if !snapshot
                .nodes
                .iter()
                .any(|node| node.id.starts_with(&format!("{scope}/")))
            {
                return Err(invalid(format!(
                    "_module_rounds names unknown scope {scope:?}"
                )));
            }
            provided.insert(scope.clone(), limit);
        }
        snapshot.module_rounds = provided;
    }
    validate_interfaces(&snapshot)?;
    snapshot.validate()?;
    Ok(snapshot)
}

fn owned_interface_files(snapshot: &GraphSnapshot, node: &GraphNode, field: &str) -> Vec<String> {
    if let Some(agent_name) = &node.agent {
        let agent = &snapshot.agents[agent_name];
        return if field == "reads" {
            agent.reads.clone()
        } else {
            agent.writes.clone()
        };
    }
    node.op
        .as_ref()
        .and_then(|name| snapshot.ops.get(name))
        .and_then(|op| op.get(field))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn validate_interfaces(snapshot: &GraphSnapshot) -> Result<(), GraphError> {
    let back_edges = structural_back_edges(snapshot);
    let incoming = |node: &str| {
        snapshot
            .edges
            .iter()
            .filter(|edge| edge.to_node == node)
            .map(|edge| edge.from_node.as_str())
            .collect::<Vec<_>>()
    };
    let mut feeds = BTreeMap::<String, BTreeSet<String>>::new();
    for node in &snapshot.nodes {
        let mut visible = BTreeSet::new();
        let mut stack = incoming(&node.id);
        while let Some(source) = stack.pop() {
            if source == node.id || !visible.insert(source.to_owned()) {
                continue;
            }
            stack.extend(
                incoming(source)
                    .into_iter()
                    .filter(|prior| !back_edges.contains(&(*prior, source))),
            );
        }
        feeds.insert(node.id.clone(), visible);
    }
    for node in &snapshot.nodes {
        if let Some(op_name) = &node.op
            && let Some(call) = snapshot.ops[op_name].get("call")
        {
            if snapshot
                .edges
                .iter()
                .filter(|edge| edge.from_node == node.id)
                .count()
                > 1
            {
                return Err(invalid(format!(
                    "call node {:?} cannot choose between multiple outgoing edges",
                    node.id
                )));
            }
            if let Some(files) = call.get("files").and_then(Value::as_array) {
                for selection in files {
                    let source = selection
                        .get("node")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !feeds[&node.id].contains(source) {
                        return Err(invalid(format!(
                            "call node {:?}: file source {source:?} is not upstream",
                            node.id
                        )));
                    }
                }
            }
        }
        let wanted = owned_interface_files(snapshot, node, "reads");
        if wanted.is_empty() {
            continue;
        }
        let mut produced = owned_interface_files(snapshot, node, "writes")
            .into_iter()
            .collect::<BTreeSet<_>>();
        for source in &feeds[&node.id] {
            if let Some(source) = snapshot
                .nodes
                .iter()
                .find(|candidate| candidate.id == *source)
            {
                produced.extend(owned_interface_files(snapshot, source, "writes"));
            }
        }
        let missing = wanted
            .into_iter()
            .filter(|file| !produced.contains(file))
            .collect::<BTreeSet<_>>();
        if !missing.is_empty() {
            let handed = feeds[&node.id]
                .iter()
                .map(|source| {
                    let writes = snapshot
                        .nodes
                        .iter()
                        .find(|candidate| candidate.id == *source)
                        .map(|source| owned_interface_files(snapshot, source, "writes"))
                        .unwrap_or_default();
                    format!(
                        "{source} writes {}",
                        if writes.is_empty() {
                            "nothing".into()
                        } else {
                            writes.join(", ")
                        }
                    )
                })
                .collect::<Vec<_>>();
            return Err(invalid(format!(
                "node {:?} reads {}, and nothing it can be handed writes it. It can be handed: {}",
                node.id,
                missing.into_iter().collect::<Vec<_>>().join(", "),
                if handed.is_empty() {
                    "nothing".into()
                } else {
                    handed.join("; ")
                }
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_graph_conformance_fixtures_compile_to_their_expanded_snapshots() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/graph-conformance/python-v1.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let id = case["id"].as_str().unwrap();
            let compiled = GraphSnapshot::from_authoring(case["authoring_graph"].clone())
                .unwrap_or_else(|error| panic!("fixture {id:?} did not compile: {error}"));
            let expected = GraphSnapshot::admit(case["expanded_graph"].clone())
                .unwrap_or_else(|error| panic!("fixture {id:?} golden is invalid: {error}"));
            assert_eq!(
                compiled, expected,
                "Python expansion differs for fixture {id:?}"
            );
        }
    }

    #[test]
    fn layout_is_ignored_but_unknown_runtime_fields_are_reported() {
        let mut authoring = json!({
            "agents": {"worker": {"model": "test"}},
            "nodes": [{"id": "work", "agent": "worker"}],
            "layout": {"positions": {"work": {"x": 3, "y": 9}}, "custom": [1, 2]}
        });
        let compiled = GraphSnapshot::from_authoring(authoring.clone()).unwrap();
        assert_eq!(compiled.entry, "work");
        assert_eq!(compiled.nodes[0].id, "work");

        authoring["nodes"][0]["mystery_runtime_flag"] = Value::Bool(true);
        let error = GraphSnapshot::from_authoring(authoring)
            .unwrap_err()
            .to_string();
        assert!(error.contains("mystery_runtime_flag"), "{error}");
    }

    #[test]
    fn modules_inline_at_their_entry_and_exit_with_separate_round_budgets() {
        let authoring = json!({
            "agents": {"worker": {"model": "test"}},
            "entry": "start",
            "max_rounds": 2,
            "graphs": {"review": {
                "entry": "draft", "exit": "settle", "max_rounds": 4,
                "nodes": [
                    {"id": "draft", "agent": "worker", "with": "write"},
                    {"id": "check", "agent": "worker"},
                    {"id": "settle", "agent": "worker"}
                ],
                "edges": [
                    {"from": "draft", "to": "check"},
                    {"from": "check", "to": "draft"},
                    {"from": "check", "to": "settle"}
                ]
            }},
            "nodes": [
                {"id": "start", "agent": "worker"},
                {"id": "module", "graph": "review", "max_rounds": 3},
                {"id": "done", "agent": "worker"}
            ],
            "edges": [{"from": "start", "to": "module"}, {"from": "module", "to": "done"}]
        });
        let compiled = GraphSnapshot::from_authoring(authoring).unwrap();
        assert_eq!(compiled.entry, "start");
        assert_eq!(
            compiled.edges,
            vec![
                GraphEdge {
                    from_node: "start".into(),
                    to_node: "module/draft".into()
                },
                GraphEdge {
                    from_node: "module/draft".into(),
                    to_node: "module/check".into()
                },
                GraphEdge {
                    from_node: "module/check".into(),
                    to_node: "module/draft".into()
                },
                GraphEdge {
                    from_node: "module/check".into(),
                    to_node: "module/settle".into()
                },
                GraphEdge {
                    from_node: "module/settle".into(),
                    to_node: "done".into()
                },
            ]
        );
        assert_eq!(
            compiled
                .nodes
                .iter()
                .find(|node| node.id == "module/draft")
                .unwrap()
                .max_rounds,
            Some(4)
        );
        assert_eq!(compiled.module_rounds.get("module"), Some(&3));
        assert_eq!(
            compiled
                .nodes
                .iter()
                .find(|node| node.id == "module/draft")
                .unwrap()
                .input,
            Some(json!("write"))
        );
    }

    #[test]
    fn module_fanout_join_targets_are_bound_to_each_use_scope() {
        let authoring = json!({
            "agents": {"worker": {"model": "test"}},
            "ops": {"split": {"fanout": {"join": "collect"}}, "join": {"join": {}}},
            "entry": "first",
            "graphs": {"region": {
                "entry": "split", "exit": "collect",
                "nodes": [
                    {"id": "split", "op": "split"},
                    {"id": "left", "agent": "worker"},
                    {"id": "right", "agent": "worker"},
                    {"id": "collect", "op": "join"}
                ],
                "edges": [
                    {"from": "split", "to": "left"}, {"from": "split", "to": "right"},
                    {"from": "left", "to": "collect"}, {"from": "right", "to": "collect"}
                ]
            }},
            "nodes": [{"id": "first", "graph": "region"}, {"id": "second", "graph": "region"}],
            "edges": [{"from": "first", "to": "second"}]
        });
        let compiled = GraphSnapshot::from_authoring(authoring).unwrap();
        assert_eq!(compiled.entry, "first/split");
        assert_eq!(
            compiled.ops["split@first"]["fanout"]["join"],
            "first/collect"
        );
        assert_eq!(
            compiled.ops["split@second"]["fanout"]["join"],
            "second/collect"
        );
        assert_eq!(compiled.parallel_regions().unwrap().len(), 2);
    }

    #[test]
    fn authoring_diagnostics_reject_module_cycles_and_unproduced_reads() {
        let cyclic = json!({
            "agents": {"worker": {"model": "test"}},
            "graphs": {"a": {"entry": "again", "exit": "again", "nodes": [{"id": "again", "graph": "a"}]}},
            "nodes": [{"id": "start", "agent": "worker"}, {"id": "use", "graph": "a"}],
            "edges": [{"from": "start", "to": "use"}]
        });
        assert!(
            GraphSnapshot::from_authoring(cyclic)
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );

        let missing = json!({
            "agents": {"worker": {"model": "test", "reads": ["input.txt"]}},
            "nodes": [{"id": "work", "agent": "worker"}]
        });
        let error = GraphSnapshot::from_authoring(missing)
            .unwrap_err()
            .to_string();
        assert!(error.contains("reads input.txt"), "{error}");
    }
}
