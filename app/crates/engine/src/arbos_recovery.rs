//! What a run does with fx's model-response recovery. fx retries a failed
//! model request with backoff, pauses the turn when it cannot go on, or gives
//! up; each change arrives as [`AgentEvent::ModelRecovery`]. The tracker here
//! turns those into one plain line for the person and the worker's status
//! word, holds a paused turn open for one automatic resume, and marks the
//! worker `error` when a pause outlives [`PAUSE_LIMIT`].
//!
//! The [`SilentModelPulse`] covers the one failure fx cannot report today: a
//! model request that settles with no content, no error, and no retry
//! decision, so no frame ever reaches the engine.

use std::path::{Path, PathBuf};
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

    /// Nothing recovery-shaped is in play: no retry line, no pause, no
    /// failure. The silent-model pulse only runs while this holds — a turn
    /// fx is already reporting (or holding open) owns its own status.
    pub fn is_clear(&self) -> bool {
        matches!(self.phase, Phase::Clear)
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

/// How long a live worker turn must produce no events at all before the
/// silent-model pulse arms. Shorter silent stretches are normal work (a long
/// tool call, redacted thinking, an agent waiting on an external process),
/// so the pulse stays out of them.
pub const SILENT_MODEL_AFTER: Duration = Duration::from_secs(120);
/// How often an armed pulse re-reads the usage ledger while the turn stays
/// silent: one bounded tail read per window.
pub const SILENT_MODEL_POLL: Duration = Duration::from_secs(15);
/// The one line the pulse writes while it waits. PR 841's retry line opens
/// with the same words, so a turn the pulse flags that goes on to report
/// real recovery frames reads as one story.
pub const SILENT_MODEL_STEP: &str = "Waiting on the model";
/// The ledger tail the pulse reads: hundreds of entries, and always the
/// newest one, which is all the pulse compares against.
const LEDGER_TAIL: u64 = 64 * 1024;

/// Watches for a worker's model settling requests silently. fx reports
/// retries and pauses as [`AgentEvent::ModelRecovery`] frames (PR 841), but
/// a request that settles with no content, no error, and no retry decision
/// leaves nothing on the stream — the 2026-10-12 arbos-updater window, where
/// three settlements ~300 s apart kept a live worker's status on `working`
/// for eleven minutes. The only trace is fx's usage ledger, which records
/// one `incident` per settled request, so the pulse reads its tail: once the
/// turn has been silent past [`SILENT_MODEL_AFTER`] it baselines the ledger,
/// and one incident newer than that baseline means the model is still
/// settling requests with nothing to show — `waiting`, through PR 841's
/// writer, until any event at all comes back.
///
/// The silence is the signal, not the incidents: a healthy worker logs
/// dozens of incidents while it works (the control case measured 39 in
/// eleven minutes), so the pulse never arms while events flow. Ledger
/// timestamps are compared to each other only, never to the local clock —
/// the ledger's `occurred_at_ms` is fx's notion of when a request settled.
#[derive(Debug)]
pub struct SilentModelPulse {
    /// The worker's fx home; `None` watches nothing (this run is not a
    /// worker turn inside a project).
    home: Option<PathBuf>,
    /// Silence threshold and re-check cadence. `ZERON_SILENT_MODEL_MS` and
    /// `ZERON_SILENT_MODEL_POLL_MS` override both (tests); `0` on the
    /// threshold disables the pulse outright.
    after: Duration,
    poll: Duration,
    /// `false` when `ZERON_SILENT_MODEL_MS=0`: the pulse never arms.
    armed_by_default: bool,
    /// When the next ledger sample is due; `None` until the turn has been
    /// silent past `after` (that first sample takes the baseline).
    due_at: Option<Instant>,
    /// The newest incident the ledger held when the pulse armed. A later
    /// one is the wait signal.
    baseline_ms: Option<u64>,
    /// The pulse wrote `waiting`; the next event of any kind restores
    /// `working`.
    flipped: bool,
}

impl SilentModelPulse {
    /// `home` is the worker's fx home (the ledger lives at
    /// `<home>/.fx/usage.jsonl`); `None` makes the pulse inert.
    pub fn new(home: Option<PathBuf>) -> Self {
        let threshold = env_ms("ZERON_SILENT_MODEL_MS");
        let poll = env_ms("ZERON_SILENT_MODEL_POLL_MS");
        Self {
            home,
            after: threshold
                .filter(|ms| *ms > 0)
                .map(Duration::from_millis)
                .unwrap_or(SILENT_MODEL_AFTER),
            poll: poll
                .filter(|ms| *ms > 0)
                .map(Duration::from_millis)
                .unwrap_or(SILENT_MODEL_POLL),
            armed_by_default: threshold.is_none_or(|ms| ms > 0),
            due_at: None,
            baseline_ms: None,
            flipped: false,
        }
    }

    /// The pulse has a ledger to watch and has not spoken yet.
    pub fn watching(&self) -> bool {
        self.armed_by_default && self.home.is_some() && !self.flipped
    }

    /// When the drive loop should next run [`Self::tick`]: the arming moment
    /// while unarmed, the next sample once armed, and never once the pulse
    /// has spoken or has nothing to watch.
    pub fn next_due(&self, last_event_at: Instant) -> Option<Instant> {
        if !self.watching() {
            return None;
        }
        Some(self.due_at.unwrap_or(last_event_at + self.after))
    }

    /// One drive-loop tick: past the silence threshold the pulse arms on the
    /// ledger as it stands, and each poll after that compares a fresh read
    /// against that baseline. The waiting effect comes back once, on the
    /// first incident newer than the baseline.
    pub fn tick(&mut self, now: Instant, last_event_at: Instant) -> Option<RecoveryEffect> {
        let due = match self.due_at {
            None => self.armed_by_default && now >= last_event_at + self.after,
            Some(at) => now >= at,
        };
        if !due || !self.watching() {
            return None;
        }
        let home = self.home.clone()?;
        let effect = if self.due_at.is_none() {
            // Arming: whatever the ledger already holds is the baseline —
            // incidents from before the silence are normal work.
            self.baseline_ms = latest_incident_ms(&home);
            None
        } else {
            let newer = latest_incident_ms(&home)
                .is_some_and(|at| self.baseline_ms.is_none_or(|base| at > base));
            if newer {
                self.flipped = true;
                Some(RecoveryEffect::Waiting {
                    step: SILENT_MODEL_STEP.to_owned(),
                })
            } else {
                None
            }
        };
        self.due_at = Some(now + self.poll);
        effect
    }

    /// An event of any kind arrived. The silence window restarts, and if the
    /// pulse had flipped the worker to `waiting`, this event is the model
    /// answering: the caller restores `working` (a recovery frame arriving
    /// next rewrites the line with its own words). Returns whether the pulse
    /// had flipped, exactly once.
    pub fn event(&mut self) -> bool {
        let flipped = self.flipped;
        self.due_at = None;
        self.baseline_ms = None;
        self.flipped = false;
        flipped
    }
}

/// `ZERON_SILENT_MODEL_MS`-style knobs: whole milliseconds, `0` included.
fn env_ms(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// The newest `incident` fx's usage ledger holds, from the tail of
/// `<fx_home>/.fx/usage.jsonl`. fx writes one `incident` per settled request
/// (usage accounting, not a retry marker); `completeness == "incomplete"`
/// marks the settlements it could not attribute, which is the shape a
/// provider that never returns usage data leaves on every request. `None`
/// when there is nothing to read yet.
pub fn latest_incident_ms(fx_home: &Path) -> Option<u64> {
    let tail = ledger_tail(fx_home)?;
    let mut newest = None;
    for line in tail.lines() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if entry.get("kind").and_then(|v| v.as_str()) != Some("incident") {
            continue;
        }
        if entry.get("completeness").and_then(|v| v.as_str()) != Some("incomplete") {
            continue;
        }
        let at = entry.get("occurred_at_ms").and_then(|v| v.as_u64());
        if newest.is_none_or(|seen| at.is_some_and(|at| at > seen)) {
            newest = at;
        }
    }
    newest
}

/// The last [`LEDGER_TAIL`] bytes of the usage ledger, starting on a line
/// boundary so a record cut in half is never parsed. `None` when the ledger
/// does not exist yet.
fn ledger_tail(fx_home: &Path) -> Option<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(fx_home.join(".fx").join("usage.jsonl")).ok()?;
    let size = file.metadata().ok()?.len();
    let start = size.saturating_sub(LEDGER_TAIL);
    if start > 0 {
        file.seek(SeekFrom::Start(start)).ok()?;
    }
    let mut bytes = Vec::with_capacity((size - start) as usize);
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if start == 0 {
        return Some(text);
    }
    // The read began mid-record: everything before the first newline is a
    // partial line, not an entry.
    text.split_once('\n').map(|(_, rest)| rest.to_owned())
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

    /// A scratch fx home with a usage ledger the test appends to, the way
    /// fx appends one record per settled request.
    fn ledger_home(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("arbos-pulse-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".fx")).unwrap();
        root
    }

    /// One fx usage-ledger record, in the exact shape fx 0.0.13 writes.
    fn incident(ms: u64) -> String {
        format!(
            "{{\"schema_version\":1,\"kind\":\"incident\",\"occurred_at_ms\":{ms},\
             \"completeness\":\"incomplete\"}}\n"
        )
    }

    fn append_record(home: &Path, record: &str) {
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(home.join(".fx").join("usage.jsonl"))
            .unwrap();
        file.write_all(record.as_bytes()).unwrap();
    }

    /// The pulse with test-sized windows (the fields are crate-visible).
    fn quick_pulse(home: &Path) -> SilentModelPulse {
        let mut pulse = SilentModelPulse::new(Some(home.to_path_buf()));
        pulse.after = Duration::from_millis(20);
        pulse.poll = Duration::from_millis(5);
        pulse
    }

    /// The incident's own ledger: the coverage header fx opens the file
    /// with, then the dense incident cadence of a healthy worker at work,
    /// ending at 22:33:41 UTC — the moment the model went silent.
    fn incident_prefix(home: &Path) {
        append_record(
            home,
            "{\"schema_version\":1,\"kind\":\"coverage\",\"started_at_ms\":1791834492203}\n",
        );
        append_record(home, &incident(1791844307724));
        append_record(home, &incident(1791844353713));
        append_record(home, &incident(1791844421558));
    }

    #[test]
    fn the_ledger_tail_yields_the_newest_incomplete_incident() {
        let home = ledger_home("reader");
        // No ledger yet: nothing to read.
        assert_eq!(latest_incident_ms(&home), None);
        incident_prefix(&home);
        // Records that are not incomplete incidents never count, and a
        // malformed line (a torn write) is skipped, not fatal.
        append_record(
            &home,
            "{\"schema_version\":1,\"kind\":\"incident\",\"occurred_at_ms\":1791849900000,\
             \"completeness\":\"complete\"}\n",
        );
        append_record(
            &home,
            "{\"schema_version\":1,\"kind\":\"generation\",\"occurred_at_ms\":1791849900001}\n",
        );
        append_record(&home, "not json at all\n");
        assert_eq!(latest_incident_ms(&home), Some(1791844421558));
        // The spaced window from the incident: 22:38:24, then 22:43:27 —
        // the newest wins.
        append_record(&home, &incident(1791844704063));
        append_record(&home, &incident(1791845007214));
        assert_eq!(latest_incident_ms(&home), Some(1791845007214));

        // A ledger past the 64 KiB tail cap: only the tail is read, so an
        // old incident that fell out of the window is not the answer, the
        // newest one at the end still is, and a first line cut mid-record
        // cannot be parsed into a bogus timestamp.
        let big = ledger_home("reader-tail");
        let mut text = String::new();
        text.push_str(&incident(1791834492201));
        let filler = incident(1791840000000);
        for _ in 0..700 {
            text.push_str(&filler);
        }
        text.push_str(&incident(1791845383634));
        append_record(&big, &text);
        assert!(
            big.join(".fx")
                .join("usage.jsonl")
                .metadata()
                .unwrap()
                .len()
                > LEDGER_TAIL
        );
        assert_eq!(latest_incident_ms(&big), Some(1791845383634));
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&big);
    }

    /// The 2026-10-12 arbos-updater window, as the pulse sees it: a live
    /// turn that stops producing events while requests keep settling ~300 s
    /// apart. One fresh incident past the baseline waits, once; any event at
    /// all restores `working`.
    #[test]
    fn a_silent_turn_with_spaced_incidents_waits_once_and_any_event_restores() {
        let home = ledger_home("waits");
        incident_prefix(&home);
        let mut pulse = quick_pulse(&home);
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);

        // The turn runs and speaks; the pulse has nothing to do while it
        // does. Its first due moment is one silence window past the event.
        assert_eq!(pulse.next_due(at(5)), Some(at(25)));
        assert_eq!(pulse.tick(at(10), at(5)), None);

        // The stream goes quiet. One window past the last event the pulse
        // arms on the ledger as it stands — 22:33:41 is normal work, not a
        // wait signal.
        assert_eq!(pulse.tick(at(25), at(5)), None);
        assert_eq!(pulse.next_due(at(26)), Some(at(30)));
        assert_eq!(pulse.tick(at(30), at(5)), None);

        // 22:38:24 (1791844704063): the first spaced settlement lands. The
        // next poll sees it and the pulse speaks, once.
        append_record(&home, &incident(1791844704063));
        assert_eq!(
            pulse.tick(at(35), at(5)),
            Some(RecoveryEffect::Waiting {
                step: SILENT_MODEL_STEP.to_owned()
            })
        );
        assert!(!pulse.watching());
        assert_eq!(pulse.next_due(at(36)), None);

        // 22:43:27 (1791845007214): the second settlement changes nothing,
        // and neither do further ticks.
        append_record(&home, &incident(1791845007214));
        assert_eq!(pulse.tick(at(40), at(5)), None);
        assert_eq!(pulse.tick(at(60), at(5)), None);

        // Any event at all — here the dense cadence resuming at 22:43:34 —
        // is the model answering: restore `working`, exactly once.
        assert!(pulse.event());
        assert!(!pulse.event());
        // The silence window starts over for whatever comes next.
        assert_eq!(pulse.next_due(at(65)), Some(at(85)));
        assert_eq!(pulse.tick(at(70), at(65)), None);
        let _ = fs::remove_dir_all(&home);
    }

    /// The guard the design insists on: incidents are a per-request
    /// accounting heartbeat (a healthy worker logged 39 in eleven minutes),
    /// so they are a wait signal only under turn silence. While events
    /// flow, ever-newer incidents must never wait.
    #[test]
    fn incidents_while_events_flow_never_wait() {
        let home = ledger_home("guard");
        incident_prefix(&home);
        let mut pulse = quick_pulse(&home);
        let start = Instant::now();

        // Events keep arriving, and a fresh incident settles between each
        // one: the silence window never passes, so the pulse never even
        // takes a baseline.
        let mut newest = 1791844421558;
        let mut last_event = start;
        for step in 0..20 {
            newest += 1;
            append_record(&home, &incident(newest));
            last_event = start + Duration::from_millis(5 * step);
            assert_eq!(
                pulse.tick(last_event, last_event),
                None,
                "step {step} must not wait"
            );
        }
        // Nothing was ever flipped, so no event has anything to restore.
        assert!(!pulse.event());
        assert_eq!(pulse.due_at, None);

        // Only real silence arms the pulse — and it arms on the ledger as it
        // stands, so the incidents that landed during the flow are the
        // baseline, not the signal.
        let quiet = last_event + pulse.after + Duration::from_millis(1);
        assert_eq!(pulse.tick(quiet, last_event), None);
        assert_eq!(pulse.baseline_ms, Some(newest));
        append_record(&home, &incident(newest + 1));
        assert_eq!(
            pulse.tick(quiet + pulse.poll, last_event),
            Some(RecoveryEffect::Waiting {
                step: SILENT_MODEL_STEP.to_owned()
            })
        );
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn a_pulse_with_nothing_to_watch_stays_quiet() {
        // Not a worker turn in a project: no ledger to read.
        let mut pulse = SilentModelPulse::new(None);
        let now = Instant::now();
        assert_eq!(pulse.next_due(now), None);
        assert_eq!(
            pulse.tick(now + SILENT_MODEL_AFTER + Duration::from_secs(1), now),
            None
        );
        assert!(!pulse.event());
        // `ZERON_SILENT_MODEL_MS=0` disables the pulse the same way.
        let mut off = quick_pulse(Path::new("/nowhere"));
        off.armed_by_default = false;
        assert_eq!(off.next_due(now), None);
        assert_eq!(off.tick(now + Duration::from_secs(1), now), None);
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
