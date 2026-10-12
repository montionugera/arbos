//! What the project shows while Main works: the live step, the unread flag,
//! and the links in Main's reply. State lives in `.arbos/runtime/live.json`,
//! which git ignores. The derived step fallback is in memory only.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use zeron_proto::{AgentEvent, ToolCall};

const MAX_STEP_WORDS: usize = 6;

/// Imperative verbs the step check accepts when the first word does not end
/// in `ing`. The `ing` form is accepted on its own.
const VERBS: &[&str] = &[
    "add",
    "ask",
    "build",
    "check",
    "clean",
    "close",
    "compare",
    "draft",
    "fix",
    "gather",
    "look",
    "merge",
    "move",
    "open",
    "plan",
    "read",
    "review",
    "run",
    "search",
    "send",
    "set",
    "sort",
    "spawn",
    "split",
    "start",
    "stop",
    "sum",
    "summarize",
    "test",
    "update",
    "verify",
    "wait",
    "watch",
    "write",
];

#[derive(Debug, Default, Serialize, Deserialize)]
struct LiveFile {
    #[serde(default)]
    unread: bool,
    #[serde(default)]
    step: String,
}

/// What the UI reads for one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveView {
    /// Main finished a turn that showed text, and nobody has looked yet.
    pub unread: bool,
    /// The live step, from `status.step` or derived from what Main is doing.
    /// Empty when Main is idle and set nothing.
    pub step: String,
    /// `set` when Main wrote the step, `derived` for the fallback, `none`.
    pub step_source: &'static str,
}

/// One link in Main's reply that points at a local path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkCheck {
    pub target: String,
    pub image: bool,
    pub ok: bool,
}

/// Marker the UI drops: a broken local link becomes `[label](arbos-missing:path)`.
pub const MISSING_LINK_SCHEME: &str = "arbos-missing:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Activity {
    Messaging,
    Command,
    Writing,
}

fn live_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn activities() -> std::sync::MutexGuard<'static, HashMap<PathBuf, (Activity, String)>> {
    static MAP: OnceLock<Mutex<HashMap<PathBuf, (Activity, String)>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn live_path(project: &Path) -> PathBuf {
    project.join(".arbos").join("runtime").join("live.json")
}

fn read_live(project: &Path) -> LiveFile {
    fs::read_to_string(live_path(project))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_live(project: &Path, file: &LiveFile) -> Result<(), String> {
    let path = live_path(project);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let text = serde_json::to_string_pretty(file).map_err(|err| err.to_string())?;
    fs::write(path, text).map_err(|err| err.to_string())
}

/// The one wake Main receives when a worker turn ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerWake {
    pub worker: String,
    pub main_chat_id: String,
    /// `Worker <id> done.` and the worker's last reply on the lines after.
    pub body: String,
}

/// Record the end of a worker turn and build the wake for Main. The first
/// call for a turn returns the wake; later calls for the same turn return
/// nothing, so Main sees one `done` per worker turn.
pub fn worker_wake(project: &Path, chat_id: &str, label: &str, reply: &str) -> Option<WorkerWake> {
    let done = crate::arbos_layout::finish_worker(project, chat_id, label, reply)?;
    let body = if done.result.is_empty() {
        format!("Worker {} {label}.", done.id)
    } else {
        format!("Worker {} {label}.\n{}", done.id, done.result)
    };
    Some(WorkerWake {
        worker: done.id,
        main_chat_id: done.main_chat_id,
        body,
    })
}

/// The unread flag and the live step for one project.
pub fn live(project: &Path) -> LiveView {
    let _guard = live_lock();
    let file = read_live(project);
    let step = file.step.trim();
    if !step.is_empty() {
        return LiveView {
            unread: file.unread,
            step: step.to_owned(),
            step_source: "set",
        };
    }
    let derived = activities()
        .get(project)
        .map(|(activity, name)| derived_step(*activity, name));
    match derived {
        Some(step) => LiveView {
            unread: file.unread,
            step,
            step_source: "derived",
        },
        None => LiveView {
            unread: file.unread,
            step: String::new(),
            step_source: "none",
        },
    }
}

