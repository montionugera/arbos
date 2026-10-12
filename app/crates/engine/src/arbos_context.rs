//! What fills Main's context window, read from fx's session log.
//!
//! fx writes a `context_sections` frame just before each turn it commits:
//! the sizes of that turn's last model request by section (system prompt,
//! tools, rules, skills, summarized conversation, conversation) and the
//! request as a whole, all in fx's own token estimate. When the log has one,
//! the view is built from it and marked `measured`.
//!
//! Older logs have no such frame. Then the split is an estimate: the texts
//! fx logged (conversation, summary) and the files fx loads (rules, skills)
//! are counted at four bytes a token, and the Arbos prompt plus fx's own
//! base prompt make the system prompt. The tool catalog is unknown without
//! fx's measurement and reads as zero. `turn_completed` only counts the
//! prompt text the user submitted, so it never stands in for the request.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
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

/// What fx counted for one completed turn: the prompt text the user
/// submitted and the output the model produced.
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
    /// fx's estimate of the last request when measured, else the bucket sum.
    pub used: u64,
    pub window: Option<u64>,
    /// `used / window`, rounded, when the window is known.
    pub percent: Option<u64>,
    /// The buckets are fx's own section sizes rather than the estimate.
    pub measured: bool,
    pub turns: usize,
    pub buckets: Vec<Bucket>,
    pub history: Vec<TurnTokens>,
}

/// One `context_sections` frame, as fx writes it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct Sections {
    system_prompt: u64,
    tools: u64,
    rules: u64,
    skills: u64,
    summarized_conversation: u64,
    conversation: u64,
    request: u64,
}

impl Sections {
    fn buckets(self) -> Vec<Bucket> {
        bucket_list([
            self.system_prompt,
            self.tools,
            self.rules,
            self.skills,
            self.summarized_conversation,
            self.conversation,
        ])
    }
}

const BUCKET_NAMES: [&str; 6] = [
    "system prompt",
    "tools",
    "rules",
    "skills",
    "summarized conversation",
    "conversation",
];

fn bucket_list(tokens: [u64; 6]) -> Vec<Bucket> {
    BUCKET_NAMES
        .iter()
        .zip(tokens)
        .map(|(name, tokens)| Bucket { name, tokens })
        .collect()
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
    summary: u64,
    history: Vec<TurnTokens>,
    turns: usize,
    /// The last `context_sections` frame, which fx writes just before the
    /// turn it describes.
    sections: Option<Sections>,
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
            "context_sections" => {
                if let Ok(sections) = serde_json::from_value::<Sections>(payload.clone()) {
                    scan.sections = Some(sections);
                }
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
            }
            _ => {}
        }
    }
    let (buckets, measured) = match scan.sections {
        Some(sections) => (sections.buckets(), true),
        None => (
            bucket_list([
                fixed.system_prompt_tokens,
                0,
                fixed.rules_tokens,
                fixed.skills_tokens,
                scan.summary,
                scan.conversation,
            ]),
            false,
        ),
    };
    let sum: u64 = buckets.iter().map(|bucket| bucket.tokens).sum();
    let used = match scan.sections {
        Some(sections) if sections.request > 0 => sections.request,
        _ => sum,
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

    fn sections(system_prompt: u64, tools: u64, conversation: u64, request: u64) -> Value {
        serde_json::json!({ "context_sections": {
            "system_prompt": system_prompt, "tools": tools, "rules": 310, "skills": 45,
            "summarized_conversation": 900, "conversation": conversation,
            "request": request
        }})
    }

    fn bucket(view: &UsageView, name: &str) -> u64 {
        view.buckets
            .iter()
            .find(|bucket| bucket.name == name)
            .map(|bucket| bucket.tokens)
            .unwrap()
    }

    #[test]
    fn logged_sections_fill_the_buckets_and_the_latest_turn_wins() {
        let log = [
            frame(1, sections(1_400, 3_900, 200, 6_800)),
            frame(
                2,
                serde_json::json!({ "user": { "text": "x".repeat(400) } }),
            ),
            frame(
                3,
                serde_json::json!({ "assistant": { "text": "z".repeat(200) } }),
            ),
            frame(4, turn(100, 60, true)),
            frame(5, sections(1_400, 3_900, 2_600, 9_200)),
            frame(6, serde_json::json!({ "user": { "text": "w".repeat(40) } })),
            frame(7, turn(10, 20, true)),
            "not json at all".to_owned(),
        ]
        .join("\n");
        let view = parse_usage(&log, &fixed(), Some(128_000));
        assert_eq!(view.turns, 2);
        assert!(view.measured);
        assert_eq!(view.used, 9_200);
        assert_eq!(view.window, Some(128_000));
        assert_eq!(view.percent, Some(8));
        assert_eq!(bucket(&view, "system prompt"), 1_400);
        assert_eq!(bucket(&view, "tools"), 3_900);
        assert_eq!(bucket(&view, "rules"), 310);
        assert_eq!(bucket(&view, "skills"), 45);
        assert_eq!(bucket(&view, "summarized conversation"), 900);
        assert_eq!(bucket(&view, "conversation"), 2_600);
        assert_eq!(
            view.buckets.iter().map(|b| b.name).collect::<Vec<_>>(),
            BUCKET_NAMES
        );
        assert_eq!(view.history.len(), 2);
        assert_eq!(view.history[1].input_tokens, 10);
        assert_eq!(view.history[1].output_tokens, 20);
        assert!(view.history[1].exact);
    }

    #[test]
    fn sections_without_a_request_total_sum_their_buckets() {
        let log = [
            frame(
                1,
                serde_json::json!({ "context_sections": { "system_prompt": 1_000, "tools": 500 } }),
            ),
            frame(2, serde_json::json!({ "user": { "text": "hello" } })),
            frame(3, turn(2, 1, true)),
        ]
        .join("\n");
        let view = parse_usage(&log, &fixed(), Some(3_000));
        assert!(view.measured);
        assert_eq!(view.used, 1_500);
        assert_eq!(view.percent, Some(50));
        assert_eq!(bucket(&view, "conversation"), 0);
    }

    #[test]
    fn a_log_without_sections_falls_back_to_the_estimate() {
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
            frame(5, turn(100, 60, true)),
            frame(6, serde_json::json!({ "user": { "text": "w".repeat(40) } })),
            frame(7, turn(10, 20, true)),
        ]
        .join("\n");
        let view = parse_usage(&log, &fixed(), Some(128_000));
        // 100 user + 25 shell + 1 name + 200 output + 50 reply + 10 user = 386.
        let conversation = 100 + 25 + 1 + 200 + 50 + 10;
        assert_eq!(view.turns, 2);
        assert!(!view.measured);
        assert_eq!(bucket(&view, "system prompt"), 2_000);
        assert_eq!(bucket(&view, "tools"), 0);
        assert_eq!(bucket(&view, "rules"), 300);
        assert_eq!(bucket(&view, "skills"), 50);
        assert_eq!(bucket(&view, "summarized conversation"), 0);
        assert_eq!(bucket(&view, "conversation"), conversation);
        assert_eq!(view.used, 2_350 + conversation);
        assert_eq!(view.percent, Some(3));
        assert_eq!(view.history.len(), 2);
        assert_eq!(view.history[0].input_tokens, 100);
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
        assert_eq!(bucket(&view, "summarized conversation"), 300);
        assert_eq!(bucket(&view, "conversation"), 10);
        assert!(!view.measured);
        assert_eq!(view.used, 2_350 + 300 + 10);
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
