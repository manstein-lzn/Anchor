//! A small recorded world, written through real kernels into two data roots
//! the way a Host writes them: Run histories in `kernel-runs`, an execution
//! port's observations in `kernel-facts`. Every time is set explicitly.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anchor_facts::{AgentConfiguration, Fact, Failure, Invocation, Observation, Outcome};
use anchor_kernel::{
    Action, Authority, Clock, FileStore, JudgeCtx, Judgement, Kernel, Phase, Principal,
    PrincipalKind, Resource, Submission, Validator, Verification,
};
use serde_json::{Value, json};

/// Business content that must never reach a projected row.
pub const SECRET: &str = "SECRET-PAYLOAD-7f3a";

#[derive(Clone, Default)]
pub struct TestClock(Arc<AtomicU64>);

impl TestClock {
    pub fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Anyone;

impl Authority for Anyone {
    fn permits(&self, _: &Principal, _: Action, _: &str) -> bool {
        true
    }
}

/// Accepts every write; `{"settle": true}` settles the task as done.
struct Step;

impl Validator for Step {
    fn judge(&self, ctx: &JudgeCtx, submission: &Submission) -> Judgement {
        let judgement = Judgement::accept(ctx.resource.state.clone(), Verification::Evidenced);
        if submission.content["settle"] == json!(true) {
            judgement.with_phase(Phase::Settled {
                result: anchor_kernel::Outcome::Done,
            })
        } else {
            judgement
        }
    }
}

pub struct World {
    pub dir: tempfile::TempDir,
    pub clock: TestClock,
    pub runs: Kernel<FileStore>,
    pub facts: Kernel<FileStore>,
    pub grades: Kernel<FileStore>,
}

pub fn executor() -> Principal {
    Principal::new("anchor/executor", PrincipalKind::Agent)
}

pub fn configuration(node: &str) -> AgentConfiguration {
    AgentConfiguration {
        version: anchor_facts::VERSION,
        graph_digest: "g".into(),
        node_id: node.into(),
        model: "flash".into(),
        model_binding: "b".into(),
        runtime_sha256: "goose".into(),
        instructions_sha256: anchor_facts::text_digest("work"),
        plugin_instructions_sha256: anchor_facts::text_digest(""),
        plugins: Vec::new(),
        routes: Vec::new(),
        host_sha256: "host".into(),
        tools_sha256: "tools".into(),
        envelope_sha256: anchor_facts::text_digest("{\"network\":false}"),
    }
}

pub fn turn(index: u64, observed: u64, elapsed: u64) -> Fact {
    Fact::ModelTurn {
        index,
        message_id: Some(format!("m{index}")),
        observed_at_ms: Some(observed),
        elapsed_ms: Some(elapsed),
        time_to_first_token_ms: Some(elapsed),
        input_tokens: Some(100 * index),
        output_tokens: Some(10),
        total_tokens: Some(100 * index + 10),
        cache_read_tokens: Some(50),
        cache_write_tokens: None,
        cost_usd: None,
        compaction: false,
    }
}

pub fn call(
    tool: &str,
    request: u64,
    arguments: Value,
    span: (u64, u64),
    failure: Option<Failure>,
) -> Fact {
    Fact::ToolCall {
        tool: tool.into(),
        tool_call_id: Some(format!("c{request}-{}", span.0)),
        model_request: request,
        arguments_shape: anchor_facts::arguments_shape(&arguments),
        path: arguments["path"].as_str().map(anchor_facts::path_class),
        program: anchor_facts::command_program(&arguments),
        shell_program: None,
        command: None,
        started_at_ms: Some(span.0),
        ended_at_ms: Some(span.1),
        ok: failure.is_none(),
        failure,
    }
}

pub fn lifecycle(span: (u64, u64), outcome: Outcome, turns: u64, calls: u64) -> Fact {
    Fact::Lifecycle {
        started_at_ms: span.0,
        ended_at_ms: span.1,
        outcome,
        model_turns: turns,
        tool_calls: calls,
        tool_calls_dropped: 0,
        resumed: false,
    }
}

impl World {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::default();
        let state = dir.path().join("host/state");
        std::fs::create_dir_all(state.join("kernel-runs")).unwrap();
        std::fs::create_dir_all(state.join("kernel-facts")).unwrap();
        std::fs::create_dir_all(state.join("kernel-grades")).unwrap();
        let mut runs = Kernel::with_clock(
            FileStore::new(state.join("kernel-runs")).unwrap(),
            Box::new(Anyone),
            Box::new(clock.clone()),
        )
        .unwrap();
        runs.install("validator/step", Arc::new(Step)).unwrap();
        let mut facts = Kernel::with_clock(
            FileStore::new(state.join("kernel-facts")).unwrap(),
            Box::new(anchor_facts::RecorderOnly),
            Box::new(clock.clone()),
        )
        .unwrap();
        anchor_facts::install(&mut facts).unwrap();
        let mut grades = Kernel::with_clock(
            FileStore::new(state.join("kernel-grades")).unwrap(),
            Box::new(anchor_facts::GraderOnly),
            Box::new(clock.clone()),
        )
        .unwrap();
        anchor_facts::install_grades(&mut grades).unwrap();
        Self {
            dir,
            clock,
            runs,
            facts,
            grades,
        }
    }

