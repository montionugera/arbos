//! What fills Main's context window, read from fx's session log.
//!
//! fx records the input tokens of every turn in `turn_completed`. It does not
//! record how those tokens split. The split here is an estimate: the texts
//! fx logged (conversation, summary) and the files fx loads (rules, skills)
//! are counted at four bytes a token; the Arbos prompt and fx's own base
//! prompt make the system prompt; whatever the measured first turn had on
//! top of that is the tool catalog.

use std::fs;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::arbos_layout;

/// `fx/src/builtins/system_prompt.md`, about 5.9 KB.
const FX_BASE_PROMPT_TOKENS: u64 = 1_500;
const BYTES_PER_TOKEN: u64 = 4;
/// Turns kept in `history`, newest last.
const HISTORY_TURNS: usize = 20;

/// One slice of the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Bucket {
    pub name: &'static str,
    pub tokens: u64,
}

/// Input and output tokens of one completed turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnTokens {
    pub index: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub exact: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageView {
    /// Tokens the last completed turn sent, or the bucket sum when fx has
    /// not recorded a turn yet.
    pub used: u64,
    pub window: Option<u64>,
    /// `used / window`, rounded, when the window is known.
    pub percent: Option<u64>,
    /// `used` comes from fx's count rather than the estimate.
    pub measured: bool,
    pub turns: usize,
    pub buckets: Vec<Bucket>,
    pub history: Vec<TurnTokens>,
}

/// Inputs that do not come from the log.
#[derive(Debug, Clone, Default)]
pub struct Fixed {
    pub system_prompt_tokens: u64,
    pub rules_tokens: u64,
    pub skills_tokens: u64,
}

/// Main's context usage for one project. `window` is the model's context
/// size when the engine knows it from fx's live usage update.
pub fn usage(project: &Path, window: Option<u64>) -> UsageView {
    let fx_home = project
        .join(".arbos")
        .join("agents")
        .join("main")
        .join("fx");
    let log = arbos_layout::latest_events_log(&fx_home)
        .and_then(|path| fs::read_to_string(path).ok())
        .unwrap_or_default();
    parse_usage(&log, &fixed_inputs(project, &fx_home), window)
}

/// The Arbos prompt, fx's base prompt, and the rule and skill files fx loads.
pub fn fixed_inputs(project: &Path, fx_home: &Path) -> Fixed {
    let prompt = arbos_layout::coordinator_prompt(&arbos_layout::read_user_preferences());
    let rules = [
        project.join("AGENTS.md"),
        fx_home.join(".fx").join("AGENTS.md"),
    ]
    .iter()
    .filter_map(|path| fs::metadata(path).ok())
    .map(|meta| meta.len())
    .sum::<u64>();
    let skills = [
        fx_home.join(".fx").join("skills"),
        project.join(".fx").join("skills"),
    ]
    .iter()
    .map(|dir| skills_bytes(dir))
    .sum::<u64>();
    Fixed {
        system_prompt_tokens: tokens(prompt.len() as u64) + FX_BASE_PROMPT_TOKENS,
        rules_tokens: tokens(rules),
        skills_tokens: tokens(skills),
    }
}

/// fx shows the model each skill's frontmatter, not the whole file.
fn skills_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| fs::read_to_string(entry.path().join("SKILL.md")).ok())
        .map(|text| frontmatter(&text).len() as u64)
        .sum()
}

fn frontmatter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---") else {
        return text.get(..200).unwrap_or(text);
    };
    match rest.find("\n---") {
        Some(end) => &rest[..end],
        None => text.get(..200).unwrap_or(text),
    }
}

fn tokens(bytes: u64) -> u64 {
    bytes.div_ceil(BYTES_PER_TOKEN)
}

#[derive(Default)]
struct Scan {
    /// Conversation tokens after the last checkpoint.
    conversation: u64,
    /// Conversation tokens since the last checkpoint, at the point the
    /// first measured turn completed.
    first_turn_conversation: u64,
    summary: u64,
    history: Vec<TurnTokens>,
    turns: usize,
    first_measured: Option<u64>,
    last_measured: Option<(u64, bool)>,
}

