//! The shared vocabulary of harness facts.
//!
//! An execution port records what it observed as ordinary kernel submissions
//! to resources it owns, in its own state root; the data hub reads them back
//! through [`anchor_kernel::HistoryReader`]. This crate is the only contract
//! between the two: resource names, validators and content schemas. It holds
//! no storage and no analysis.
//!
//! Facts carry no business content. A tool call is recorded by its name, the
//! shape of its arguments and how it ended, never by argument or result values.

use anchor_kernel::{
    Action, Authority, JudgeCtx, Judgement, Kernel, KernelError, Principal, PrincipalKind, Reason,
    Store, Submission, Validator, Verification,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};

/// Judges observation records: only host code may record, and only facts
/// about the invocation the resource is named after.
pub const OBSERVATION: &str = "validator/observation";
/// Configuration manifests are content-addressed and never change.
pub const CONFIGURATION: &str = "validator/configuration";
/// The host principal that records observations.
pub const RECORDER_ID: &str = "anchor/recorder";
/// The content schema version written by this crate.
pub const VERSION: u32 = 1;

/// The recorder, as the configuration it observed. Authority is judged by id
/// and kind only; the digest is recorded identity.
pub fn recorder(configuration: &str) -> Principal {
    Principal::new(RECORDER_ID, PrincipalKind::Code).with_config(configuration)
}

/// Install the validators of this vocabulary into a kernel.
pub fn install<S: Store>(kernel: &mut Kernel<S>) -> Result<(), KernelError> {
    kernel.install(OBSERVATION, Arc::new(Observations))?;
    kernel.install(CONFIGURATION, Arc::new(Immutable))
}

/// The authority of an observation root: the recorder writes traces.
pub struct RecorderOnly;

impl Authority for RecorderOnly {
    fn permits(&self, principal: &Principal, action: Action, target: &str) -> bool {
        principal.id == RECORDER_ID
            && principal.kind == PrincipalKind::Code
            && action == Action::Write
            && target.starts_with("trace/")
    }
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// One execution of one node in one Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invocation {
    pub run_id: String,
    pub graph_digest: String,
    pub node_id: String,
    pub invocation: u64,
}

impl Invocation {
    /// The resource holding this invocation's observations.
    pub fn trace(&self) -> String {
        format!("trace/{}/{}/{}", self.run_id, self.node_id, self.invocation)
    }
}

/// The resource holding the manifest with this digest.
pub fn configuration_id(digest: &str) -> String {
    format!("config/{digest}")
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// What the host froze for one AgentNode invocation. Its digest is the
/// configuration identity stamped on every observation of the invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfiguration {
    pub version: u32,
    pub graph_digest: String,
    pub node_id: String,
    pub model: String,
    pub model_binding: String,
    pub runtime_sha256: String,
    /// Digest of the node's instructions and task text.
    pub instructions_sha256: String,
    /// Digest of the Plugin instructions disclosed to the model.
    pub plugin_instructions_sha256: String,
    pub plugins: Vec<PluginIdentity>,
    pub routes: Vec<String>,
    /// Digest of the Host executable: every harness behavior it implements,
    /// such as tool refusals and path rules. Empty on manifests recorded
    /// before it was, so their digests do not change.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host_sha256: String,
    /// Digest of the tool definitions disclosed to the model, see
    /// [`tools_digest`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tools_sha256: String,
    /// Digest of the common invocation environment, whose canonical description
    /// is another immutable manifest at `configuration_id(envelope_sha256)`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub envelope_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PluginIdentity {
    pub id: String,
    pub digest: String,
    pub authorization: String,
}

impl AgentConfiguration {
    /// Order-insensitive collections are sorted so equal configurations have
    /// equal digests.
    pub fn normalized(mut self) -> Self {
        self.plugins.sort();
        self.routes.sort();
        self
    }

    pub fn digest(&self) -> String {
        let bytes = serde_json::to_vec(&self.clone().normalized()).expect("manifest serializes");
        format!("{:x}", Sha256::digest(bytes))
    }
}

