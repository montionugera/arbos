//! The durable goal of one project: `.arbos/context/goal.toml`.
//!
//! `/goal <text>` writes the record. While the goal is active and Main is
//! idle, the wake loop in [`crate::arbos_wakes`] sends Main a goal wake on an
//! interval the agent picks with `goal.schedule`. Stop ends the turn only;
//! the record stays. Only the goal ops change the state, so a finishing
//! worker or job never resumes a paused goal.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::arbos_layout::{layout_lock, root_of, status_items};

/// Seconds between goal wakes when the agent has not picked an interval.
pub const GOAL_INTERVAL_DEFAULT: u64 = 600;
/// The shortest interval `goal.schedule` accepts.
pub const GOAL_INTERVAL_MIN: u64 = 30;
/// The longest interval `goal.schedule` accepts: one day.
pub const GOAL_INTERVAL_MAX: u64 = 86_400;
/// Longest objective kept in the record.
const OBJECTIVE_BYTES: usize = 2000;
/// Fewest words a completion reason needs to count as a sentence.
const REASON_WORDS: usize = 3;
/// Fewest characters a completion reason needs to count as a sentence.
const REASON_CHARS: usize = 20;
/// Longest completion reason kept in the record.
const REASON_BYTES: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalState {
    Active,
    Paused,
    Complete,
}

impl GoalState {
    pub fn as_str(self) -> &'static str {
        match self {
            GoalState::Active => "active",
            GoalState::Paused => "paused",
            GoalState::Complete => "complete",
        }
    }
}

/// The on-disk record. Times are RFC 3339 strings so the file reads well.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalRecord {
    pub objective: String,
    pub started: String,
    pub state: GoalState,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_wake: String,
    /// Seconds between wakes. `0` means [`GOAL_INTERVAL_DEFAULT`].
    #[serde(default)]
    pub every_seconds: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub completed: String,
    /// Why the goal was completed: Main's sentence, or a note that a person
    /// completed it from the UI.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// How many goal wakes have been sent.
    #[serde(default)]
    pub wakes: u64,
}

/// What the row above the composer shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GoalRow {
    pub objective: String,
    pub state: String,
    pub started: String,
    /// Seconds since the goal started. A complete goal stops the clock at
    /// its completion time.
    pub elapsed_seconds: u64,
    pub last_wake: String,
    pub every_seconds: u64,
    pub wakes: u64,
}

