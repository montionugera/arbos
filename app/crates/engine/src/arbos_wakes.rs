//! `.arbos/subscriptions/*.toml` fired into the Main chat.
//!
//! A due file becomes a Worked wake: the same mark a finished worker uses, so
//! the transcript shows a system line and fx receives `_meta.fx.wake`. No user
//! bubble is written. Fire times live in `.arbos/runtime/`, which git ignores.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Timelike;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeron_doc::SessionCommandPayload;
use zeron_proto::{HarnessId, RunRequest, SandboxLevel};

use crate::doc_host::DocHost;
use crate::sessions::SessionsEngine;
use crate::workspace_host::WorkspaceHost;

const TICK: Duration = Duration::from_secs(5);
const GH_EVERY_TICKS: u64 = 4;

#[derive(Debug, Deserialize)]
struct Subscription {
    kind: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    every_seconds: u64,
    #[serde(default)]
    hour: Option<u32>,
    #[serde(default)]
    minute: Option<u32>,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    pr: Option<u64>,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    channel: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WakeState {
    #[serde(default)]
    armed: bool,
    #[serde(default)]
    next_unix: i64,
    #[serde(default)]
    last_key: String,
}

/// Poll open projects. One task, started with the engine.
pub fn spawn(host: DocHost, workspace: WorkspaceHost, sessions: SessionsEngine) {
    tokio::spawn(async move {
        let mut ticks = 0u64;
        loop {
            tokio::time::sleep(TICK).await;
            ticks = ticks.wrapping_add(1);
            let host = host.clone();
            let workspace = workspace.clone();
            let sessions = sessions.clone();
            let poll_github = ticks % GH_EVERY_TICKS == 0;
            let joined = tokio::task::spawn_blocking(move || {
                tick(&host, &workspace, &sessions, poll_github)
            })
            .await;
            if let Err(err) = joined {
                tracing::warn!(error = %err, "subscription tick stopped");
            }
        }
    });
}

fn tick(host: &DocHost, workspace: &WorkspaceHost, sessions: &SessionsEngine, poll_github: bool) {
    crate::arbos_remote::mirror(host, workspace);
    for (chat_id, cwd) in main_chats(workspace) {
        if let Err(err) = tick_project(host, sessions, &chat_id, &cwd, poll_github) {
            tracing::warn!(chat = %chat_id, error = %err, "subscription project tick failed");
        }
    }
}

fn main_chats(workspace: &WorkspaceHost) -> Vec<(String, String)> {
    let Ok(chats) = workspace.read_chats() else {
        return Vec::new();
    };
    chats
        .into_iter()
        .filter_map(|chat| {
            if chat.archived || !chat.id.ends_with("-main") {
                return None;
            }
            let cwd = chat.cwd?;
            let project = Path::new(&cwd);
            let root = project.join(".arbos").join("project.toml");
            if !root.is_file() || !crate::arbos_remote::owns_project(project) {
                return None;
            }
            Some((chat.id, cwd))
        })
        .collect()
}

fn tick_project(
    host: &DocHost,
    sessions: &SessionsEngine,
    chat_id: &str,
    cwd: &str,
    poll_github: bool,
) -> Result<(), String> {
    let dir = Path::new(cwd).join(".arbos").join("subscriptions");
    if !dir.is_dir() {
        return Ok(());
    }
    let mut state = read_state(cwd);
    let mut names = fs::read_dir(&dir)
        .map_err(|err| err.to_string())?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("toml"))
        .collect::<Vec<_>>();
    names.sort();
    let mut changed = false;
    for path in names {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!(file = name, error = %err, "subscription was not read");
                continue;
            }
        };
        let sub: Subscription = match toml::from_str(&text) {
            Ok(sub) => sub,
            Err(err) => {
                tracing::warn!(file = name, error = %err, "subscription was not parsed");
                continue;
            }
        };
        let slot = state.entry(name.to_owned()).or_default();
        let before = serde_json::to_string(&*slot).unwrap_or_default();
        let body = match sub.kind.as_str() {
            "timer" => timer_body(&sub, slot).map(|due| due.then(|| wake_text(&sub, name))),
            "github_pr" if poll_github => github_pr_body(&sub, slot),
            "github_ci" if poll_github => github_ci_body(&sub, slot),
            "slack" if poll_github => slack_body(&sub, slot),
            "github_pr" | "github_ci" | "slack" => Ok(None),
            other => {
                tracing::warn!(file = name, kind = other, "subscription kind is unknown");
                Ok(None)
            }
        };
        let after = serde_json::to_string(slot).unwrap_or_default();
        if before != after {
            changed = true;
        }
        match body {
            Ok(Some(body)) => {
                match queue_wake(host, sessions, chat_id, cwd, &body) {
                    Ok(()) => {
                        let id = crate::new_id();
                        let line = format!("Worked · {}", body.trim());
                        crate::arbos_remote::note_worked(cwd, &id, &line);
                        tracing::info!(chat = %chat_id, file = name, "subscription wake queued");
                    }
                    Err(err) => {
                        tracing::warn!(chat = %chat_id, file = name, error = %err, "subscription wake was not queued")
                    }
                }
            }
            Ok(None) => {}
            Err(err) => tracing::warn!(file = name, error = %err, "subscription was not checked"),
        }
    }
    if changed {
        write_state(cwd, &state)?;
    }
    Ok(())
}

