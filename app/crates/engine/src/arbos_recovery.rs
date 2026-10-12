//! What a run does with fx's model-response recovery. fx retries a failed
//! model request with backoff, pauses the turn when it cannot go on, or gives
//! up; each change arrives as [`AgentEvent::ModelRecovery`]. The tracker here
//! turns those into one plain line for the person and the worker's status
//! word, holds a paused turn open for one automatic resume, and marks the
//! worker `error` when a pause outlives [`PAUSE_LIMIT`].

use std::time::{Duration, Instant};

use zeron_proto::{AgentEvent, ModelRecoveryState};

/// A pause that has not resumed after this long is an error.
pub const PAUSE_LIMIT: Duration = Duration::from_secs(5 * 60);

/// Bounds on fx's advised delay before the automatic resume. The upper bound
/// keeps the resume ahead of [`PAUSE_LIMIT`].
const RESUME_DELAY_MIN: Duration = Duration::from_secs(5);
const RESUME_DELAY_MAX: Duration = Duration::from_secs(240);
/// Used when fx advises no delay at all.
const RESUME_DELAY_DEFAULT: Duration = Duration::from_secs(30);

/// fx's word for a pause the client may continue with an empty prompt.
const CONTINUE_LATER: &str = "continue_later";

/// What the engine writes for one recovery change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryEffect {
    /// The model is being retried or has paused: `status = "waiting"` and
    /// `step` is this line.
    Waiting { step: String },
    /// The model answers again: `status = "working"`, step cleared.
    Working,
    /// The model gave up, paused twice, or paused too long: `status =
    /// "error"` with `reason` as the result, and Main is woken.
    Error { reason: String },
}

/// What a turn's `Done` means while recovery is in play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    /// The turn ended; the worker is done and Main is woken as usual.
    Over,
    /// The turn is paused, not over: the worker stays `waiting`. When set,
    /// `resume_at` is when the automatic resume goes out.
    Held { resume_at: Option<Instant> },
}

#[derive(Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Clear,
    Active,
    Paused {
        since: Instant,
        /// fx said `continue_later`: an empty prompt resumes the turn.
        resumable: bool,
        delay: Duration,
        reason: String,
    },
    Failed,
}

/// Per-run recovery state. Time is passed in so tests can move it.
#[derive(Debug, Default)]
pub struct RecoveryTracker {
    phase: Phase,
    /// Automatic resumes spent on the current turn.
    resumes: u8,
    /// When the resume prompt goes out; set when the paused turn ends.
    resume_at: Option<Instant>,
    /// The line last written as the step, so a clear can leave Main's own
    /// step alone.
    pub last_step: String,
}

impl RecoveryTracker {
    /// Take one event. `None` for events that are not recovery, or that
    /// change nothing.
    pub fn observe(&mut self, event: &AgentEvent, now: Instant) -> Option<RecoveryEffect> {
        let AgentEvent::ModelRecovery {
            state,
            cause,
            action,
            required_action,
            failed_attempt,
            attempt_limit,
            delay_seconds,
            diagnostic,
            ..
        } = event
        else {
            return None;
        };
        let reason = reason_text(diagnostic.as_deref(), cause.as_deref());
        let effect = match state {
            ModelRecoveryState::Active => {
                if self.phase == Phase::Failed {
                    return None;
                }
                self.phase = Phase::Active;
                RecoveryEffect::Waiting {
                    step: active_line(
                        action.as_deref(),
                        *failed_attempt,
                        *attempt_limit,
                        *delay_seconds,
                    ),
                }
            }
            ModelRecoveryState::Paused => {
                if self.phase == Phase::Failed {
                    return None;
                }
                let resumable = required_action.as_deref() == Some(CONTINUE_LATER);
                if resumable && self.resumes > 0 {
                    self.phase = Phase::Failed;
                    self.resume_at = None;
                    RecoveryEffect::Error {
                        reason: format!("The model paused again: {reason}"),
                    }
                } else {
                    self.phase = Phase::Paused {
                        since: now,
                        resumable,
                        delay: delay_seconds
                            .map(Duration::from_secs)
                            .unwrap_or(RESUME_DELAY_DEFAULT),
                        reason: reason.clone(),
                    };
                    RecoveryEffect::Waiting {
                        step: format!("The model paused: {reason}"),
                    }
                }
            }
            ModelRecoveryState::Failed => {
                self.phase = Phase::Failed;
                self.resume_at = None;
                RecoveryEffect::Error {
                    reason: format!("The model gave up: {reason}"),
                }
            }
            ModelRecoveryState::Recovered => match self.phase {
                Phase::Clear | Phase::Failed => return None,
                Phase::Active | Phase::Paused { .. } => {
                    self.phase = Phase::Clear;
                    self.resume_at = None;
                    RecoveryEffect::Working
                }
            },
        };
        self.last_step = match &effect {
            RecoveryEffect::Waiting { step } => step.clone(),
            RecoveryEffect::Working | RecoveryEffect::Error { .. } => String::new(),
        };
        Some(effect)
    }