    pub fn state(&self) -> PathBuf {
        self.dir.path().join("host/state")
    }

    pub fn root(&self, name: &str) -> PathBuf {
        self.state().join(name)
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn run(&self, id: &str, at: u64) {
        self.clock.set(at);
        self.runs
            .create(
                Resource::new(format!("run/{id}"), "validator/step", Value::Null).task(u64::MAX),
            )
            .unwrap();
    }

    /// A node result, carrying business content the hub must not project.
    pub fn result(&self, run: &str, slot: &str, at: u64, cost: u64, settle: bool) {
        self.clock.set(at);
        let target = format!("run/{run}");
        let seen = self.runs.load(&target).unwrap().unwrap().version;
        self.runs
            .submit(
                &executor(),
                cost,
                Submission::new(target, seen, Action::Write)
                    .slot(slot)
                    .content(json!({"summary": SECRET, "settle": settle})),
            )
            .unwrap();
    }

    /// Grade `run` of `task` with `(check, passed)` pairs.
    pub fn grade(&self, run: &str, task: &str, checks: &[(&str, bool)], at: u64) {
        self.clock.set(at);
        let id = anchor_facts::grade_id(run);
        let _ = self
            .grades
            .create(Resource::new(&id, anchor_facts::GRADE, Value::Null));
        let seen = self.grades.load(&id).unwrap().unwrap().version;
        let grade = anchor_facts::Grade {
            version: anchor_facts::VERSION,
            run_id: run.into(),
            task: task.into(),
            task_version: 1,
            checks: checks
                .iter()
                .map(|(name, passed)| anchor_facts::Check {
                    name: (*name).into(),
                    passed: *passed,
                })
                .collect(),
        };
        let verdict = self
            .grades
            .submit(
                &anchor_facts::grader("grader-1"),
                0,
                Submission::new(id, seen, Action::Write)
                    .content(serde_json::to_value(grade).unwrap()),
            )
            .unwrap();
        assert!(verdict.is_accepted());
    }

    pub fn observe(
        &self,
        run: &str,
        node: &str,
        at: u64,
        config: &AgentConfiguration,
        facts: Vec<Fact>,
    ) {
        self.observe_as(run, node, at, &config.digest(), Some(config), facts);
    }

    /// Record observations under `digest`, with `manifest` as its manifest
    /// if given.
    pub fn observe_as(
        &self,
        run: &str,
        node: &str,
        at: u64,
        digest: &str,
        manifest: Option<&AgentConfiguration>,
        facts: Vec<Fact>,
    ) {
        self.clock.set(at);
        if let Some(manifest) = manifest {
            let _ = self.facts.create(
                Resource::new(
                    anchor_facts::configuration_id(&manifest.envelope_sha256),
                    anchor_facts::CONFIGURATION,
                    serde_json::json!({"network": manifest.envelope_sha256 == anchor_facts::text_digest("{\"network\":true}")}),
                )
                .configured(&manifest.envelope_sha256),
            );
            let _ = self.facts.create(
                Resource::new(
                    anchor_facts::configuration_id(digest),
                    anchor_facts::CONFIGURATION,
                    serde_json::to_value(manifest).unwrap(),
                )
                .configured(digest),
            );
        }
        let invocation = Invocation {
            run_id: run.into(),
            graph_digest: "g".into(),
            node_id: node.into(),
            invocation: 1,
        };
        let trace = invocation.trace();
        let _ = self.facts.create(
            Resource::new(&trace, anchor_facts::OBSERVATION, Value::Null).configured(digest),
        );
        let seen = self.facts.load(&trace).unwrap().unwrap().version;
        let verdict = self
            .facts
            .submit(
                &anchor_facts::recorder(digest),
                0,
                Submission::new(&trace, seen, Action::Write).content(
                    serde_json::to_value(Observation {
                        version: anchor_facts::VERSION,
                        invocation,
                        facts,
                    })
                    .unwrap(),
                ),
            )
            .unwrap();
        assert!(verdict.is_accepted());
    }
}

/// Run `r1`: an Op, an observed worker that failed a call and corrected it,
/// a node that spent model requests without being observed, and a check
/// that settles the Run; then a refused late write, and observations of an
/// invocation the Run never recorded.
pub fn r1(world: &World) {
    let worker = configuration("worker");
    world.run("r1", 1_000);
    world.result("r1", "prepare#1", 1_200, 0, false);
    world.observe(
        "r1",
        "worker",
        4_720,
        &worker,
        vec![
            // Turn 1 ran 1400..2400; its usage is observed only after the
            // call it asked for has run.
            turn(1, 2_460, 1_000),
            call(
                "anchor_run",
                1,
                json!({"command": "x"}),
                (2_400, 2_450),
                Some(Failure::NotAuthorized),
            ),
            turn(2, 3_601, 1_000),
            call(
                "anchor_run",
                2,
                json!({"command": ["x"]}),
                (3_480, 3_600),
                None,
            ),
            turn(3, 4_632, 1_000),
            call(
                "final_result",
                3,
                json!({"summary": SECRET}),
                (4_630, 4_631),
                None,
            ),
            lifecycle((1_300, 4_700), Outcome::Completed, 3, 3),
        ],
    );
    world.result("r1", "worker#1", 4_800, 3, false);
    world.result("r1", "critic#1", 6_000, 2, false);
    world.result("r1", "verify#1", 6_300, 0, true);
    // Settled: refused, and recorded as refused.
    world.clock.set(6_400);
    world
        .runs
        .submit(
            &executor(),
            0,
            Submission::new("run/r1", 0, Action::Write).slot("late#1"),
        )
        .unwrap();
    world.observe(
        "r1",
        "ghost",
        6_500,
        &configuration("ghost"),
        vec![
            turn(1, 6_470, 50),
            lifecycle((6_420, 6_490), Outcome::Failed, 1, 0),
        ],
    );
}

/// Run `r2`: still executing, with a worker that looped on one failing call,
/// one very slow call and a long silence between turns.
pub fn r2(world: &World) {
    let worker = configuration("worker");
    world.run("r2", 10_000);
    let bad = json!({"path": "/x"});
    world.observe(
        "r2",
        "worker",
        200_000,
        &worker,
        vec![
            turn(1, 11_100, 1_000),
            call(
                "anchor_read",
                1,
                bad.clone(),
                (11_100, 11_150),
                Some(Failure::NotAuthorized),
            ),
            turn(2, 12_200, 1_000),
            call(
                "anchor_read",
                2,
                bad.clone(),
                (12_200, 12_250),
                Some(Failure::NotAuthorized),
            ),
            turn(3, 13_300, 1_000),
            call(
                "anchor_read",
                3,
                bad,
                (13_300, 13_350),
                Some(Failure::NotAuthorized),
            ),
            turn(4, 14_400, 1_000),
            call(
                "anchor_run",
                4,
                json!({"command": ["sleep"]}),
                (14_400, 74_400),
                None,
            ),
            // A minute of nothing before the next request.
            turn(5, 136_000, 1_000),
            call(
                "final_result",
                5,
                json!({"summary": "s"}),
                (136_000, 136_001),
                None,
            ),
            lifecycle((10_050, 136_100), Outcome::Completed, 5, 5),
        ],
    );
    world.result("r2", "worker#1", 136_200, 5, false);
}
