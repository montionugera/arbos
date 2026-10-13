//! The silent-model pulse, mocked against the 2026-10-13 rig stall: a live
//! worker turn on engy produced no events for ten minutes while its model
//! sent nothing — no content, no error, no `modelResponseRecovery` frame (fx
//! waits on a response head without limit and calls a started stream
//! stalled only after ten minutes) — and the worker's `agent.toml` stayed
//! `working` throughout. The first pulse (PR 849) read fx's usage ledger and
//! waited for an `incident` newer than its baseline; a stalled request
//! settles nothing, so that never came.
//!
//! The pulse under test treats the silence itself as the signal: a live
//! worker turn with no event for the window and nothing still running flips
//! to `waiting` through the same writer a reported retry uses (PR 841's
//! `note_worker_recovery`; the tab strip and Main's agent list render the
//! step unchanged), and the next event of any kind restores `working`.

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
    SandboxLevel, SessionStatus, SteeringMode, ToolCall,
};

const CHAT: &str = "chat-pulse";
/// The pulse window for every test in this file (a process-global env knob,
/// set once before any engine assembles).
const SILENT_MS: u64 = 400;
/// Long enough past the window for the pulse's tick to have run.
const PAST_WINDOW_MS: u64 = SILENT_MS + 300;

fn init_env() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: called before any engine (and thus any reader of the vars)
        // exists in this test process; all tests share the one value.
        unsafe {
            std::env::set_var("ZERON_SILENT_MODEL_MS", SILENT_MS.to_string());
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

fn tool_call(id: &str, command: &str) -> AgentEvent {
    AgentEvent::ToolCall {
        id: id.into(),
        call: ToolCall::Exec {
            command: command.into(),
        },
    }
}

fn tool_result(id: &str) -> AgentEvent {
    AgentEvent::ToolResult {
        id: id.into(),
        is_error: false,
        output: Some("ok\n".into()),
        diff: None,
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

/// The rig stall itself: a worker's turn speaks once, then its model sends
/// nothing — no frame, nothing settled, nothing on the wire. One silence
/// window later the worker reads `waiting` with the step line in its file
/// and its tab row; more silence changes nothing (the pulse speaks once);
/// the first event back restores `working` with the step cleared. The turn
/// can still end normally afterwards.
#[tokio::test]
async fn a_silent_turn_waits_on_the_model_within_the_window_and_any_event_restores_working() {
    let rig = assemble("check for app updates");
    rig.drive("check for app updates").await;

    rig.feed.send(session_started()).unwrap();
    rig.feed.send(text("Reading the release feed.")).unwrap();
    wait_for(
        &rig,
        |rig| rig.worker().status == "waiting",
        "a silent turn waits on the model",
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

    // Another window of nothing changes nothing — the pulse has spoken.
    tokio::time::sleep(Duration::from_millis(PAST_WINDOW_MS)).await;
    let view = rig.worker();
    assert_eq!(
        (view.status.as_str(), view.step.as_str()),
        ("waiting", "Waiting on the model")
    );

    // The model answers: any event restores `working` with the step cleared.
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

/// A healthy worker is silent only between its own events: while they keep
/// arriving inside the window the worker stays `working` throughout.
#[tokio::test]
async fn events_inside_the_window_keep_the_worker_working() {
    let rig = assemble("check for app updates");
    rig.drive("check for app updates").await;

    rig.feed.send(session_started()).unwrap();
    for round in 0..12usize {
        rig.feed
            .send(text("Still working through the checklist."))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(SILENT_MS / 4)).await;
        let view = rig.worker();
        assert_eq!(
            (view.status.as_str(), view.step.as_str()),
            ("working", ""),
            "round {round}: a turn that keeps speaking is not waiting"
        );
    }

    rig.feed.send(done()).unwrap();
    wait_for(&rig, |rig| rig.worker().status == "done", "the turn ends").await;
    rig.core.sessions.shutdown().await;
}

/// A long tool call is silent too, and that silence is the command's, not
/// the model's: the pulse holds while the call is open and runs from the
/// result on.
#[tokio::test]
async fn an_open_tool_call_holds_the_pulse_until_its_result() {
    let rig = assemble("check for app updates");
    rig.drive("check for app updates").await;

    rig.feed.send(session_started()).unwrap();
    rig.feed.send(tool_call("t1", "cargo test")).unwrap();
    tokio::time::sleep(Duration::from_millis(PAST_WINDOW_MS)).await;
    let view = rig.worker();
    assert_eq!(
        (view.status.as_str(), view.step.as_str()),
        ("working", ""),
        "a running command is not the model stalling"
    );

    // The command finishes and the model goes quiet on the result: now the
    // silence is the model's.
    rig.feed.send(tool_result("t1")).unwrap();
    wait_for(
        &rig,
        |rig| rig.worker().status == "waiting",
        "silence after the result waits on the model",
    )
    .await;
    assert_eq!(rig.worker().step, "Waiting on the model");

    rig.feed.send(text("Tests green.")).unwrap();
    wait_for(
        &rig,
        |rig| rig.worker().status == "working",
        "the model answering restores working",
    )
    .await;
    rig.feed.send(done()).unwrap();
    wait_for(&rig, |rig| rig.worker().status == "done", "the turn ends").await;
    rig.core.sessions.shutdown().await;
}

/// A parked worker keeps one recovery tracker across its turns, and the
/// pulse runs only while that tracker is clear. A retry line the first
/// turn's `Done` outlived (fx sent `active`, never `recovered`) used to
/// leave the tracker busy for ever, so the next turn's stall fell through
/// with the pulse switched off.
#[tokio::test]
async fn a_retry_the_first_turn_outlived_does_not_silence_the_next_turns_pulse() {
    let rig = assemble("check for app updates");
    rig.drive("check for app updates").await;

    rig.feed.send(session_started()).unwrap();
    rig.feed.send(first_retry_frame()).unwrap();
    wait_for(
        &rig,
        |rig| rig.worker().status == "waiting",
        "the reported retry waits",
    )
    .await;
    // fx ends the turn without ever clearing the line.
    rig.feed.send(done()).unwrap();
    wait_for(&rig, |rig| rig.worker().status == "done", "the turn ends").await;
    wait_for(
        &rig,
        |rig| rig.core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Idle),
        "the session parks",
    )
    .await;

    // Main sends the worker its next brief: the worker is working again and
    // the steered turn goes silent at once.
    arbos_layout::restart_worker(&rig.project, "oak").unwrap();
    rig.core
        .sessions
        .steer(CHAT, "now check the edge channel", None)
        .await
        .expect("steer accepted");
    wait_for(
        &rig,
        |rig| rig.worker().status == "waiting",
        "the next turn's stall still waits on the model",
    )
    .await;
    assert_eq!(rig.worker().step, "Waiting on the model");

    rig.feed.send(text("Edge is current.")).unwrap();
    wait_for(
        &rig,
        |rig| rig.worker().status == "working",
        "the model answering restores working",
    )
    .await;
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