/// `status.step`. Six words at most; the first word is a verb.
pub fn set_step(project: &Path, text: &str) -> Result<String, String> {
    let step = check_step(text)?;
    let _guard = live_lock();
    let mut file = read_live(project);
    file.step = step.clone();
    write_live(project, &file)?;
    Ok(step)
}

/// Validate a step line and return it trimmed.
pub fn check_step(text: &str) -> Result<String, String> {
    let step = text.split_whitespace().collect::<Vec<_>>();
    if step.is_empty() {
        return Err("the step is empty".into());
    }
    if step.len() > MAX_STEP_WORDS {
        return Err(format!("the step has more than {MAX_STEP_WORDS} words"));
    }
    if !is_verb(step[0]) {
        return Err("the step must start with a verb".into());
    }
    Ok(step.join(" "))
}

fn is_verb(word: &str) -> bool {
    let word = word
        .trim_matches(|ch: char| !ch.is_alphanumeric())
        .to_ascii_lowercase();
    if word.is_empty() {
        return false;
    }
    (word.len() >= 5 && word.ends_with("ing")) || VERBS.contains(&word.as_str())
}

/// The person looked at the project.
pub fn mark_seen(project: &Path) -> Result<(), String> {
    let _guard = live_lock();
    let mut file = read_live(project);
    if !file.unread {
        return Ok(());
    }
    file.unread = false;
    write_live(project, &file)
}

/// A Main turn ended. The step clears. Unread is set only when the turn
/// showed text.
pub fn end_main_turn(project: &Path, showed_text: bool) {
    activities().remove(project);
    let _guard = live_lock();
    let mut file = read_live(project);
    let changed = !file.step.is_empty() || (showed_text && !file.unread);
    file.step.clear();
    if showed_text {
        file.unread = true;
    }
    if changed && let Err(err) = write_live(project, &file) {
        tracing::warn!(error = %err, "live state was not written");
    }
}

/// Watch Main's stream for the derived step.
pub fn note_activity(project: &Path, event: &AgentEvent) {
    let next = match event {
        AgentEvent::ToolCall { call, .. } => match call {
            ToolCall::Exec { .. } => Some((Activity::Command, String::new())),
            ToolCall::Mcp {
                server,
                tool,
                input,
            } if server == "arbos" => {
                if matches!(tool.as_str(), "agents.send" | "agents.spawn") {
                    let name = input
                        .as_ref()
                        .and_then(|input| input.get("name"))
                        .and_then(|name| name.as_str())
                        .unwrap_or("a worker")
                        .to_owned();
                    Some((Activity::Messaging, name))
                } else {
                    None
                }
            }
            ToolCall::Unknown { name, input } if name.starts_with("mcp_arbos_agents_s") => {
                let who = input
                    .as_ref()
                    .and_then(|input| input.get("name"))
                    .and_then(|name| name.as_str())
                    .unwrap_or("a worker")
                    .to_owned();
                Some((Activity::Messaging, who))
            }
            _ => None,
        },
        AgentEvent::TextDelta { text } if !text.trim().is_empty() => {
            Some((Activity::Writing, String::new()))
        }
        _ => None,
    };
    if let Some(next) = next {
        activities().insert(project.to_path_buf(), next);
    }
}

fn derived_step(activity: Activity, name: &str) -> String {
    match activity {
        Activity::Messaging => format!("Messaging {name}"),
        Activity::Command => "Running a command".into(),
        Activity::Writing => "Writing a reply".into(),
    }
}

/// Check every local path linked in `text`. Links with a URL scheme are
/// skipped. Paths resolve against the project folder.
pub fn check_links(project: &Path, text: &str) -> Vec<LinkCheck> {
    links(text)
        .into_iter()
        .map(|(_, target, image_mark)| {
            let path = resolve(project, &target);
            let image = image_mark || looks_like_image(&target);
            let ok = if image {
                image_decodes(&path)
            } else {
                path.exists()
            };
            LinkCheck { target, image, ok }
        })
        .collect()
}