fn goal_path(project: &Path) -> PathBuf {
    root_of(project).join("context").join("goal.toml")
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

/// Clamp a requested interval into the accepted range. `None` and `0` mean
/// the default.
pub fn clamp_goal_interval(seconds: Option<u64>) -> u64 {
    match seconds {
        None | Some(0) => GOAL_INTERVAL_DEFAULT,
        Some(seconds) => seconds.clamp(GOAL_INTERVAL_MIN, GOAL_INTERVAL_MAX),
    }
}

/// Seconds between wakes for this record.
pub fn goal_interval(record: &GoalRecord) -> u64 {
    clamp_goal_interval(Some(record.every_seconds))
}

/// Whether the loop should wake Main now. Only an active goal wakes, only
/// when Main is idle, and only once the interval has passed since the last
/// wake (or since the start, before the first wake).
pub fn goal_wake_due(record: &GoalRecord, now: DateTime<Utc>, main_working: bool) -> bool {
    if record.state != GoalState::Active || main_working {
        return false;
    }
    let since = parse_time(&record.last_wake)
        .or_else(|| parse_time(&record.started))
        .unwrap_or(now);
    let elapsed = now.signed_duration_since(since).num_seconds();
    elapsed >= goal_interval(record) as i64
}

/// Longest objective the wake headline shows.
const HEADLINE_CHARS: usize = 72;

/// The first line of a goal wake. The transcript shows it as the Worked line,
/// so it is plain words, never a tag.
pub fn goal_wake_headline(objective: &str) -> String {
    let objective = objective.split_whitespace().collect::<Vec<_>>().join(" ");
    let short: String = objective.chars().take(HEADLINE_CHARS).collect();
    if short.chars().count() < objective.chars().count() {
        format!("Goal check · {}…", short.trim_end())
    } else {
        format!("Goal check · {short}")
    }
}

/// The rule every goal wake repeats. The engine enforces it in
/// [`complete_goal`].
pub const GOAL_COMPLETE_RULE: &str =
    "Call goal.complete only when every step of the goal is checked and you can say why.";

/// The prompt a goal wake carries. The caller adds the wake mark. The first
/// line is the headline; the notification block follows it.
pub fn goal_wake_prompt(record: &GoalRecord, now: DateTime<Utc>) -> String {
    let elapsed = parse_time(&record.started)
        .map(|started| now.signed_duration_since(started).num_seconds().max(0) as u64)
        .unwrap_or(0);
    format!(
        "{}\n\
         <system_notification source=\"goal\">\n\
         Goal: {}\n\
         Started {}. Elapsed {}. Wake {}.\n\
         This is a goal wake, not a message from the person. \
         Make progress or end the turn quietly. \
         {GOAL_COMPLETE_RULE} \
         Call goal.schedule to pick how long to wait before the next wake.\n\
         </system_notification>",
        goal_wake_headline(&record.objective),
        record.objective.trim(),
        record.started.trim(),
        elapsed_text(elapsed),
        record.wakes + 1
    )
}

fn elapsed_text(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// Read the record. `None` when the project has no goal.
pub fn read_goal(project: &Path) -> Option<GoalRecord> {
    let text = fs::read_to_string(goal_path(project)).ok()?;
    toml::from_str(&text).ok()
}

fn write_goal(project: &Path, record: &GoalRecord) -> Result<(), String> {
    let path = goal_path(project);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = toml::to_string_pretty(record).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).map_err(|e| e.to_string())?;
    if let Err(err) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(err.to_string());
    }
    Ok(())
}

/// Start a goal. The caller holds the layout lock: `/goal` calls this
/// while it writes `GOALS.md` and the checklist.
pub(crate) fn start_goal_unlocked(project: &Path, objective: &str) -> Result<GoalRecord, String> {
    start_goal_at(project, objective, Utc::now())
}

fn start_goal_at(
    project: &Path,
    objective: &str,
    now: DateTime<Utc>,
) -> Result<GoalRecord, String> {
    let objective = clip(objective);
    if objective.is_empty() {
        return Err("the goal needs an objective".into());
    }
    let record = GoalRecord {
        objective,
        started: now.to_rfc3339(),
        state: GoalState::Active,
        last_wake: String::new(),
        every_seconds: 0,
        completed: String::new(),
        reason: String::new(),
        wakes: 0,
    };
    write_goal(project, &record)?;
    Ok(record)
}

/// Start a goal from a plain op (not `/goal`).
pub fn start_goal(project: &Path, objective: &str) -> Result<GoalRecord, String> {
    let _guard = layout_lock();
    start_goal_unlocked(project, objective)
}

/// The row data for the UI. `None` when the project has no goal.
pub fn goal_row(project: &Path) -> Option<GoalRow> {
    goal_row_at(read_goal(project)?, Utc::now())
}

fn goal_row_at(record: GoalRecord, now: DateTime<Utc>) -> Option<GoalRow> {
    let started = parse_time(&record.started)?;
    let end = match record.state {
        GoalState::Complete => parse_time(&record.completed).unwrap_or(now),
        GoalState::Active | GoalState::Paused => now,
    };
    let elapsed_seconds = end.signed_duration_since(started).num_seconds().max(0) as u64;
    let every_seconds = goal_interval(&record);
    Some(GoalRow {
        objective: record.objective,
        state: record.state.as_str().to_owned(),
        started: record.started,
        elapsed_seconds,
        last_wake: record.last_wake,
        every_seconds,
        wakes: record.wakes,
    })
}