    /// A paused turn is not over when its `Done` lands: the worker stays
    /// `waiting` and Main is not woken.
    pub fn holds_turn(&self) -> bool {
        matches!(self.phase, Phase::Paused { .. })
    }

    /// The turn's `Done` arrived. A pause holds the turn open: a resumable
    /// one schedules its one automatic resume and says when it goes out,
    /// the rest wait for the five-minute rule. A resumed turn that ends with
    /// no new pause is over, and so is any turn that ended clean; either
    /// gives the next turn a fresh resume.
    pub fn turn_ended(&mut self, now: Instant) -> TurnEnd {
        match &self.phase {
            Phase::Paused {
                resumable: true,
                delay,
                ..
            } if self.resumes == 0 => {
                self.resumes = 1;
                let at = now + (*delay).clamp(RESUME_DELAY_MIN, RESUME_DELAY_MAX);
                self.resume_at = Some(at);
                TurnEnd::Held {
                    resume_at: Some(at),
                }
            }
            Phase::Paused { .. } if self.resumes == 0 => TurnEnd::Held { resume_at: None },
            Phase::Paused { .. } | Phase::Clear => {
                self.phase = Phase::Clear;
                self.resumes = 0;
                self.resume_at = None;
                self.last_step.clear();
                TurnEnd::Over
            }
            Phase::Active | Phase::Failed => TurnEnd::Over,
        }
    }

    /// When the scheduled resume is due.
    pub fn resume_at(&self) -> Option<Instant> {
        self.resume_at
    }

    /// The resume prompt went out.
    pub fn take_resume(&mut self) -> bool {
        self.resume_at.take().is_some()
    }

    /// When the pause turns into an error.
    pub fn error_at(&self) -> Option<Instant> {
        match &self.phase {
            Phase::Paused { since, .. } => Some(*since + PAUSE_LIMIT),
            Phase::Clear | Phase::Active | Phase::Failed => None,
        }
    }

    /// The clock moved. A pause older than [`PAUSE_LIMIT`] is an error.
    pub fn tick(&mut self, now: Instant) -> Option<RecoveryEffect> {
        let Phase::Paused { since, reason, .. } = &self.phase else {
            return None;
        };
        if now < *since + PAUSE_LIMIT {
            return None;
        }
        let reason = format!("The model paused and did not resume: {reason}");
        self.phase = Phase::Failed;
        self.resume_at = None;
        self.last_step.clear();
        Some(RecoveryEffect::Error { reason })
    }

    /// The run is over. A pause still open cannot resume any more.
    pub fn end(&mut self) -> Option<RecoveryEffect> {
        let Phase::Paused { reason, .. } = &self.phase else {
            return None;
        };
        let reason = format!("The model paused and the session ended: {reason}");
        self.phase = Phase::Failed;
        self.resume_at = None;
        self.last_step.clear();
        Some(RecoveryEffect::Error { reason })
    }
}

