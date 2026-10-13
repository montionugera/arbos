//! The silent-model pulse, mocked against the 2026-10-12 arbos-updater
//! incident (22:33–22:44 UTC): a live worker turn produced no events for
//! eleven minutes while its model kept settling requests silently — no
//! content, no error, no `modelResponseRecovery` frame — so nothing mapped
//! and the status stayed `working`. The only trace was fx's usage ledger,
//! which recorded one `incident` per settled request ~300 s apart.
//!
//! The pulse under test reads that ledger while a live worker turn is
//! silent, and flips the worker to `waiting` through the same writer a
//! reported retry uses (PR 841's `note_worker_recovery`; the tab strip and
//! Main's agent list render the step unchanged). The ledger records below
//! are verbatim from the incident, timestamps and all: 22:33:41 (the dense
//! cadence's last settlement), 22:38:24 and 22:43:27 (the spaced ones).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::{Mutex, mpsc};

use zeron_engine::{EngineCore, HarnessRegistry, arbos_layout};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ModelRecoveryState, ReasoningLevel, RunRequest,
    SandboxLevel, SessionStatus, SteeringMode,
};

const CHAT: &str = "chat-pulse";
/// Pulse windows for every test in this file (process-global env knobs, set
/// once before any engine assembles).
const SILENT_MS: u64 = 400;
const POLL_MS: u64 = 100;

/// 22:33:41 UTC — the dense cadence's last settlement; the model goes quiet.
const SETTLED_2233: u64 = 1791844421558;
/// 22:38:24 UTC — the first spaced settlement inside the silent window.
const SETTLED_2238: u64 = 1791844704063;
/// 22:43:27 UTC — the second spaced settlement.
const SETTLED_2243: u64 = 1791845007214;

fn init_env() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: called before any engine (and thus any reader of the vars)
        // exists in this test process; all tests share the one value.
        unsafe {
            std::env::set_var("ZERON_SILENT_MODEL_MS", SILENT_MS.to_string());
            std::env::set_var("ZERON_SILENT_MODEL_POLL_MS", POLL_MS.to_string());
            // The turn under test is silent by design; the quiesce watchdog
            // must not park it first (fx's turns have an authoritative
            // prompt end, so the engine never quiesces them either).
            std::env::set_var("ZERON_TURN_QUIESCE_MS", "0");
        }
    });
}