fn update<F>(project: &Path, change: F) -> Result<GoalRecord, String>
where
    F: FnOnce(&mut GoalRecord) -> Result<(), String>,
{
    let _guard = layout_lock();
    let mut record = read_goal(project).ok_or_else(|| "this project has no goal".to_string())?;
    change(&mut record)?;
    write_goal(project, &record)?;
    Ok(record)
}

/// `goal.schedule { seconds }`. The value is clamped, never rejected.
pub fn schedule_goal(project: &Path, seconds: u64) -> Result<GoalRecord, String> {
    update(project, |record| {
        record.every_seconds = clamp_goal_interval(Some(seconds));
        Ok(())
    })
}

/// `goal.pause`. The clock keeps running; wakes stop.
pub fn pause_goal(project: &Path) -> Result<GoalRecord, String> {
    update(project, |record| match record.state {
        GoalState::Active | GoalState::Paused => {
            record.state = GoalState::Paused;
            Ok(())
        }
        GoalState::Complete => Err("the goal is complete".into()),
    })
}

/// `goal.resume`. Only a person or Main resumes; the loop never does.
pub fn resume_goal(project: &Path) -> Result<GoalRecord, String> {
    update(project, |record| match record.state {
        GoalState::Active | GoalState::Paused => {
            record.state = GoalState::Active;
            Ok(())
        }
        GoalState::Complete => Err("the goal is complete; start a new one".into()),
    })
}

/// `goal.complete` from Main. Allowed only on evidence: every checklist line
/// of the current goal is ticked and `reason` says why in at least one
/// sentence. The refusal names what is still open. Stops the loop and the
/// clock.
pub fn complete_goal(project: &Path, reason: &str) -> Result<GoalRecord, String> {
    let reason = clip_to(reason, REASON_BYTES);
    // `status_items` takes the layout lock, so it runs before `update` does.
    let open: Vec<String> = status_items(project)
        .into_iter()
        .filter(|(done, _)| !done)
        .map(|(_, text)| text)
        .collect();
    complete_goal_checked(project, &reason, &open)
}

fn complete_goal_checked(
    project: &Path,
    reason: &str,
    open: &[String],
) -> Result<GoalRecord, String> {
    if !reason_is_a_sentence(reason) {
        return Err(refusal(open, true));
    }
    if !open.is_empty() {
        return Err(refusal(open, false));
    }
    finish_goal(project, reason)
}

/// A person completing from the UI. No checklist or reason is needed; a
/// Main-only call cannot reach this.
pub fn force_complete_goal(project: &Path) -> Result<GoalRecord, String> {
    finish_goal(project, "Completed by the person.")
}

fn finish_goal(project: &Path, reason: &str) -> Result<GoalRecord, String> {
    update(project, |record| {
        record.state = GoalState::Complete;
        record.completed = Utc::now().to_rfc3339();
        record.reason = reason.trim().to_owned();
        Ok(())
    })
}

/// At least one sentence: a few words, not just a word or a tick.
fn reason_is_a_sentence(reason: &str) -> bool {
    let reason = reason.trim();
    reason.split_whitespace().count() >= REASON_WORDS && reason.chars().count() >= REASON_CHARS
}

/// Checklist lines that are still open. Empty when every step is checked.
pub fn open_steps(project: &Path) -> Vec<String> {
    status_items(project)
        .into_iter()
        .filter(|(done, _)| !done)
        .map(|(_, text)| text)
        .collect()
}

/// The one line the thread shows when `goal.complete` is refused. One step
/// is named; further open steps are a count. `None` when nothing is open.
pub fn still_open_line(open: &[String]) -> Option<String> {
    let first = open.first()?.trim();
    if first.is_empty() {
        return None;
    }
    if open.len() == 1 {
        Some(format!("Goal still open · {first}"))
    } else {
        Some(format!(
            "Goal still open · {first} +{} more",
            open.len() - 1
        ))
    }
}