/// Rewrite broken local links with [`MISSING_LINK_SCHEME`] so the UI can drop
/// them. Returns the new text and the targets that were missing.
pub fn mark_missing_links(project: &Path, text: &str) -> (String, Vec<String>) {
    let mut missing = Vec::new();
    let mut out = text.to_owned();
    for check in check_links(project, text) {
        if check.ok || check.target.starts_with(MISSING_LINK_SCHEME) {
            continue;
        }
        let from = format!("]({})", check.target);
        let to = format!("]({MISSING_LINK_SCHEME}{})", check.target);
        out = out.replace(&from, &to);
        missing.push(check.target);
    }
    (out, missing)
}

/// `(label, target, is_image)` for every markdown link whose target is a
/// local path.
fn links(text: &str) -> Vec<(String, String, bool)> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        let image = i > 0 && bytes[i - 1] == b'!';
        let Some(close) = text[i..].find("](") else {
            break;
        };
        let label_end = i + close;
        let target_start = label_end + 2;
        let Some(end) = text[target_start..].find(')') else {
            break;
        };
        let target_end = target_start + end;
        let label = text[i + 1..label_end].to_owned();
        let target = text[target_start..target_end]
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(['<', '>'])
            .to_owned();
        i = target_end + 1;
        if target.is_empty() || target.starts_with('#') || has_scheme(&target) {
            continue;
        }
        found.push((label, target, image));
    }
    found
}

fn has_scheme(target: &str) -> bool {
    let Some((scheme, _)) = target.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

fn resolve(project: &Path, target: &str) -> PathBuf {
    let target = target.strip_prefix("file://").unwrap_or(target);
    let path = Path::new(target);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    if let Some(rest) = target.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return Path::new(&home).join(rest);
    }
    project.join(path)
}