fn run_request(cwd: &Path, prompt: &str) -> RunRequest {
    RunRequest {
        mcp: None,
        prompt: prompt.into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.display().to_string(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

fn session_started() -> AgentEvent {
    AgentEvent::SessionStarted {
        harness: HarnessId::Fx,
        model: "glm-5.3".into(),
        tools: vec![],
        cwd: "/tmp".into(),
        session_id: "hs-p".into(),
        assistant_message_id: "a-p".into(),
    }
}

fn text(t: &str) -> AgentEvent {
    AgentEvent::TextDelta { text: t.into() }
}

fn done() -> AgentEvent {
    AgentEvent::Done {
        status: DoneStatus::Completed,
        result: None,
        error: None,
        session_id: Some("hs-p".into()),
    }
}

/// Exactly what the harness normalizer produces for the frame fx 0.0.13
/// emitted on the incident's retries (captured live from the installed
/// binary; the fixture `fx_model_recovery_first_retry_of_the_incident_maps`
/// in `app/crates/harness/src/acp/normalize.rs` pins the frame → event half
/// of that contract).
fn first_retry_frame() -> AgentEvent {
    AgentEvent::ModelRecovery {
        state: ModelRecoveryState::Active,
        cause: Some("provider_unavailable".into()),
        action: Some("retrying_request".into()),
        required_action: None,
        failed_attempt: Some(1),
        attempt_limit: Some(10),
        delay_seconds: Some(2),
        retry_deadline: None,
        diagnostic: Some(
            "⚠ Provider unavailable · HTTP 503 · server_error: provider unavailable · retrying request in 2s".into(),
        ),
    }
}

fn recovered_frame() -> AgentEvent {
    AgentEvent::ModelRecovery {
        state: ModelRecoveryState::Recovered,
        cause: None,
        action: None,
        required_action: None,
        failed_attempt: None,
        attempt_limit: None,
        delay_seconds: None,
        retry_deadline: None,
        diagnostic: None,
    }
}

/// One fx usage-ledger record, in the exact shape fx 0.0.13 writes.
fn incident_record(ms: u64) -> String {
    format!(
        "{{\"schema_version\":1,\"kind\":\"incident\",\"occurred_at_ms\":{ms},\
         \"completeness\":\"incomplete\"}}\n"
    )
}

/// The worker's ledger as the incident left it at 22:33:41 UTC: fx's
/// coverage header, then the dense incident cadence of a turn at work.
fn write_window_ledger(path: &Path) {
    let mut text = String::from(
        "{\"schema_version\":1,\"kind\":\"coverage\",\"started_at_ms\":1791834492203}\n",
    );
    for ms in [1791844307724, 1791844353713, SETTLED_2233] {
        text.push_str(&incident_record(ms));
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A request settles: fx appends one incident record, as it did at 22:38:24.
fn settle(path: &Path, ms: u64) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(incident_record(ms).as_bytes()).unwrap();
}

/// Feed-by-hand harness standing in for fx (see turn_quiesce.rs): the test
/// pushes events through a channel, so it can model a turn that speaks once
/// and then goes silent for minutes. The feed is served only to the test's
/// own dispatch (matched by prompt) — the wake a finished worker sends Main
/// runs the same harness and gets an immediately-completed stream instead.
struct FeedHarness {
    main_prompt: String,
    feed: Mutex<Option<mpsc::UnboundedReceiver<AgentEvent>>>,
}

#[async_trait]
impl Harness for FeedHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Fx
    }
    fn display_name(&self) -> &str {
        "Feed"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    /// fx owns its turn until the request settles: a silent model request
    /// is still a live turn, never a quiesce candidate.
    fn authoritative_prompt_end(&self) -> bool {
        true
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        mut controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        if request.prompt != self.main_prompt {
            // Some other run of this harness (Main's wake): complete at once.
            return Ok(futures::stream::iter(vec![Ok(done())]).boxed());
        }
        let mut feed = self
            .feed
            .lock()
            .await
            .take()
            .expect("FeedHarness serves the main dispatch once per test");
        let (tx, rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(64);
        tokio::spawn(async move {
            let mut steering_open = true;
            loop {
                tokio::select! {
                    biased;
                    steer = controls.steering.recv(), if steering_open => match steer {
                        Some(_) => {
                            let boundary = AgentEvent::Steered {
                                assistant_message_id: None,
                                next_assistant_message_id: None,
                            };
                            if tx.send(Ok(boundary)).await.is_err() {
                                return;
                            }
                        }
                        None => steering_open = false,
                    },
                    event = feed.recv() => match event {
                        Some(event) => {
                            if tx.send(Ok(event)).await.is_err() {
                                return;
                            }
                        }
                        None => return,
                    },
                }
            }
        });
        Ok(futures::stream::unfold(rx, |mut rx| async {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

struct Rig {
    core: EngineCore,
    feed: mpsc::UnboundedSender<AgentEvent>,
    project: PathBuf,
    ledger: PathBuf,
    _dir: tempfile::TempDir,
}

fn assemble(main_prompt: &str) -> Rig {
    init_env();
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    arbos_layout::ensure(&project, Some("Pulse")).expect("project layout assembles");
    arbos_layout::claim_main(&project, "main-chat").unwrap();
    arbos_layout::reserve_worker(&project, "oak", "Keep the app current", "glm-5.3", None)
        .expect("worker reserves");
    arbos_layout::bind_worker_chat(&project, "oak", CHAT).unwrap();
    let ledger = project
        .join(".arbos")
        .join("agents")
        .join("oak")
        .join("fx")
        .join(".fx")
        .join("usage.jsonl");
    write_window_ledger(&ledger);

    let (feed, rx) = mpsc::unbounded_channel();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(FeedHarness {
        main_prompt: main_prompt.into(),
        feed: Mutex::new(Some(rx)),
    }));
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Fx, None)
        .expect("engine core assembles");
    Rig {
        core,
        feed,
        project,
        ledger,
        _dir: dir,
    }
}

impl Rig {
    async fn drive(&self, prompt: &str) {
        self.core
            .sessions
            .dispatch(
                CHAT,
                HarnessId::Fx,
                run_request(&self.project, prompt),
                None,
            )
            .await
            .expect("dispatch");
    }

    fn worker(&self) -> arbos_layout::AgentView {
        arbos_layout::worker_result(&self.project, "oak").expect("worker row")
    }

    fn agent_toml(&self) -> String {
        std::fs::read_to_string(
            self.project
                .join(".arbos")
                .join("agents")
                .join("oak")
                .join("agent.toml"),
        )
        .unwrap()
    }
}

async fn wait_for<F>(rig: &Rig, mut predicate: F, what: &str)
where
    F: FnMut(&Rig) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate(rig) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; worker = {:#?}",
            rig.worker()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The incident itself: a worker's turn speaks once, then goes silent while
/// its model settles requests ~300 s apart. The pulse baselines the ledger
/// at 22:33:41, the 22:38:24 settlement flips the worker to `waiting` with
/// the step line in its file and its tab row, the 22:43:27 settlement
/// changes nothing (the pulse speaks once), and the first event back — the
/// dense cadence resuming at 22:43:34 — restores `working` with the step
/// cleared. The turn can still end normally afterwards.
#[tokio::test]
async fn a_silent_turn_with_spaced_incidents_waits_then_any_event_restores_working() {
    let rig = assemble("check for app updates");
    rig.drive("check for app updates").await;

    rig.feed.send(session_started()).unwrap();
    rig.feed.send(text("Reading the release feed.")).unwrap();
    // The turn goes quiet past the pulse's silence window, so the ledger is
    // baselined at 22:33:41 — normal work, not a wait signal.
    tokio::time::sleep(Duration::from_millis(SILENT_MS + 300)).await;
    assert_eq!(rig.worker().status, "working");

    // 22:38:24: the first spaced settlement lands. Within one poll the
    // worker reads `waiting`.
    settle(&rig.ledger, SETTLED_2238);
    wait_for(
        &rig,
        |rig| rig.worker().status == "waiting",
        "a silent turn with a fresh settlement waits on the model",
    )
    .await;

    let view = rig.worker();
    assert_eq!(view.status, "waiting");
    assert_eq!(view.step, "Waiting on the model");
    // The file Main and the person read back.
    let toml = rig.agent_toml();
    assert!(toml.contains("status = \"waiting\""), "{toml}");
    assert!(toml.contains("step = \"Waiting on the model\""), "{toml}");
    // The tab row: the strip keys a live worker's step by chat id.
    let row = arbos_layout::agents(&rig.project)
        .into_iter()
        .find(|agent| agent.chat_id == CHAT)
        .expect("the worker is in the agent list");
    assert_eq!(
        (row.status.as_str(), row.step.as_str()),
        ("waiting", "Waiting on the model")
    );
    // The session itself is still a live turn.
    assert_eq!(
        rig.core.sessions.session_status(CHAT).map(|s| s.status),
        Some(SessionStatus::Working)
    );

    // 22:43:27: the second spaced settlement changes nothing — the pulse
    // has already spoken.
    settle(&rig.ledger, SETTLED_2243);
    tokio::time::sleep(Duration::from_millis(POLL_MS * 3)).await;
    let view = rig.worker();
    assert_eq!(
        (view.status.as_str(), view.step.as_str()),
        ("waiting", "Waiting on the model")
    );

    // 22:43:34: the dense cadence resumes and content flows again — any
    // event restores `working` with the step cleared.
    rig.feed
        .send(text("Back online; writing the report."))
        .unwrap();
    wait_for(
        &rig,
        |rig| rig.worker().status == "working",
        "any event restores working",
    )
    .await;
    let view = rig.worker();
    assert_eq!((view.status.as_str(), view.step.as_str()), ("working", ""));

    // And the turn still ends like any other.
    rig.feed.send(done()).unwrap();
    wait_for(&rig, |rig| rig.worker().status == "done", "the turn ends").await;
    rig.core.sessions.shutdown().await;
}

/// The guard the incident's control case demands: a healthy worker logs
/// incidents constantly while it works (the measured control logged 39 in
/// eleven minutes), so incidents are a wait signal only under silence. The
/// same spaced window records arriving while events flow must leave the
/// worker `working` throughout.
#[tokio::test]
async fn incidents_while_events_flow_keep_the_worker_working() {
    let rig = assemble("check for app updates");
    rig.drive("check for app updates").await;

    rig.feed.send(session_started()).unwrap();
    // Settements keep landing — including the window's own spaced records
    // and the dense tail behind them — while the turn keeps producing
    // events well inside the silence window.
    let dense_tail = [1791845014057, 1791845021671, 1791845383634];
    for round in 0..18usize {
        settle(&rig.ledger, SETTLED_2238 + round as u64);
        settle(
            &rig.ledger,
            dense_tail[round % dense_tail.len()] + round as u64,
        );
        rig.feed
            .send(text("Still working through the checklist."))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
        let view = rig.worker();
        assert_eq!(
            (view.status.as_str(), view.step.as_str()),
            ("working", ""),
            "round {round}: an incident while events flow is not a wait"
        );
    }

    rig.feed.send(done()).unwrap();
    wait_for(&rig, |rig| rig.worker().status == "done", "the turn ends").await;
    rig.core.sessions.shutdown().await;
}

/// The frame fx actually emits when it retries — the incident's provider
/// failures did produce these, on other requests — maps end to end to the
/// waiting line PR 841 promised, and a `recovered` frame clears it. This is
/// the engine half of the contract; the harness fixture pinned the frame →
/// event half.
#[tokio::test]
async fn a_reported_retry_frame_waits_with_its_own_line_and_recovers() {
    let rig = assemble("check for app updates");
    rig.drive("check for app updates").await;

    rig.feed.send(session_started()).unwrap();
    rig.feed.send(first_retry_frame()).unwrap();
    wait_for(
        &rig,
        |rig| rig.worker().status == "waiting",
        "a reported retry waits",
    )
    .await;
    let view = rig.worker();
    assert_eq!(view.status, "waiting");
    assert_eq!(view.step, "Waiting on the model — try 1 of 10, next in 2 s");
    let toml = rig.agent_toml();
    assert!(toml.contains("status = \"waiting\""), "{toml}");
    assert!(
        toml.contains("step = \"Waiting on the model — try 1 of 10, next in 2 s\""),
        "{toml}"
    );

    rig.feed.send(recovered_frame()).unwrap();
    wait_for(
        &rig,
        |rig| rig.worker().status == "working",
        "a recovered frame restores working",
    )
    .await;
    assert_eq!(rig.worker().step, "");

    rig.feed.send(done()).unwrap();
    wait_for(&rig, |rig| rig.worker().status == "done", "the turn ends").await;
    rig.core.sessions.shutdown().await;
}

/// The hand-off the pulse must not fumble: after it flips the worker on its
/// own, a real recovery frame arriving as the very next event rewrites the
/// line with its own words instead of leaving the worker `working`. The
/// restore-on-event runs first, the frame's mapping second.
#[tokio::test]
async fn a_recovery_frame_after_the_pulse_flip_takes_the_status_back_over() {
    let rig = assemble("check for app updates");
    rig.drive("check for app updates").await;

    rig.feed.send(session_started()).unwrap();
    rig.feed.send(text("Reading the release feed.")).unwrap();
    tokio::time::sleep(Duration::from_millis(SILENT_MS + 300)).await;
    settle(&rig.ledger, SETTLED_2238);
    wait_for(
        &rig,
        |rig| rig.worker().status == "waiting",
        "the pulse flips the silent worker to waiting",
    )
    .await;
    assert_eq!(rig.worker().step, "Waiting on the model");

    // fx starts reporting the retry it was quietly pacing all along.
    rig.feed.send(first_retry_frame()).unwrap();
    wait_for(
        &rig,
        |rig| rig.worker().step == "Waiting on the model — try 1 of 10, next in 2 s",
        "the frame's own line takes over",
    )
    .await;
    assert_eq!(rig.worker().status, "waiting");

    rig.feed.send(recovered_frame()).unwrap();
    wait_for(
        &rig,
        |rig| rig.worker().status == "working",
        "recovery over restores working",
    )
    .await;
    assert_eq!(rig.worker().step, "");

    rig.feed.send(done()).unwrap();
    wait_for(&rig, |rig| rig.worker().status == "done", "the turn ends").await;
    rig.core.sessions.shutdown().await;
}