/// The digest of tool definitions as (name, description, parameter schema):
/// independent of their order and of the key order of their schemas.
pub fn tools_digest<'a>(tools: impl IntoIterator<Item = (&'a str, &'a str, &'a Value)>) -> String {
    let mut tools: Vec<Value> = tools
        .into_iter()
        .map(|(name, description, parameters)| {
            Value::Array(vec![
                Value::from(name),
                Value::from(description),
                canonical(parameters),
            ])
        })
        .collect();
    tools.sort_by_key(|tool| tool.to_string());
    let bytes = serde_json::to_vec(&tools).expect("tools serialize");
    format!("{:x}", Sha256::digest(bytes))
}

/// Objects with their keys sorted, recursively. With serde_json's
/// `preserve_order`, which other crates in a build may enable, maps keep
/// insertion order, so equal values could otherwise hash differently.
pub fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonical(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// The digest of a text, for manifests that must not carry the text itself.
pub fn text_digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

// ---------------------------------------------------------------------------
// Observations
// ---------------------------------------------------------------------------

/// What the host observed of one invocation, recorded as the content of one
/// submission when the invocation ends. The facts happened at the times they
/// carry; one record holds them all because they are reported together, and a
/// record per fact would only multiply durable writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub version: u32,
    pub invocation: Invocation,
    pub facts: Vec<Fact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fact {
    /// What the workspace looked like, taken inside its own command sandbox.
    WorkspaceFingerprint {
        phase: FingerprintPhase,
        /// None for an unborn HEAD; never records a ref name or repository URL.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head: Option<String>,
        /// None when git is unavailable inside the command sandbox.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dirty: Option<bool>,
        /// Known lock files that exist, as relative path -> SHA-256.
        files: BTreeMap<String, String>,
    },
    /// When the invocation ran and how it ended.
    Lifecycle {
        started_at_ms: u64,
        ended_at_ms: u64,
        outcome: Outcome,
        model_turns: u64,
        tool_calls: u64,
        tool_calls_dropped: u64,
        resumed: bool,
    },
    /// One model response, as the host observed its usage report.
    ModelTurn {
        /// 1-based order within the invocation.
        index: u64,
        message_id: Option<String>,
        /// When the host saw the usage report. Goose reports a turn's usage
        /// once the turn is processed, which is after the tool calls it asked
        /// for have run: this is not when the response ended. A turn ends no
        /// later than its first tool call starts.
        observed_at_ms: Option<u64>,
        elapsed_ms: Option<u64>,
        time_to_first_token_ms: Option<u64>,
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
        cache_read_tokens: Option<u64>,
        cache_write_tokens: Option<u64>,
        cost_usd: Option<f64>,
        compaction: bool,
    },
    /// One tool call through the host bridge.
    ToolCall {
        tool: String,
        /// Goose's own id for the call, when it sent one.
        tool_call_id: Option<String>,
        /// How many model requests had been made when the call arrived: the
        /// call answers model request number `model_request`.
        model_request: u64,
        arguments_shape: String,
        /// Where a `path` argument pointed, for the tools that take one.
        /// Only the family, never the path: a host-side check can then be
        /// told apart from a mistake in the value without recording the
        /// value. Records from before this field read as `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathClass>,
        /// For a tool that runs a command, the base name of the program it
        /// was asked to run: a name the host's allowlist carries anyway, never
        /// its arguments. Records from before this field read as `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        program: Option<String>,
        /// Heuristic attribution hint: the first command's base name in a shell
        /// `-c` script (including short-option combinations such as `-lc`).
        /// This does not change `program`, which remains the interpreter.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell_program: Option<String>,
        /// For a tool that runs a command, how the command ended. The call
        /// succeeds whenever the host ran the command, so `ok` says nothing
        /// about the command itself. Records from before this field read as
        /// `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<CommandEnd>,
        started_at_ms: Option<u64>,
        ended_at_ms: Option<u64>,
        ok: bool,
        failure: Option<Failure>,
    },
}

/// The invocation boundary at which the workspace was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FingerprintPhase {
    Start,
    End,
}

/// How a command a tool ran ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CommandEnd {
    pub status: CommandStatus,
    /// The exit code, when the command exited.
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// The program started and exited, with whatever code.
    Exited,
    /// The sandbox could not start the program: it is not there, or not
    /// executable, inside the sandbox.
    NotStarted,
    /// The host stopped it at its time limit.
    TimedOut,
}