/// "Waiting on the model — try 3 of 8, next in 32 s", with the parts fx
/// did not send left out.
fn active_line(
    action: Option<&str>,
    attempt: Option<u64>,
    limit: Option<u64>,
    delay: Option<u64>,
) -> String {
    let head = match action {
        Some("waiting_for_connectivity") => "Waiting for the connection",
        Some("checking_liveness") => "Checking the connection",
        _ => "Waiting on the model",
    };
    let mut parts = Vec::new();
    match (attempt, limit) {
        (Some(attempt), Some(limit)) => parts.push(format!("try {attempt} of {limit}")),
        (Some(attempt), None) => parts.push(format!("try {attempt}")),
        (None, _) => {}
    }
    if let Some(delay) = delay {
        parts.push(format!("next in {delay} s"));
    }
    if parts.is_empty() {
        head.to_owned()
    } else {
        format!("{head} — {}", parts.join(", "))
    }
}

/// fx's label without its warning glyph, else its cause word in plain
/// words, else a stand-in.
fn reason_text(diagnostic: Option<&str>, cause: Option<&str>) -> String {
    if let Some(text) = diagnostic {
        let text = text.trim().trim_start_matches(['⚠', '✓']).trim();
        if !text.is_empty() {
            return text.to_owned();
        }
    }
    match cause {
        Some(word) if !word.is_empty() => word.replace('_', " "),
        _ => "no reason given".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::{arbos_layout, arbos_live};

    fn recovery(
        state: ModelRecoveryState,
        required_action: Option<&str>,
        attempt: Option<u64>,
        delay: Option<u64>,
        diagnostic: Option<&str>,
    ) -> AgentEvent {
        AgentEvent::ModelRecovery {
            state,
            cause: Some("provider_unavailable".into()),
            action: None,
            required_action: required_action.map(str::to_owned),
            failed_attempt: attempt,
            attempt_limit: attempt.map(|_| 8),
            delay_seconds: delay,
            retry_deadline: None,
            diagnostic: diagnostic.map(str::to_owned),
        }
    }

    fn active(attempt: u64, delay: u64) -> AgentEvent {
        recovery(
            ModelRecoveryState::Active,
            None,
            Some(attempt),
            Some(delay),
            Some("⚠ Provider unavailable · HTTP 503 · retrying request in 32s"),
        )
    }

    fn paused(delay: u64) -> AgentEvent {
        recovery(
            ModelRecoveryState::Paused,
            Some(CONTINUE_LATER),
            Some(4),
            Some(delay),
            Some("⚠ Rate limited · server requested a longer wait · recovery paused · attempt 4"),
        )
    }

    fn failed() -> AgentEvent {
        recovery(
            ModelRecoveryState::Failed,
            Some("surface_stall"),
            Some(4),
            None,
            Some(
                "⚠ Response ended early · stream interrupted · kept failing at the same point · stopped",
            ),
        )
    }

    fn recovered() -> AgentEvent {
        recovery(ModelRecoveryState::Recovered, None, None, None, None)
    }

    #[test]
    fn active_retries_read_as_one_waiting_line() {
        let mut tracker = RecoveryTracker::default();
        let now = Instant::now();
        assert_eq!(
            tracker.observe(&active(3, 32), now),
            Some(RecoveryEffect::Waiting {
                step: "Waiting on the model — try 3 of 8, next in 32 s".into()
            })
        );
        assert_eq!(
            tracker.observe(
                &recovery(ModelRecoveryState::Active, None, None, None, None),
                now
            ),
            Some(RecoveryEffect::Waiting {
                step: "Waiting on the model".into()
            })
        );
        let mut waiting = recovery(ModelRecoveryState::Active, None, None, Some(8), None);
        if let AgentEvent::ModelRecovery { action, .. } = &mut waiting {
            *action = Some("waiting_for_connectivity".into());
        }
        assert_eq!(
            tracker.observe(&waiting, now),
            Some(RecoveryEffect::Waiting {
                step: "Waiting for the connection — next in 8 s".into()
            })
        );
        assert!(!tracker.holds_turn());
        assert_eq!(
            tracker.observe(&recovered(), now),
            Some(RecoveryEffect::Working)
        );
        // A clear with nothing to clear writes nothing.
        assert_eq!(tracker.observe(&recovered(), now), None);
        assert_eq!(
            tracker.observe(&AgentEvent::TextDelta { text: "hi".into() }, now),
            None
        );
    }

    #[test]
    fn a_failed_recovery_is_an_error_with_the_reason() {
        let mut tracker = RecoveryTracker::default();
        let now = Instant::now();
        tracker.observe(&active(2, 4), now);
        assert_eq!(
            tracker.observe(&failed(), now),
            Some(RecoveryEffect::Error {
                reason: "The model gave up: Response ended early · stream interrupted · kept failing at the same point · stopped".into()
            })
        );
        // The error stands: a late clear or retry does not reopen it.
        assert_eq!(tracker.observe(&recovered(), now), None);
        assert_eq!(tracker.observe(&active(1, 1), now), None);
        assert_eq!(tracker.turn_ended(now), TurnEnd::Over);
    }

    #[test]
    fn a_pause_holds_the_turn_and_resumes_once_after_the_advised_delay() {
        let mut tracker = RecoveryTracker::default();
        let now = Instant::now();
        assert_eq!(
            tracker.observe(&paused(20), now),
            Some(RecoveryEffect::Waiting {
                step: "The model paused: Rate limited · server requested a longer wait · recovery paused · attempt 4".into()
            })
        );
        assert!(tracker.holds_turn());
        assert_eq!(tracker.error_at(), Some(now + PAUSE_LIMIT));
        // The paused turn's Done schedules the resume; the worker is not done.
        assert_eq!(
            tracker.turn_ended(now),
            TurnEnd::Held {
                resume_at: Some(now + Duration::from_secs(20))
            }
        );
        assert_eq!(tracker.resume_at(), Some(now + Duration::from_secs(20)));
        assert!(tracker.take_resume());
        assert!(!tracker.take_resume());
        // The resumed turn recovers and finishes clean.
        assert_eq!(
            tracker.observe(&recovered(), now + Duration::from_secs(25)),
            Some(RecoveryEffect::Working)
        );
        assert!(!tracker.holds_turn());
        assert_eq!(
            tracker.turn_ended(now + Duration::from_secs(60)),
            TurnEnd::Over
        );
        // A clean turn end gives the next turn its own resume.
        tracker.observe(&paused(1), now + Duration::from_secs(90));
        assert_eq!(
            tracker.turn_ended(now + Duration::from_secs(90)),
            TurnEnd::Held {
                resume_at: Some(now + Duration::from_secs(90) + RESUME_DELAY_MIN)
            }
        );
    }

    #[test]
    fn a_resumed_turn_that_ends_without_a_clear_is_over() {
        let mut tracker = RecoveryTracker::default();
        let now = Instant::now();
        tracker.observe(&paused(10), now);
        assert!(matches!(tracker.turn_ended(now), TurnEnd::Held { .. }));
        tracker.take_resume();
        // fx answered the resume with a finished turn and no status update.
        assert!(tracker.holds_turn());
        assert_eq!(
            tracker.turn_ended(now + Duration::from_secs(40)),
            TurnEnd::Over
        );
        assert!(!tracker.holds_turn());
        assert_eq!(tracker.error_at(), None);
        assert_eq!(tracker.last_step, "");
    }

    #[test]
    fn a_second_pause_in_the_same_turn_is_an_error() {
        let mut tracker = RecoveryTracker::default();
        let now = Instant::now();
        tracker.observe(&paused(600), now);
        // A long advised delay still resumes ahead of the pause limit.
        assert_eq!(
            tracker.turn_ended(now),
            TurnEnd::Held {
                resume_at: Some(now + RESUME_DELAY_MAX)
            }
        );
        tracker.take_resume();
        let later = now + Duration::from_secs(250);
        assert_eq!(
            tracker.observe(&paused(600), later),
            Some(RecoveryEffect::Error {
                reason: "The model paused again: Rate limited · server requested a longer wait · recovery paused · attempt 4".into()
            })
        );
        assert!(!tracker.holds_turn());
        assert_eq!(tracker.resume_at(), None);
        assert_eq!(tracker.turn_ended(later), TurnEnd::Over);
    }

    #[test]
    fn a_pause_older_than_five_minutes_is_an_error() {
        let mut tracker = RecoveryTracker::default();
        let now = Instant::now();
        let mut stuck = paused(30);
        if let AgentEvent::ModelRecovery {
            required_action, ..
        } = &mut stuck
        {
            *required_action = Some("inspect_uncertain_tool".into());
        }
        tracker.observe(&stuck, now);
        // Not resumable by the engine: no resume is scheduled, the turn is held.
        assert_eq!(tracker.turn_ended(now), TurnEnd::Held { resume_at: None });
        assert!(tracker.holds_turn());
        assert_eq!(tracker.tick(now + Duration::from_secs(299)), None);
        assert!(tracker.holds_turn());
        assert_eq!(
            tracker.tick(now + Duration::from_secs(300)),
            Some(RecoveryEffect::Error {
                reason: "The model paused and did not resume: Rate limited · server requested a longer wait · recovery paused · attempt 4".into()
            })
        );
        assert!(!tracker.holds_turn());
        assert_eq!(tracker.tick(now + Duration::from_secs(900)), None);
    }

    #[test]
    fn a_resumable_pause_that_never_resumes_is_an_error_too() {
        let mut tracker = RecoveryTracker::default();
        let now = Instant::now();
        tracker.observe(&paused(30), now);
        assert!(matches!(tracker.turn_ended(now), TurnEnd::Held { .. }));
        // The resume went out, but fx never answered at all.
        tracker.take_resume();
        assert_eq!(
            tracker.tick(now + PAUSE_LIMIT - Duration::from_secs(1)),
            None
        );
        assert!(matches!(
            tracker.tick(now + PAUSE_LIMIT),
            Some(RecoveryEffect::Error { .. })
        ));
        // The run ending with a pause still open is an error as well.
        let mut ended = RecoveryTracker::default();
        ended.observe(&paused(30), now);
        assert!(matches!(ended.end(), Some(RecoveryEffect::Error { reason })
            if reason.starts_with("The model paused and the session ended: ")));
        assert_eq!(ended.end(), None);
    }

    #[test]
    fn reasons_drop_the_glyph_and_fall_back_to_the_cause() {
        assert_eq!(
            reason_text(Some("⚠ Rate limited · paused"), None),
            "Rate limited · paused"
        );
        assert_eq!(
            reason_text(Some("   "), Some("connectivity_lost")),
            "connectivity lost"
        );
        assert_eq!(reason_text(None, None), "no reason given");
    }

    fn scratch(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("arbos-recovery-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        arbos_layout::ensure(&root, Some("Recovery")).unwrap();
        arbos_layout::claim_main(&root, "main-chat").unwrap();
        arbos_layout::reserve_worker(&root, "oak", "Fix the tests", "glm-5.3", None).unwrap();
        arbos_layout::bind_worker_chat(&root, "oak", "oak-chat").unwrap();
        root
    }

    fn oak(project: &Path) -> arbos_layout::AgentView {
        arbos_layout::worker_result(project, "oak").unwrap()
    }

    /// The effects written to `agent.toml`: the status word, the step line,
    /// and the result text Main reads back.
    #[test]
    fn recovery_effects_reach_the_worker_file() {
        let root = scratch("file");
        let mut tracker = RecoveryTracker::default();
        let now = Instant::now();

        let Some(RecoveryEffect::Waiting { step }) = tracker.observe(&active(3, 32), now) else {
            panic!("a retry is a wait");
        };
        arbos_layout::note_worker_recovery(&root, "oak-chat", "waiting", &step).unwrap();
        let view = oak(&root);
        assert_eq!(view.status, "waiting");
        assert_eq!(view.step, "Waiting on the model — try 3 of 8, next in 32 s");
        assert!(arbos_layout::status_is_live(&view.status));

        assert_eq!(
            tracker.observe(&recovered(), now),
            Some(RecoveryEffect::Working)
        );
        arbos_layout::note_worker_recovery(&root, "oak-chat", "working", "").unwrap();
        let view = oak(&root);
        assert_eq!((view.status.as_str(), view.step.as_str()), ("working", ""));

        let Some(RecoveryEffect::Error { reason }) = tracker.observe(&failed(), now) else {
            panic!("a stop is an error");
        };
        let wake = arbos_live::worker_wake(&root, "oak-chat", "error", &reason).unwrap();
        assert_eq!(wake.main_chat_id, "main-chat");
        assert!(
            wake.body
                .starts_with("Worker oak error.\nThe model gave up: ")
        );
        let view = oak(&root);
        assert_eq!(view.status, "error");
        assert_eq!(view.step, "");
        assert!(
            view.result
                .starts_with("The model gave up: Response ended early")
        );
        // A finished worker takes no more recovery notes, and no second wake.
        assert!(arbos_layout::note_worker_recovery(&root, "oak-chat", "waiting", "x").is_none());
        assert!(arbos_live::worker_wake(&root, "oak-chat", "done", "late").is_none());
        // Main's chat is not a worker.
        assert!(arbos_layout::note_worker_recovery(&root, "main-chat", "waiting", "x").is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_waiting_worker_can_still_finish_or_time_out_as_error() {
        let root = scratch("waiting");
        arbos_layout::note_worker_recovery(&root, "oak-chat", "waiting", "The model paused: x")
            .unwrap();
        assert_eq!(oak(&root).status, "waiting");
        // The turn ends clean after all: `done` wins over `waiting`.
        let done = arbos_live::worker_wake(&root, "oak-chat", "done", "All green.").unwrap();
        assert_eq!(done.body, "Worker oak done.\nAll green.");
        assert_eq!(oak(&root).status, "done");
        assert_eq!(oak(&root).step, "");

        // The five-minute rule on a fresh turn.
        arbos_layout::restart_worker(&root, "oak").unwrap();
        let mut tracker = RecoveryTracker::default();
        let now = Instant::now();
        let Some(RecoveryEffect::Waiting { step }) = tracker.observe(&paused(30), now) else {
            panic!("a pause is a wait");
        };
        arbos_layout::note_worker_recovery(&root, "oak-chat", "waiting", &step).unwrap();
        let Some(RecoveryEffect::Error { reason }) = tracker.tick(now + PAUSE_LIMIT) else {
            panic!("five minutes paused is an error");
        };
        arbos_live::worker_wake(&root, "oak-chat", "error", &reason).unwrap();
        let view = oak(&root);
        assert_eq!(view.status, "error");
        assert_eq!(
            view.result,
            "The model paused and did not resume: Rate limited · server requested a longer wait · recovery paused · attempt 4"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn main_gets_the_line_as_its_live_step_and_keeps_its_own() {
        let root = scratch("main");
        arbos_live::set_harness_step(&root, "Waiting on the model — try 1 of 8");
        assert_eq!(
            arbos_live::live(&root).step,
            "Waiting on the model — try 1 of 8"
        );
        arbos_live::clear_harness_step(&root, "Waiting on the model — try 1 of 8");
        assert_eq!(arbos_live::live(&root).step, "");
        arbos_live::set_harness_step(&root, "The model paused: x");
        arbos_live::set_step(&root, "Checking the gate").unwrap();
        arbos_live::clear_harness_step(&root, "The model paused: x");
        assert_eq!(arbos_live::live(&root).step, "Checking the gate");
        let _ = fs::remove_dir_all(&root);
    }
}