fn wake_text(sub: &Subscription, name: &str) -> String {
    let prompt = sub.prompt.trim();
    if prompt.is_empty() {
        format!("Subscription {name} fired.")
    } else {
        format!("Subscription {name} fired.\n{prompt}")
    }
}

fn timer_body(sub: &Subscription, state: &mut WakeState) -> Result<bool, String> {
    if sub.every_seconds > 0 {
        let now = chrono::Utc::now().timestamp();
        if !state.armed {
            state.armed = true;
            state.next_unix = now.saturating_add(sub.every_seconds as i64);
            return Ok(false);
        }
        if now < state.next_unix {
            return Ok(false);
        }
        state.next_unix = now.saturating_add(sub.every_seconds as i64);
        state.last_key = now.to_string();
        return Ok(true);
    }
    let hour = sub.hour.unwrap_or(8);
    let minute = sub.minute.unwrap_or(0);
    if hour > 23 || minute > 59 {
        return Err("timer hour or minute is out of range".into());
    }
    let now = chrono::Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let reached = now.hour() > hour || (now.hour() == hour && now.minute() >= minute);
    if state.last_key == today {
        return Ok(false);
    }
    if !state.armed {
        state.armed = true;
        if reached {
            state.last_key = today;
        }
        return Ok(false);
    }
    if !reached {
        return Ok(false);
    }
    state.last_key = today;
    Ok(true)
}

fn github_pr_body(sub: &Subscription, state: &mut WakeState) -> Result<Option<String>, String> {
    let Some(pr) = sub.pr else {
        return Err("github_pr needs a pr number".into());
    };
    if !valid_repo(&sub.repo) {
        return Err("github_pr repo must be owner/name".into());
    }
    let value = gh_json(&[
        "pr",
        "view",
        &pr.to_string(),
        "--repo",
        &sub.repo,
        "--json",
        "statusCheckRollup,headRefOid,title,url",
    ])?;
    let sha = value
        .get("headRefOid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if sha.is_empty() {
        return Ok(None);
    }
    let key = format!("{sha}:red");
    if state.last_key == key {
        return Ok(None);
    }
    let failed = failed_checks(value.get("statusCheckRollup"));
    if failed.is_empty() {
        return Ok(None);
    }
    state.last_key = key;
    let title = value.get("title").and_then(Value::as_str).unwrap_or("");
    let url = value.get("url").and_then(Value::as_str).unwrap_or("");
    Ok(Some(format!(
        "Pull request {pr} on {} went red.\n{title}\n{}\n{url}\n{}",
        sub.repo,
        failed.join("\n"),
        sub.prompt.trim()
    )))
}