fn looks_like_image(target: &str) -> bool {
    let lower = target.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// The file is present and its header is one the image view can decode.
fn image_decodes(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    image_header_ok(&bytes)
}

fn image_header_ok(bytes: &[u8]) -> bool {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 12 {
        return false;
    }
    if bytes.starts_with(PNG) {
        return bytes.len() > 24 && &bytes[12..16] == b"IHDR";
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return bytes.ends_with(&[0xFF, 0xD9]);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return true;
    }
    if bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return true;
    }
    bytes.starts_with(b"BM")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("arbos-live-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".arbos").join("runtime")).unwrap();
        root
    }

    #[test]
    fn a_worker_turn_wakes_main_once_with_the_last_reply() {
        let root = scratch("wake");
        crate::arbos_layout::ensure(&root, Some("Wake")).unwrap();
        crate::arbos_layout::claim_main(&root, "main-chat").unwrap();
        crate::arbos_layout::reserve_worker(&root, "docs", "Write the note", "glm-5.3", None)
            .unwrap();
        crate::arbos_layout::bind_worker_chat(&root, "docs", "docs-chat").unwrap();
        let first = worker_wake(
            &root,
            "docs-chat",
            "done",
            "The note is at research/note.md.",
        )
        .expect("first end of turn wakes Main");
        assert_eq!(first.worker, "docs");
        assert_eq!(first.main_chat_id, "main-chat");
        assert_eq!(
            first.body,
            "Worker docs done.\nThe note is at research/note.md."
        );
        assert!(worker_wake(&root, "docs-chat", "done", "again").is_none());
        assert!(worker_wake(&root, "main-chat", "done", "main is not a worker").is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn step_needs_a_verb_and_six_words_at_most() {
        assert_eq!(
            check_step("Checking the gate").unwrap(),
            "Checking the gate"
        );
        assert_eq!(
            check_step("  Review   the  diff ").unwrap(),
            "Review the diff"
        );
        assert!(check_step("").is_err());
        assert!(check_step("The gate is red").is_err());
        assert!(check_step("Checking one two three four five six").is_err());
        assert!(check_step("Checking one two three four five").is_ok());
    }

    #[test]
    fn step_is_stored_then_cleared_at_turn_end() {
        let root = scratch("step");
        assert!(set_step(&root, "gate is red").is_err());
        set_step(&root, "Checking the gate").unwrap();
        let view = live(&root);
        assert_eq!(view.step, "Checking the gate");
        assert_eq!(view.step_source, "set");
        end_main_turn(&root, false);
        let view = live(&root);
        assert_eq!(view.step, "");
        assert_eq!(view.step_source, "none");
        assert!(!view.unread);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn derived_step_follows_what_main_does() {
        let root = scratch("derived");
        note_activity(
            &root,
            &AgentEvent::ToolCall {
                id: "t1".into(),
                call: ToolCall::Exec {
                    command: "cargo test".into(),
                },
            },
        );
        assert_eq!(live(&root).step, "Running a command");
        assert_eq!(live(&root).step_source, "derived");
        note_activity(
            &root,
            &AgentEvent::ToolCall {
                id: "t2".into(),
                call: ToolCall::Mcp {
                    server: "arbos".into(),
                    tool: "agents.send".into(),
                    input: Some(serde_json::json!({ "name": "docs", "text": "go" })),
                },
            },
        );
        assert_eq!(live(&root).step, "Messaging docs");
        note_activity(
            &root,
            &AgentEvent::TextDelta {
                text: "Done.".into(),
            },
        );
        assert_eq!(live(&root).step, "Writing a reply");
        set_step(&root, "Waiting for docs").unwrap();
        assert_eq!(live(&root).step, "Waiting for docs");
        end_main_turn(&root, true);
        assert_eq!(live(&root).step, "");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unread_is_set_only_by_a_turn_with_text() {
        let root = scratch("unread");
        end_main_turn(&root, false);
        assert!(!live(&root).unread);
        end_main_turn(&root, true);
        assert!(live(&root).unread);
        mark_seen(&root).unwrap();
        assert!(!live(&root).unread);
        end_main_turn(&root, false);
        assert!(!live(&root).unread);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn links_are_checked_and_missing_ones_are_marked() {
        let root = scratch("links");
        fs::write(root.join("note.md"), "hi").unwrap();
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&[0u8; 20]);
        fs::write(root.join("ok.png"), png).unwrap();
        fs::write(root.join("bad.png"), b"not an image at all").unwrap();
        let text = "See [the note](note.md), ![shot](ok.png), ![broken](bad.png), \
                    [gone](missing/file.md), [web](https://example.com), [top](#here).";
        let checks = check_links(&root, text);
        assert_eq!(checks.len(), 4);
        assert!(checks[0].ok && !checks[0].image);
        assert!(checks[1].ok && checks[1].image);
        assert!(!checks[2].ok && checks[2].image);
        assert!(!checks[3].ok && !checks[3].image);
        let (marked, missing) = mark_missing_links(&root, text);
        assert_eq!(
            missing,
            vec!["bad.png".to_owned(), "missing/file.md".to_owned()]
        );
        assert!(marked.contains("](note.md)"));
        assert!(marked.contains("](ok.png)"));
        assert!(marked.contains("](arbos-missing:bad.png)"));
        assert!(marked.contains("](arbos-missing:missing/file.md)"));
        assert!(marked.contains("](https://example.com)"));
        let (again, missing_again) = mark_missing_links(&root, &marked);
        assert_eq!(again, marked);
        assert!(missing_again.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn image_headers_are_recognized() {
        assert!(image_header_ok(b"GIF89a\0\0\0\0\0\0\0\0"));
        assert!(image_header_ok(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!image_header_ok(b"\xFF\xD8\xFF\xE0 cut off jpeg"));
        assert!(image_header_ok(b"\xFF\xD8\xFF\xE0 whole jpeg \xFF\xD9"));
        assert!(!image_header_ok(b"short"));
    }
}