impl CommandEnd {
    /// Whether the command ran and exited 0.
    pub fn clean(&self) -> bool {
        self.status == CommandStatus::Exited && self.exit_code == Some(0)
    }
}

/// How a command ended, from a command tool's result: its `status`, its
/// `exit_code`, and whether the sandbox reported that it could not start the
/// program. The output is read for that report only and never recorded.
pub fn command_end(result: &Value) -> Option<CommandEnd> {
    let status = result.get("status")?.as_str()?;
    let exit_code = result
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok());
    let status = match status {
        "timed_out" => CommandStatus::TimedOut,
        _ if result
            .get("stderr")
            .and_then(Value::as_str)
            .is_some_and(|stderr| stderr.starts_with("bwrap: execvp ")) =>
        {
            CommandStatus::NotStarted
        }
        _ => CommandStatus::Exited,
    };
    Some(CommandEnd { status, exit_code })
}

/// The program a command tool was asked to run: the base name of the first
/// element of its `command` argument.
pub fn command_program(arguments: &Value) -> Option<String> {
    let first = arguments.get("command")?.as_array()?.first()?.as_str()?;
    let name = first.rsplit('/').next().unwrap_or(first);
    (!name.is_empty()).then(|| name.to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Failed,
    Cancelled,
    /// Ended without a known result; resumable after inspection.
    Interrupted,
}

/// Why a tool call did not succeed, as far as the host can tell without
/// reading its business content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    /// The host's sandbox or grants did not allow it.
    NotAuthorized,
    /// The arguments did not fit the tool's contract.
    InvalidArguments,
    /// The completion tool refused the attempt.
    CompletionRejected,
    Cancelled,
    /// The host refused to execute it for another reason.
    NotExecuted,
    /// The tool ran and reported an error.
    ToolError,
}

/// Classify a tool error by the host's own refusal wording. Only the class is
/// recorded, never the text.
///
/// Classes stay what they were recorded as, so experiments compare. A path
/// refused for leaving the read grant is `NotAuthorized` even when the wording
/// quotes the argument contract; the call's [`PathClass`] tells whether the
/// model addressed the wrong place.
pub fn classify_failure(error: &str) -> Failure {
    let error = error.to_ascii_lowercase();
    if error.contains("not authorized") || error.contains("must belong to an authorized") {
        Failure::NotAuthorized
    } else if error.contains("invalid arguments") || error.contains("missing field") {
        Failure::InvalidArguments
    } else if error.contains("final_result") || error.contains("observed_receipt") {
        Failure::CompletionRejected
    } else if error.contains("cancelled") {
        Failure::Cancelled
    } else if error.contains("not_executed") || error.contains("refused") {
        Failure::NotExecuted
    } else {
        Failure::ToolError
    }
}

/// The family a `path` argument belongs to, without the path itself. A tool
/// refuses a path either because the host does not let it go there or because
/// the model addressed the wrong place; this tells the two apart later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathClass {
    /// Relative to the workspace, and staying inside it.
    Relative,
    /// Absolute, under the workspace prefix.
    Workspace,
    /// Absolute, somewhere else.
    Absolute,
    /// Relative, but climbing out with `..`.
    Parent,
}

/// The family of a `path` argument. The path itself is never recorded.
pub fn path_class(path: &str) -> PathClass {
    if path.starts_with("/workspace/") {
        PathClass::Workspace
    } else if path.starts_with('/') {
        PathClass::Absolute
    } else if path.split('/').any(|segment| segment == "..") {
        PathClass::Parent
    } else {
        PathClass::Relative
    }
}

/// A digest of the structure of tool arguments: keys and value types, never
/// values. Equal shapes mean the model called the tool the same way.
pub fn arguments_shape(arguments: &Value) -> String {
    fn shape(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| (key.clone(), shape(value)))
                    .collect(),
            ),
            Value::Array(_) => Value::from("array"),
            Value::String(_) => Value::from("string"),
            Value::Number(_) => Value::from("number"),
            Value::Bool(_) => Value::from("bool"),
            Value::Null => Value::from("null"),
        }
    }
    let bytes = serde_json::to_vec(&canonical(&shape(arguments))).expect("shape serializes");
    format!("{:x}", Sha256::digest(bytes))
}