fn github_ci_body(sub: &Subscription, state: &mut WakeState) -> Result<Option<String>, String> {
    if !valid_repo(&sub.repo) {
        return Err("github_ci repo must be owner/name".into());
    }
    if !valid_branch(&sub.branch) {
        return Err("github_ci needs a branch".into());
    }
    let value = gh_json(&[
        "run",
        "list",
        "--repo",
        &sub.repo,
        "--branch",
        &sub.branch,
        "--limit",
        "10",
        "--json",
        "databaseId,conclusion,displayTitle,url,status",
    ])?;
    let Some(runs) = value.as_array() else {
        return Ok(None);
    };
    for run in runs {
        let conclusion = run
            .get("conclusion")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !conclusion.eq_ignore_ascii_case("failure") {
            continue;
        }
        let id = run.get("databaseId").and_then(Value::as_u64).unwrap_or(0);
        let key = format!("run:{id}");
        if state.last_key == key {
            return Ok(None);
        }
        state.last_key = key;
        let title = run
            .get("displayTitle")
            .and_then(Value::as_str)
            .unwrap_or("workflow");
        let url = run.get("url").and_then(Value::as_str).unwrap_or("");
        return Ok(Some(format!(
            "CI on {} branch {} went red.\n{title}\n{url}\n{}",
            sub.repo,
            sub.branch,
            sub.prompt.trim()
        )));
    }
    Ok(None)
}

fn slack_body(sub: &Subscription, _state: &mut WakeState) -> Result<Option<String>, String> {
    if sub.channel.trim().is_empty() {
        return Err("slack needs a channel".into());
    }
    if !slack_installed() {
        tracing::debug!("slack subscription is waiting for fx slack install");
        return Ok(None);
    }
    // fx slack install stores a workspace token. It does not hand this process
    // a message stream. The file is valid; a wake needs that stream.
    tracing::debug!(channel = %sub.channel, "slack subscription is installed and has no message stream yet");
    Ok(None)
}

fn slack_installed() -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    Path::new(&home)
        .join(".fx")
        .join("slack")
        .join("installation.json")
        .is_file()
}

fn failed_checks(rollup: Option<&Value>) -> Vec<String> {
    let Some(checks) = rollup.and_then(Value::as_array) else {
        return Vec::new();
    };
    checks
        .iter()
        .filter_map(|check| {
            let conclusion = check
                .get("conclusion")
                .and_then(Value::as_str)
                .or_else(|| check.get("state").and_then(Value::as_str))
                .unwrap_or("");
            if !conclusion.eq_ignore_ascii_case("failure") {
                return None;
            }
            let name = check
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| check.get("context").and_then(Value::as_str))
                .unwrap_or("check");
            Some(format!("{name}: failure"))
        })
        .collect()
}

fn valid_repo(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && !name.is_empty()
        && !repo.contains("//")
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

fn valid_branch(branch: &str) -> bool {
    !branch.is_empty()
        && branch.len() < 200
        && !branch.starts_with('-')
        && branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

fn gh_json(args: &[&str]) -> Result<Value, String> {
    let mut command = Command::new("gh");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command.output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh {} failed: {}", args.join(" "), detail.trim()));
    }
    serde_json::from_slice(&output.stdout).map_err(|err| err.to_string())
}

fn queue_wake(
    host: &DocHost,
    sessions: &SessionsEngine,
    chat_id: &str,
    cwd: &str,
    body: &str,
) -> Result<(), String> {
    let prompt = format!("{}{body}", zeron_proto::ARBOS_WAKE_MARK);
    let queued = if sessions.is_working(chat_id) {
        host.queue_command(
            chat_id,
            SessionCommandPayload::Steer {
                prompt,
                message_id: Some(crate::new_id()),
            },
        )
    } else {
        let request = RunRequest {
            prompt,
            harness: Some(HarnessId::Fx),
            model: Some("glm-5.3".into()),
            reasoning: None,
            model_options: serde_json::Map::new(),
            cwd: cwd.to_owned(),
            sandbox: SandboxLevel::DangerFullAccess,
            auto_approve: true,
            resume: None,
            attachments: Vec::new(),
            worktree: None,
            mcp: None,
        };
        host.queue_command(
            chat_id,
            SessionCommandPayload::Run {
                request,
                message_id: crate::new_id(),
            },
        )
    };
    queued.map(|_| ()).map_err(|err| err.to_string())
}

fn state_path(cwd: &str) -> PathBuf {
    Path::new(cwd)
        .join(".arbos")
        .join("runtime")
        .join("wakes.json")
}

fn read_state(cwd: &str) -> BTreeMap<String, WakeState> {
    let Ok(text) = fs::read_to_string(state_path(cwd)) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_state(cwd: &str, state: &BTreeMap<String, WakeState>) -> Result<(), String> {
    let path = state_path(cwd);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let text = serde_json::to_string_pretty(state).map_err(|err| err.to_string())?;
    fs::write(path, text).map_err(|err| err.to_string())
}