/// What Main reads when the call is refused: the rule, what is still open,
/// and the missing reason. An open step tells Main to name it, not to
/// repeat the claim that the goal is done.
fn refusal(open: &[String], reason_missing: bool) -> String {
    let mut lines = vec!["The goal is not complete yet.".to_owned()];
    if open.is_empty() {
        lines.push("Every step is checked.".into());
    } else {
        lines.push(if open.len() == 1 {
            "1 step is still open:".to_owned()
        } else {
            format!("{} steps are still open:", open.len())
        });
        lines.extend(open.iter().map(|line| format!("- {}", line.trim())));
        lines.push("Say what is still open. Do not say the goal is complete.".into());
    }
    if reason_missing {
        lines.push("Say why in at least one sentence, in the reason.".into());
    }
    lines.push(GOAL_COMPLETE_RULE.into());
    lines.join("\n")
}

/// `goal.clear`. Removes the record; the loop ends. `GOALS.md` and the
/// checklist are not touched.
pub fn clear_goal(project: &Path) -> Result<(), String> {
    let _guard = layout_lock();
    match fs::remove_file(goal_path(project)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.to_string()),
    }
}

/// The wake loop's step: if a wake is due, stamp the record and return the
/// prompt body. The caller queues it. Nothing else about the record changes.
pub fn take_goal_wake(project: &Path, main_working: bool) -> Option<String> {
    take_goal_wake_at(project, main_working, Utc::now())
}

fn take_goal_wake_at(project: &Path, main_working: bool, now: DateTime<Utc>) -> Option<String> {
    let _guard = layout_lock();
    let mut record = read_goal(project)?;
    if !goal_wake_due(&record, now, main_working) {
        return None;
    }
    let prompt = goal_wake_prompt(&record, now);
    record.last_wake = now.to_rfc3339();
    record.wakes += 1;
    write_goal(project, &record).ok()?;
    Some(prompt)
}

fn clip(text: &str) -> String {
    clip_to(text, OBJECTIVE_BYTES)
}