// ---------------------------------------------------------------------------
// Validators
// ---------------------------------------------------------------------------

struct Observations;

impl Validator for Observations {
    fn judge(&self, ctx: &JudgeCtx, submission: &Submission) -> Judgement {
        // An agent cannot report on itself; only host code records.
        if ctx.principal.kind != PrincipalKind::Code || submission.action != Action::Write {
            return Judgement::refused(Reason::Unauthorized);
        }
        match serde_json::from_value::<Observation>(submission.content.clone()) {
            Ok(observation)
                if observation.version == VERSION
                    && !observation.facts.is_empty()
                    && observation.invocation.trace() == ctx.resource.id =>
            {
                // The trace's own state stays empty: the facts are its history.
                Judgement::accept(ctx.resource.state.clone(), Verification::Asserted)
            }
            _ => Judgement::returned(Reason::ContentInvalid),
        }
    }
}

struct Immutable;

impl Validator for Immutable {
    fn judge(&self, _: &JudgeCtx, _: &Submission) -> Judgement {
        Judgement::refused(Reason::Unauthorized)
    }
}

// ---------------------------------------------------------------------------
// Grades
// ---------------------------------------------------------------------------

/// Judges grades: a person or deterministic host code judging a Run's
/// result against a task's checks.
pub const GRADE: &str = "validator/grade";
/// The host principal that records deterministic grades.
pub const GRADER_ID: &str = "anchor/grader";

/// The grader, as the grader configuration (task definition and checks) it
/// ran. Authority is judged by id and kind only.
pub fn grader(configuration: &str) -> Principal {
    Principal::new(GRADER_ID, PrincipalKind::Code).with_config(configuration)
}

/// The resource holding the grades of one Run.
pub fn grade_id(run_id: &str) -> String {
    format!("grade/{run_id}")
}

/// Install the grade validator into a kernel. Grades live in a root of their
/// own, owned by whoever grades.
pub fn install_grades<S: Store>(kernel: &mut Kernel<S>) -> Result<(), KernelError> {
    kernel.install(GRADE, Arc::new(Grades))
}

/// The authority of a grade root written by the deterministic grader.
pub struct GraderOnly;

impl Authority for GraderOnly {
    fn permits(&self, principal: &Principal, action: Action, target: &str) -> bool {
        principal.id == GRADER_ID
            && principal.kind == PrincipalKind::Code
            && action == Action::Write
            && target.starts_with("grade/")
    }
}

/// How one Run's result fared against a task's checks. Checks are named; a
/// grade carries no business values. A Run graded again, by a changed
/// grader for example, gets another record in the same history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grade {
    pub version: u32,
    pub run_id: String,
    /// The task the Run attempted, with the version of its definition.
    pub task: String,
    pub task_version: u32,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub passed: bool,
}

impl Grade {
    /// The share of checks passed.
    pub fn score(&self) -> f64 {
        if self.checks.is_empty() {
            return 0.0;
        }
        self.checks.iter().filter(|c| c.passed).count() as f64 / self.checks.len() as f64
    }

    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }
}

struct Grades;

impl Validator for Grades {
    fn judge(&self, ctx: &JudgeCtx, submission: &Submission) -> Judgement {
        // A model may not grade work, its own or another's.
        if ctx.principal.kind == PrincipalKind::Agent || submission.action != Action::Write {
            return Judgement::refused(Reason::Unauthorized);
        }
        match serde_json::from_value::<Grade>(submission.content.clone()) {
            Ok(grade)
                if grade.version == VERSION
                    && !grade.checks.is_empty()
                    && grade_id(&grade.run_id) == ctx.resource.id
                    && distinct(&grade.checks) =>
            {
                Judgement::accept(ctx.resource.state.clone(), Verification::Mechanical)
            }
            _ => Judgement::returned(Reason::ContentInvalid),
        }
    }
}

fn distinct(checks: &[Check]) -> bool {
    let mut names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
    names.sort_unstable();
    names.windows(2).all(|pair| pair[0] != pair[1])
}

#[cfg(test)]
mod tests;