/// Read one fx `events.jsonl`. Lines that do not parse are skipped.
pub fn parse_usage(log: &str, fixed: &Fixed, window: Option<u64>) -> UsageView {
    let mut scan = Scan::default();
    for line in log.lines() {
        let Ok(frame) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some((kind, payload)) = frame
            .get("event")
            .and_then(Value::as_object)
            .and_then(|event| event.iter().next())
        else {
            continue;
        };
        let text_len = |key: &str| {
            payload
                .get(key)
                .and_then(Value::as_str)
                .map(|text| text.len() as u64)
                .unwrap_or(0)
        };
        match kind.as_str() {
            "user" | "assistant" | "steering" => {
                scan.conversation += tokens(text_len("text"));
            }
            "tool_call" => {
                scan.conversation += tokens(text_len("tool_name") + text_len("arguments_json"));
            }
            "tool_result" => {
                let bytes = payload
                    .get("output_bytes")
                    .and_then(Value::as_u64)
                    .or_else(|| payload.get("stored_bytes").and_then(Value::as_u64))
                    .unwrap_or_else(|| text_len("preview"));
                scan.conversation += tokens(bytes);
            }
            "context_checkpoint" => {
                scan.summary = tokens(text_len("summary"));
                scan.conversation = 0;
            }
            "turn_completed" => {
                scan.turns += 1;
                let progress = payload
                    .get("turn_summary")
                    .and_then(|summary| summary.get("token_progress"));
                let input = progress
                    .and_then(|progress| progress.get("input_tokens"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let output = progress
                    .and_then(|progress| progress.get("output_tokens"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let exact = progress
                    .and_then(|progress| progress.get("input_exact"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                scan.history.push(TurnTokens {
                    index: scan.turns,
                    input_tokens: input,
                    output_tokens: output,
                    exact,
                });
                if input > 0 {
                    if scan.first_measured.is_none() {
                        scan.first_measured = Some(input);
                        scan.first_turn_conversation = scan.conversation + scan.summary;
                    }
                    scan.last_measured = Some((input, exact));
                }
            }
            _ => {}
        }
    }
    let fixed_known = fixed.system_prompt_tokens + fixed.rules_tokens + fixed.skills_tokens;
    let tools = scan
        .first_measured
        .map(|first| first.saturating_sub(fixed_known + scan.first_turn_conversation))
        .unwrap_or(0);
    let buckets = vec![
        Bucket {
            name: "system prompt",
            tokens: fixed.system_prompt_tokens,
        },
        Bucket {
            name: "tools",
            tokens: tools,
        },
        Bucket {
            name: "rules",
            tokens: fixed.rules_tokens,
        },
        Bucket {
            name: "skills",
            tokens: fixed.skills_tokens,
        },
        Bucket {
            name: "summarized conversation",
            tokens: scan.summary,
        },
        Bucket {
            name: "conversation",
            tokens: scan.conversation,
        },
    ];
    let estimate: u64 = buckets.iter().map(|bucket| bucket.tokens).sum();
    let (used, measured) = match scan.last_measured {
        Some((input, _)) => (input, true),
        None => (estimate, false),
    };
    let percent = window
        .filter(|window| *window > 0)
        .map(|window| (used * 100).div_ceil(window).min(100));
    let skip = scan.history.len().saturating_sub(HISTORY_TURNS);
    UsageView {
        used,
        window,
        percent,
        measured,
        turns: scan.turns,
        buckets,
        history: scan.history.into_iter().skip(skip).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u64, event: Value) -> String {
        serde_json::json!({
            "schema_version": 1,
            "seq": seq,
            "timestamp_ms": 1_700_000_000_000u64 + seq,
            "event": event,
        })
        .to_string()
    }

    fn turn(input: u64, output: u64, exact: bool) -> Value {
        serde_json::json!({ "turn_completed": {
            "files": [],
            "turn_summary": {
                "started_at_ms": 1, "completed_at_ms": 2,
                "thinking_duration_ms": 0, "turn_duration_ms": 1,
                "token_progress": {
                    "input_tokens": input, "output_tokens": output,
                    "input_exact": exact, "output_exact": true
                }
            }
        }})
    }

    fn fixed() -> Fixed {
        Fixed {
            system_prompt_tokens: 2_000,
            rules_tokens: 300,
            skills_tokens: 50,
        }
    }

    #[test]
    fn the_breakdown_comes_from_the_turns_fx_logged() {
        let log = [
            frame(
                1,
                serde_json::json!({ "user": { "text": "x".repeat(400) } }),
            ),
            frame(
                2,
                serde_json::json!({ "tool_call": {
                    "call_id": "c1", "tool_name": "shell", "arguments_json": "y".repeat(96)
                }}),
            ),
            frame(
                3,
                serde_json::json!({ "tool_result": {
                    "call_id": "c1", "tool_name": "shell", "status": "ok",
                    "artifact_ref": "a", "output_bytes": 800, "stored_bytes": 800,
                    "completeness": "complete"
                }}),
            ),
            frame(
                4,
                serde_json::json!({ "assistant": { "text": "z".repeat(200) } }),
            ),
            frame(5, turn(8_000, 60, true)),
            frame(6, serde_json::json!({ "user": { "text": "w".repeat(40) } })),
            frame(7, turn(8_400, 20, true)),
            "not json at all".to_owned(),
        ]
        .join("\n");
        let view = parse_usage(&log, &fixed(), Some(128_000));
        // First turn: 100 user + 25 shell + 1 name + 200 output + 50 reply = 376.
        let first_conversation = 100 + 25 + 1 + 200 + 50;
        assert_eq!(view.turns, 2);
        assert!(view.measured);
        assert_eq!(view.used, 8_400);
        assert_eq!(view.window, Some(128_000));
        assert_eq!(view.percent, Some(7));
        let bucket = |name: &str| {
            view.buckets
                .iter()
                .find(|bucket| bucket.name == name)
                .map(|bucket| bucket.tokens)
                .unwrap()
        };
        assert_eq!(bucket("system prompt"), 2_000);
        assert_eq!(bucket("rules"), 300);
        assert_eq!(bucket("skills"), 50);
        assert_eq!(bucket("tools"), 8_000 - 2_350 - first_conversation);
        assert_eq!(bucket("summarized conversation"), 0);
        assert_eq!(bucket("conversation"), first_conversation + 10);
        assert_eq!(
            view.buckets.iter().map(|b| b.name).collect::<Vec<_>>(),
            [
                "system prompt",
                "tools",
                "rules",
                "skills",
                "summarized conversation",
                "conversation"
            ]
        );
        assert_eq!(view.history.len(), 2);
        assert_eq!(view.history[1].input_tokens, 8_400);
        assert_eq!(view.history[1].output_tokens, 20);
        assert!(view.history[1].exact);
    }

    #[test]
    fn a_checkpoint_moves_earlier_turns_into_the_summary() {
        let log = [
            frame(
                1,
                serde_json::json!({ "user": { "text": "x".repeat(4_000) } }),
            ),
            frame(
                2,
                serde_json::json!({ "assistant": { "text": "y".repeat(4_000) } }),
            ),
            frame(3, turn(5_000, 1_000, true)),
            frame(
                4,
                serde_json::json!({ "context_checkpoint": {
                    "covers_through_seq": 3, "summary": "s".repeat(1_200)
                }}),
            ),
            frame(
                5,
                serde_json::json!({ "user": { "text": "next".repeat(10) } }),
            ),
            frame(6, turn(4_000, 10, false)),
        ]
        .join("\n");
        let view = parse_usage(&log, &fixed(), None);
        let bucket = |name: &str| {
            view.buckets
                .iter()
                .find(|bucket| bucket.name == name)
                .map(|bucket| bucket.tokens)
                .unwrap()
        };
        assert_eq!(bucket("summarized conversation"), 300);
        assert_eq!(bucket("conversation"), 10);
        assert_eq!(bucket("tools"), 5_000 - 2_350 - 2_000);
        assert_eq!(view.used, 4_000);
        assert_eq!(view.percent, None);
        assert!(!view.history[1].exact);
    }

    #[test]
    fn an_empty_log_sums_the_estimates() {
        let view = parse_usage("", &fixed(), Some(1_000));
        assert!(!view.measured);
        assert_eq!(view.turns, 0);
        assert_eq!(view.used, 2_350);
        assert_eq!(view.percent, Some(100));
        assert!(view.history.is_empty());
    }

    #[test]
    fn fixed_inputs_count_rules_and_skill_frontmatter() {
        let root = std::env::temp_dir().join(format!("arbos-context-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let fx_home = root.join(".arbos").join("agents").join("main").join("fx");
        fs::create_dir_all(fx_home.join(".fx").join("skills").join("notes")).unwrap();
        fs::write(root.join("AGENTS.md"), "r".repeat(400)).unwrap();
        fs::write(fx_home.join(".fx").join("AGENTS.md"), "g".repeat(40)).unwrap();
        fs::write(
            fx_home
                .join(".fx")
                .join("skills")
                .join("notes")
                .join("SKILL.md"),
            format!(
                "---\nname: notes\ndescription: {}\n---\n{}",
                "d".repeat(60),
                "b".repeat(4_000)
            ),
        )
        .unwrap();
        let fixed = fixed_inputs(&root, &fx_home);
        assert_eq!(fixed.rules_tokens, 110);
        assert!(
            fixed.skills_tokens >= 20 && fixed.skills_tokens < 30,
            "{}",
            fixed.skills_tokens
        );
        assert!(fixed.system_prompt_tokens > FX_BASE_PROMPT_TOKENS);
        let _ = fs::remove_dir_all(&root);
    }
}