fn clip_to(text: &str, bytes: usize) -> String {
    let text = text.trim();
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 12, hour, minute, 0).unwrap()
    }

    fn record(state: GoalState) -> GoalRecord {
        GoalRecord {
            objective: "Ship the thing".into(),
            started: at(8, 0).to_rfc3339(),
            state,
            last_wake: String::new(),
            every_seconds: 0,
            completed: String::new(),
            reason: String::new(),
            wakes: 0,
        }
    }

    /// A project folder with a status page holding these checklist lines.
    fn project_with_checklist(lines: &[(bool, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let context = dir.path().join(".arbos").join("context");
        fs::create_dir_all(&context).unwrap();
        let mut page = String::from("# Status\n\n");
        for (done, text) in lines {
            let mark = if *done { "- [x]" } else { "- [ ]" };
            page.push_str(&format!("{mark} {text}\n"));
        }
        fs::write(context.join("status.md"), page).unwrap();
        dir
    }

    const REASON: &str = "Both workers reported and the gate is green.";

    #[test]
    fn interval_is_clamped() {
        assert_eq!(clamp_goal_interval(None), GOAL_INTERVAL_DEFAULT);
        assert_eq!(clamp_goal_interval(Some(0)), GOAL_INTERVAL_DEFAULT);
        assert_eq!(clamp_goal_interval(Some(1)), GOAL_INTERVAL_MIN);
        assert_eq!(clamp_goal_interval(Some(300)), 300);
        assert_eq!(clamp_goal_interval(Some(u64::MAX)), GOAL_INTERVAL_MAX);
        let mut fast = record(GoalState::Active);
        fast.every_seconds = 5;
        assert_eq!(goal_interval(&fast), GOAL_INTERVAL_MIN);
    }

    #[test]
    fn active_goal_wakes_idle_main_on_the_interval() {
        let active = record(GoalState::Active);
        // Before the default interval has passed since the start: no wake.
        assert!(!goal_wake_due(&active, at(8, 5), false));
        // After it: a wake, but only when Main is idle.
        assert!(goal_wake_due(&active, at(8, 10), false));
        assert!(!goal_wake_due(&active, at(8, 10), true));
        // The last wake, not the start, sets the next due time.
        let mut woken = active.clone();
        woken.last_wake = at(9, 0).to_rfc3339();
        assert!(!goal_wake_due(&woken, at(9, 5), false));
        assert!(goal_wake_due(&woken, at(9, 10), false));
        // A chosen interval applies.
        let mut hourly = woken.clone();
        hourly.every_seconds = 3600;
        assert!(!goal_wake_due(&hourly, at(9, 59), false));
        assert!(goal_wake_due(&hourly, at(10, 0), false));
    }

    #[test]
    fn paused_and_complete_goals_do_not_wake() {
        assert!(!goal_wake_due(&record(GoalState::Paused), at(12, 0), false));
        assert!(!goal_wake_due(
            &record(GoalState::Complete),
            at(12, 0),
            false
        ));
    }

    #[test]
    fn wake_prompt_carries_the_objective_and_the_rule() {
        let mut goal = record(GoalState::Active);
        goal.wakes = 2;
        let prompt = goal_wake_prompt(&goal, at(9, 30));
        let mut lines = prompt.lines();
        assert_eq!(lines.next(), Some("Goal check · Ship the thing"));
        assert_eq!(lines.next(), Some("<system_notification source=\"goal\">"));
        assert!(prompt.ends_with("</system_notification>"));
        assert!(prompt.contains("Goal: Ship the thing"));
        assert!(prompt.contains("Elapsed 1h 30m. Wake 3."));
        assert!(prompt.contains("Make progress or end the turn quietly."));
        assert!(prompt.contains(
            "Call goal.complete only when every step of the goal is checked and you can say why."
        ));
        assert!(!prompt.contains("objective is proven"));
        assert_eq!(elapsed_text(45), "45s");
        assert_eq!(elapsed_text(600), "10m");
    }

    #[test]
    fn complete_is_refused_while_a_step_is_open() {
        let dir = project_with_checklist(&[
            (true, "Split ran."),
            (false, "Oak is ready."),
            (false, "Pine is ready."),
        ]);
        let project = dir.path();
        start_goal(project, "Ship the thing").unwrap();
        let err = complete_goal(project, REASON).unwrap_err();
        assert!(err.starts_with("The goal is not complete yet."), "{err}");
        assert!(err.contains("2 steps are still open:"), "{err}");
        assert!(err.contains("\n- Oak is ready."), "{err}");
        assert!(err.contains("\n- Pine is ready."), "{err}");
        assert!(!err.contains("Split ran."), "{err}");
        assert!(err.contains(GOAL_COMPLETE_RULE), "{err}");
        assert!(
            err.contains("Say what is still open. Do not say the goal is complete."),
            "{err}"
        );
        assert_eq!(
            still_open_line(&open_steps(project)).as_deref(),
            Some("Goal still open · Oak is ready. +1 more")
        );
        assert_eq!(read_goal(project).unwrap().state, GoalState::Active);
    }

    #[test]
    fn complete_is_refused_without_a_sentence_of_reason() {
        let dir = project_with_checklist(&[(true, "Oak is ready.")]);
        let project = dir.path();
        start_goal(project, "Ship the thing").unwrap();
        for short in ["", "   ", "done", "Done. Yes.", "all steps checked"] {
            let err = complete_goal(project, short).unwrap_err();
            assert!(
                err.contains("Say why in at least one sentence"),
                "{short:?}: {err}"
            );
            assert!(err.contains("Every step is checked."), "{short:?}: {err}");
            assert!(!err.contains("Say what is still open"), "{short:?}: {err}");
            assert_eq!(read_goal(project).unwrap().state, GoalState::Active);
        }
        // Open steps and a missing reason are both named.
        let dir = project_with_checklist(&[(false, "Oak is ready.")]);
        let err = complete_goal(dir.path(), "").unwrap_err();
        assert!(
            err.contains("1 step is still open:\n- Oak is ready."),
            "{err}"
        );
        assert!(
            err.contains("Say what is still open. Do not say the goal is complete."),
            "{err}"
        );
        assert_eq!(
            still_open_line(&open_steps(dir.path())).as_deref(),
            Some("Goal still open · Oak is ready.")
        );
        assert!(err.contains("Say why in at least one sentence"), "{err}");
    }

    #[test]
    fn complete_is_allowed_when_every_step_is_ticked_with_a_reason() {
        let dir = project_with_checklist(&[
            (true, "Split ran."),
            (true, "Oak is ready."),
            (true, "Pine is ready."),
        ]);
        let project = dir.path();
        start_goal(project, "Ship the thing").unwrap();
        let done = complete_goal(project, &format!("  {REASON}  ")).unwrap();
        assert_eq!(done.state, GoalState::Complete);
        assert_eq!(done.reason, REASON);
        assert!(!done.completed.is_empty());
        let stored = read_goal(project).unwrap();
        assert_eq!(stored, done);
        assert_eq!(goal_row(project).unwrap().state, "complete");
        // No checklist at all leaves nothing open.
        let dir = project_with_checklist(&[]);
        start_goal(dir.path(), "Answer one question").unwrap();
        assert_eq!(
            complete_goal(dir.path(), REASON).unwrap().state,
            GoalState::Complete
        );
    }

    #[test]
    fn a_person_completes_without_evidence() {
        let dir = project_with_checklist(&[(false, "Oak is ready."), (false, "Pine is ready.")]);
        let project = dir.path();
        start_goal(project, "Ship the thing").unwrap();
        assert!(complete_goal(project, "").is_err());
        let done = force_complete_goal(project).unwrap();
        assert_eq!(done.state, GoalState::Complete);
        assert_eq!(done.reason, "Completed by the person.");
        assert_eq!(read_goal(project).unwrap().state, GoalState::Complete);
        assert!(take_goal_wake_at(project, false, at(23, 0)).is_none());
        // Without a goal there is nothing to complete, with or without force.
        assert!(force_complete_goal(dir.path().join("elsewhere").as_path()).is_err());
    }

    #[test]
    fn a_reason_is_a_sentence_not_a_word() {
        assert!(reason_is_a_sentence(REASON));
        assert!(reason_is_a_sentence(
            "The gate passed and the report is in."
        ));
        assert!(!reason_is_a_sentence(""));
        assert!(!reason_is_a_sentence("done"));
        assert!(!reason_is_a_sentence("Done, all good."));
        assert!(!reason_is_a_sentence(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
    }

    #[test]
    fn wake_headline_is_plain_words_and_clipped() {
        assert_eq!(
            goal_wake_headline("  Ship   the\nthing  "),
            "Goal check · Ship the thing"
        );
        let long = "a".repeat(100);
        let headline = goal_wake_headline(&long);
        assert_eq!(headline, format!("Goal check · {}…", "a".repeat(72)));
        let mut goal = record(GoalState::Active);
        goal.objective = long;
        let first = goal_wake_prompt(&goal, at(9, 30));
        let first = first.lines().next().unwrap();
        assert!(!first.contains('<'), "the headline is not a tag: {first}");
        assert!(first.starts_with("Goal check · "));
    }

    #[test]
    fn record_round_trips_and_ops_change_only_the_state() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        assert!(read_goal(project).is_none());
        assert!(goal_row(project).is_none());
        assert!(pause_goal(project).is_err());
        assert!(start_goal(project, "   ").is_err());

        let started = start_goal(project, "  Ship the thing  ").unwrap();
        assert_eq!(started.objective, "Ship the thing");
        assert_eq!(started.state, GoalState::Active);
        assert_eq!(read_goal(project).unwrap(), started);
        let row = goal_row(project).unwrap();
        assert_eq!(row.state, "active");
        assert_eq!(row.every_seconds, GOAL_INTERVAL_DEFAULT);

        assert_eq!(
            schedule_goal(project, 1).unwrap().every_seconds,
            GOAL_INTERVAL_MIN
        );
        assert_eq!(schedule_goal(project, 900).unwrap().every_seconds, 900);
        assert_eq!(goal_row(project).unwrap().every_seconds, 900);

        assert_eq!(pause_goal(project).unwrap().state, GoalState::Paused);
        assert_eq!(pause_goal(project).unwrap().state, GoalState::Paused);
        assert_eq!(resume_goal(project).unwrap().state, GoalState::Active);
        assert_eq!(read_goal(project).unwrap().every_seconds, 900);

        let complete = complete_goal(project, REASON).unwrap();
        assert_eq!(complete.state, GoalState::Complete);
        assert!(!complete.completed.is_empty());
        assert_eq!(complete.reason, REASON);
        assert!(resume_goal(project).is_err());
        assert!(pause_goal(project).is_err());
        assert_eq!(goal_row(project).unwrap().state, "complete");

        clear_goal(project).unwrap();
        assert!(read_goal(project).is_none());
        clear_goal(project).unwrap();
    }

    #[test]
    fn row_clock_runs_while_paused_and_stops_when_complete() {
        let mut paused = record(GoalState::Paused);
        paused.last_wake = at(8, 30).to_rfc3339();
        let row = goal_row_at(paused, at(10, 0)).unwrap();
        assert_eq!(row.elapsed_seconds, 7200);
        assert_eq!(row.state, "paused");
        assert_eq!(row.last_wake, at(8, 30).to_rfc3339());
        let mut done = record(GoalState::Complete);
        done.completed = at(9, 0).to_rfc3339();
        assert_eq!(goal_row_at(done, at(23, 0)).unwrap().elapsed_seconds, 3600);
    }

    #[test]
    fn loop_wakes_an_active_goal_and_stamps_it() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        start_goal_at(project, "Ship the thing", at(8, 0)).unwrap();
        // Too early, then Main busy, then due.
        assert!(take_goal_wake_at(project, false, at(8, 1)).is_none());
        assert!(take_goal_wake_at(project, true, at(8, 20)).is_none());
        let prompt = take_goal_wake_at(project, false, at(8, 20)).unwrap();
        assert!(prompt.contains("Wake 1."));
        let after = read_goal(project).unwrap();
        assert_eq!(after.wakes, 1);
        assert_eq!(after.last_wake, at(8, 20).to_rfc3339());
        assert_eq!(after.state, GoalState::Active);
        // Not again until the interval passes from that wake.
        assert!(take_goal_wake_at(project, false, at(8, 25)).is_none());
        assert!(take_goal_wake_at(project, false, at(8, 30)).is_some());
        assert_eq!(read_goal(project).unwrap().wakes, 2);
    }

    #[test]
    fn loop_leaves_paused_and_complete_goals_alone() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        start_goal_at(project, "Ship the thing", at(8, 0)).unwrap();
        pause_goal(project).unwrap();
        assert!(take_goal_wake_at(project, false, at(12, 0)).is_none());
        let paused = read_goal(project).unwrap();
        assert_eq!(paused.state, GoalState::Paused);
        assert_eq!(paused.wakes, 0);
        assert!(paused.last_wake.is_empty());
        complete_goal(project, REASON).unwrap();
        assert!(take_goal_wake_at(project, false, at(23, 0)).is_none());
        assert_eq!(read_goal(project).unwrap().state, GoalState::Complete);
    }
}
