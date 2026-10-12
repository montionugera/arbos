//! `ArbosCall` — the `arbos` MCP tools. The engine owns every write under
//! `.arbos/` and every worker process.

use std::path::Path;

use serde_json::{Value, json};
use zeron_doc::SessionCommandPayload;
use zeron_proto::{ChatConfig, HarnessId, RunRequest, SandboxLevel};

use super::EngineRpc;
use crate::arbos_goal::{self, GoalRecord};
use crate::arbos_layout::{self, AgentView};
use crate::{new_id, now_ms};
use crate::{arbos_context, arbos_live, arbos_wakes};

impl EngineRpc {
    pub(super) async fn arbos_call(&self, params: Value) -> Result<Value, String> {
        let op = params
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if op == "projects" {
            return Ok(json!({ "projects": crate::arbos_remote::phone_projects() }));
        }
        let project = params
            .get("project")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| "project path is missing".to_string())?;
        let project_path = Path::new(project);
        if !project_path.join(".arbos").join("project.toml").is_file() {
            return Err("that folder is not an Arbos project".into());
        }
        match op.as_str() {
            "spawn" => {
                self.check_requested_model(&params).await?;
                self.spawn_worker(project_path, &params)
            }
            "send" => {
                self.check_requested_model(&params).await?;
                self.send_worker(project_path, &params)
            }
            "list" => Ok(json!({ "agents": agent_rows(&arbos_layout::agents(project_path)) })),
            "result" => {
                let name = text_field(&params, "name").ok_or_else(|| "name the worker".to_string())?;
                let agent = arbos_layout::worker_result(project_path, &name)?;
                Ok(agent_row(&agent))
            }
            "transcript" => {
                let name = text_field(&params, "name").ok_or_else(|| "name the worker".to_string())?;
                let tail = params
                    .get("tail")
                    .and_then(Value::as_u64)
                    .map(|tail| usize::try_from(tail).unwrap_or(usize::MAX));
                let view = arbos_layout::worker_transcript(project_path, &name, tail)?;
                serde_json::to_value(view).map_err(|err| err.to_string())
            }
            "read" => {
                let path = context_path(&params)?;
                let text = arbos_layout::read_context(project_path, &path)?;
                Ok(json!({ "path": path, "text": text }))
            }
            "write" => {
                let path = context_path(&params)?;
                let text = params
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "text is missing".to_string())?;
                match origin_worker(project_path, &params) {
                    Some(worker) => arbos_layout::write_worker_context(project_path, &worker, &path, text)?,
                    None => arbos_layout::write_context(project_path, &path, text)?,
                }
                Ok(json!({ "path": path, "bytes": text.len() }))
            }
            "byok" => save_byok(project_path, &params),
            "goal_show" => goal_row_json(project_path),
            "goal_schedule" => {
                self.require_coordinator(project_path, &params)?;
                let seconds = params
                    .get("seconds")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "seconds is missing".to_string())?;
                arbos_goal::schedule_goal(project_path, seconds)?;
                goal_row_json(project_path)
            }
            "goal_pause" => {
                self.require_coordinator(project_path, &params)?;
                arbos_goal::pause_goal(project_path)?;
                goal_row_json(project_path)
            }
            "goal_resume" => {
                self.require_coordinator(project_path, &params)?;
                arbos_goal::resume_goal(project_path)?;
                goal_row_json(project_path)
            }
            "goal_complete" => {
                if let Err(err) = complete_goal(project_path, &params, |params| {
                    self.require_coordinator(project_path, params)
                }) {
                    if err.contains("still open") {
                        self.note_goal_still_open(project_path);
                    }
                    return Err(err);
                }
                goal_row_json(project_path)
            }
            "goal_clear" => {
                self.require_coordinator(project_path, &params)?;
                arbos_goal::clear_goal(project_path)?;
                Ok(json!({ "goal": Value::Null }))
            }
            "preferences_write" => {
                self.require_coordinator(project_path, &params)?;
                let text = text_field(&params, "text").ok_or_else(|| "the preference is empty".to_string())?;
                let line = arbos_layout::append_user_preference(&text)?;
                Ok(json!({ "line": line }))
            }
            "step" => {
                self.require_coordinator(project_path, &params)?;
                let text = text_field(&params, "text").ok_or_else(|| "the step is empty".to_string())?;
                let step = arbos_live::set_step(project_path, &text)?;
                Ok(json!({ "step": step }))
            }
            "live" => Ok(live_row(project_path)),
            "context_usage" => {
                let main = arbos_layout::main_chat_id(project_path);
                let window = self
                    .doc_host
                    .open(&main)
                    .ok()
                    .and_then(|handle| handle.doc().context_usage())
                    .and_then(|usage| usage.window);
                let view = arbos_context::usage(project_path, window);
                serde_json::to_value(view).map_err(|err| err.to_string())
            }
            "seen" => {
                arbos_live::mark_seen(project_path)?;
                Ok(live_row(project_path))
            }
            "subscriptions_add" => {
                let new: arbos_wakes::NewSubscription = serde_json::from_value(params.clone())
                    .map_err(|_| "the subscription fields were not read".to_string())?;
                let subscription = arbos_wakes::add_subscription(project_path, &new)?;
                Ok(json!({
                    "subscription": subscription,
                    "subscriptions": arbos_wakes::subscriptions(project_path),
                }))
            }
            "subscriptions_list" => Ok(json!({
                "subscriptions": arbos_wakes::subscriptions(project_path),
            })),
            "subscriptions_remove" => {
                let id = text_field(&params, "id").ok_or_else(|| "name the subscription".to_string())?;
                arbos_wakes::remove_subscription(project_path, &id)?;
                Ok(json!({
                    "removed": id,
                    "subscriptions": arbos_wakes::subscriptions(project_path),
                }))
            }
            _ => Err("unknown arbos op".into()),
        }
    }

    /// A `model` in a spawn or send must be one the provider offers. A call
    /// without a model is not checked; spawn keeps its default.
    async fn check_requested_model(&self, params: &Value) -> Result<(), String> {
        if text_field(params, "model").is_none() {
            return Ok(());
        }
        let offered = self.offered_worker_models().await?;
        check_params_model(params, &offered)
    }

    /// The model ids the worker harness offers right now, or the last list it
    /// fetched. A worker's model is checked against this before it is used.
    async fn offered_worker_models(&self) -> Result<Vec<String>, String> {
        let lease = std::sync::Arc::new(self.registry.execution_lease(HarnessId::Fx).await);
        let harness = self
            .registry
            .resolve(HarnessId::Fx)
            .map_err(|err| err.to_string())?;
        crate::model_catalogs::fetched_model_ids(self.repos.data_dir(), harness, Some(lease))
            .await
            .map_err(|err| format!("{err}, so the model was not changed; try again later"))
    }

    fn require_coordinator(&self, project: &Path, params: &Value) -> Result<(), String> {
        let origin = text_field(params, "originChatId")
            .ok_or_else(|| "origin chat is missing".to_string())?;
        let agent = arbos_layout::agents(project)
            .into_iter()
            .find(|agent| agent.chat_id == origin)
            .ok_or_else(|| "origin chat is not in this project".to_string())?;
        if agent.role != "coordinator" {
            return Err("workers cannot spawn or message agents".into());
        }
        Ok(())
    }

    /// The thread line a person sees when `goal.complete` is refused. Same
    /// path as a Worked line, so the 80-character plan filter does not hide it.
    fn note_goal_still_open(&self, project: &Path) {
        let Some(line) = arbos_goal::still_open_line(&arbos_goal::open_steps(project)) else {
            return;
        };
        let chat = arbos_layout::main_chat_id(project);
        let Ok(handle) = self.doc_host.open(&chat) else {
            return;
        };
        let _ = handle.write_system_message(&format!("goal-open-{}", new_id()), &line, now_ms());
    }

    fn spawn_worker(&self, project: &Path, params: &Value) -> Result<Value, String> {
        self.require_coordinator(project, params)?;
        if params
            .get("worktree")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err("worktree workers are not available yet".into());
        }
        let brief = text_field(params, "brief").ok_or_else(|| "the worker needs a brief".to_string())?;
        let name = text_field(params, "name").unwrap_or_default();
        let model = text_field(params, "model").unwrap_or_else(|| "glm-5.3".into());
        let machine = params.get("machine").and_then(Value::as_str);
        let readonly = params
            .get("readonly")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let requested = machine.unwrap_or("local").trim().to_owned();
        let worker = arbos_layout::reserve_worker(project, &name, &brief, &model, Some(requested.as_str()))?;
        if crate::arbos_remote::is_remote_machine(&requested) {
            if crate::arbos_remote::owns_project(project) {
                crate::arbos_remote::enqueue_local(&requested, project, &worker.id, &brief)?;
            } else {
                crate::arbos_remote::forward_spawn(project, &requested, &worker.id, &brief)?;
            }
            return Ok(json!({
                "name": worker.id,
                "status": "working",
                "machine": requested,
            }));
        }
        let origin = text_field(params, "originChatId").unwrap_or_default();
        let chat = self
            .workspace
            .chat(&origin)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "origin chat is missing".to_string())?;
        let space = chat
            .space_id
            .clone()
            .ok_or_else(|| "origin chat has no project".to_string())?;
        let cwd = chat.cwd.clone().unwrap_or_else(|| project.display().to_string());
        let chat_id = format!("{space}-w-{}", worker.id);
        let config = ChatConfig {
            harness: HarnessId::Fx,
            model: Some(worker_model(&model)),
            reasoning: None,
            model_options: serde_json::Map::new(),
            sandbox: if readonly {
                SandboxLevel::ReadOnly
            } else {
                SandboxLevel::DangerFullAccess
            },
        };
        if self
            .workspace
            .chat(&chat_id)
            .map_err(|err| err.to_string())?
            .is_none()
        {
            self.workspace
                .create_chat(
                    &chat_id,
                    Some(&space),
                    Some(&chat.device_id),
                    Some(config.clone()),
                    Some(cwd.clone()),
                )
                .map_err(|err| err.to_string())?;
        }
        self.workspace
            .rename_chat(&chat_id, &worker.id)
            .map_err(|err| err.to_string())?;
        self.workspace
            .set_chat_config(&chat_id, &config)
            .map_err(|err| err.to_string())?;
        arbos_layout::bind_worker_chat(project, &worker.id, &chat_id)?;
        self.queue_prompt(&chat_id, &cwd, &brief, &config)?;
        Ok(json!({
            "name": worker.id,
            "chatId": chat_id,
            "status": "working",
            "machine": "local",
        }))
    }

    /// `agents.send { name, text?, rename?, model? }`. A rename changes the
    /// tab label at once. A model applies from the worker's next turn. Text
    /// steers a running turn, else it is queued as the next prompt.
    fn send_worker(&self, project: &Path, params: &Value) -> Result<Value, String> {
        self.require_coordinator(project, params)?;
        let name = text_field(params, "name").ok_or_else(|| "name the worker".to_string())?;
        let SendFields {
            text,
            rename,
            model,
        } = send_fields(params)?;
        let mut worker = arbos_layout::worker_result(project, &name)?;
        if let Some(rename) = rename.as_deref() {
            worker = arbos_layout::rename_worker(project, &worker.id, rename)?;
            if !worker.chat_id.is_empty() {
                self.workspace
                    .rename_chat(&worker.chat_id, &worker.name)
                    .map_err(|err| err.to_string())?;
            }
        }
        if let Some(model) = model.as_deref() {
            worker = arbos_layout::set_worker_model(project, &worker.id, model)?;
            if !worker.chat_id.is_empty()
                && let Some(chat) = self.workspace.chat(&worker.chat_id).map_err(|err| err.to_string())?
                && let Some(mut config) = chat.config
            {
                config.model = Some(worker.model.clone());
                self.workspace
                    .set_chat_config(&worker.chat_id, &config)
                    .map_err(|err| err.to_string())?;
            }
        }
        let Some(text) = text else {
            return Ok(json!({
                "name": worker.id,
                "displayName": worker.name,
                "model": worker.model,
                "steered": false,
            }));
        };
        if worker.chat_id.is_empty() {
            return Err("that worker has no chat".into());
        }
        let cwd = self
            .workspace
            .chat(&worker.chat_id)
            .map_err(|err| err.to_string())?
            .and_then(|chat| chat.cwd)
            .unwrap_or_else(|| project.display().to_string());
        let working = self.sessions.is_working(&worker.chat_id);
        if working {
            self.doc_host
                .queue_command(
                    &worker.chat_id,
                    SessionCommandPayload::Steer {
                        prompt: text,
                        message_id: Some(new_id()),
                    },
                )
                .map_err(|err| err.to_string())?;
        } else {
            let config = ChatConfig {
                harness: HarnessId::Fx,
                model: Some(worker_model(&worker.model)),
                reasoning: None,
                model_options: serde_json::Map::new(),
                sandbox: SandboxLevel::DangerFullAccess,
            };
            self.queue_prompt(&worker.chat_id, &cwd, &text, &config)?;
            arbos_layout::restart_worker(project, &worker.id)?;
        }
        Ok(json!({
            "name": worker.id,
            "displayName": worker.name,
            "model": worker.model,
            "steered": working,
        }))
    }

    fn queue_prompt(
        &self,
        chat_id: &str,
        cwd: &str,
        prompt: &str,
        config: &ChatConfig,
    ) -> Result<(), String> {
        self.doc_host
            .queue_command(
                chat_id,
                SessionCommandPayload::Run {
                    request: RunRequest {
                        prompt: prompt.to_owned(),
                        harness: Some(config.harness),
                        model: config.model.clone(),
                        reasoning: config.reasoning,
                        model_options: serde_json::Map::new(),
                        cwd: cwd.to_owned(),
                        sandbox: config.sandbox,
                        auto_approve: true,
                        resume: None,
                        attachments: Vec::new(),
                        worktree: None,
                        mcp: None,
                    },
                    message_id: new_id(),
                },
            )
            .map_err(|err| err.to_string())?;
        Ok(())
    }
}

/// `goal.complete`. Main sends a `reason` and the checklist is checked. The
/// person's UI sends `force: true` instead; a call that comes from a chat
/// cannot set it.
fn complete_goal(
    project: &Path,
    params: &Value,
    require_coordinator: impl FnOnce(&Value) -> Result<(), String>,
) -> Result<GoalRecord, String> {
    let force = params.get("force").and_then(Value::as_bool).unwrap_or(false);
    if force {
        if text_field(params, "originChatId").is_some() {
            return Err("only a person can complete a goal without evidence; give a reason instead".into());
        }
        return arbos_goal::force_complete_goal(project);
    }
    require_coordinator(params)?;
    let reason = text_field(params, "reason").unwrap_or_default();
    arbos_goal::complete_goal(project, &reason)
}

/// Settings → Keys. Writes the fx connection into the main agent's home and
/// the secret into the keychain or the mode 600 file. The secret is not
/// copied into the connection file or the reply. A tool key (GitHub,
/// 1Password) goes to the store only; it has no fx connection.
fn save_byok(project: &Path, params: &Value) -> Result<Value, String> {
    let provider = text_field(params, "provider").ok_or_else(|| "name the provider".to_string())?;
    let secret = params
        .get("secret")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if let Some((name, var_name)) = zeron_harness::secrets::TOOLS
        .iter()
        .find(|(name, _)| *name == provider)
    {
        return save_tool_key(name, var_name, secret);
    }
    let spec = byok_spec(&provider).ok_or_else(|| "unknown provider".to_string())?;
    let base_url = text_field(params, "baseUrl").unwrap_or_else(|| spec.base_url.to_owned());
    if spec.needs_key && secret.is_empty() {
        return Err("that provider needs a key".into());
    }
    if !secret.is_empty() {
        zeron_harness::secrets::write(spec.secret_name, secret)?;
        if let Some(var_name) = zeron_harness::secrets::PROVIDERS
            .iter()
            .find(|(name, _)| *name == spec.secret_name)
            .map(|(_, var_name)| *var_name)
        {
            // The next fx child inherits this. The value is not logged.
            unsafe { std::env::set_var(var_name, secret) };
        }
    }
    write_fx_connection(project, &spec, &base_url, secret)?;
    Ok(json!({
        "ok": true,
        "provider": spec.id,
        "stored": !secret.is_empty(),
        "place": zeron_harness::secrets::place(),
    }))
}

/// A worker tool token. The fx child reads it from its environment; `gh` and
/// `op` read the variable themselves. The value is not logged.
fn save_tool_key(
    name: &'static str,
    var_name: &'static str,
    secret: &str,
) -> Result<Value, String> {
    if secret.is_empty() {
        return Err("that tool needs a token".into());
    }
    zeron_harness::secrets::write(name, secret)?;
    // The next fx child inherits this. The value is not logged.
    unsafe { std::env::set_var(var_name, secret) };
    Ok(json!({
        "ok": true,
        "provider": name,
        "stored": true,
        "place": zeron_harness::secrets::place(),
    }))
}

struct ByokSpec {
    id: &'static str,
    secret_name: &'static str,
    env_name: &'static str,
    base_url: &'static str,
    needs_key: bool,
    /// Built-in fx provider name. Custom connections use `providers` instead.
    builtin: Option<&'static str>,
}

fn byok_spec(provider: &str) -> Option<ByokSpec> {
    Some(match provider {
        "engy" => ByokSpec {
            id: "engy",
            secret_name: "engy",
            env_name: "ENGY_API_KEY",
            base_url: "https://api.engy.ai/v1",
            needs_key: true,
            builtin: Some("engy"),
        },
        "ai-gateway" => ByokSpec {
            id: "ai-gateway",
            secret_name: "ai-gateway",
            env_name: "AI_GATEWAY_API_KEY",
            base_url: "https://ai-gateway.vercel.sh/v1",
            needs_key: true,
            builtin: None,
        },
        "openrouter" => ByokSpec {
            id: "openrouter",
            secret_name: "openrouter",
            env_name: "OPENROUTER_API_KEY",
            base_url: "https://openrouter.ai/api/v1",
            needs_key: true,
            builtin: None,
        },
        "ollama" => ByokSpec {
            id: "ollama",
            secret_name: "ollama",
            env_name: "OLLAMA_API_KEY",
            base_url: "http://127.0.0.1:11434/v1",
            needs_key: false,
            builtin: None,
        },
        "codex" => ByokSpec {
            id: "codex",
            secret_name: "codex",
            env_name: "CODEX_API_KEY",
            base_url: "",
            needs_key: false,
            builtin: Some("codex"),
        },
        "grok" => ByokSpec {
            id: "grok",
            secret_name: "grok",
            env_name: "GROK_API_KEY",
            base_url: "",
            needs_key: false,
            builtin: Some("grok"),
        },
        _ => return None,
    })
}

fn write_fx_connection(
    project: &Path,
    spec: &ByokSpec,
    base_url: &str,
    secret: &str,
) -> Result<(), String> {
    let path = project
        .join(".arbos")
        .join("agents")
        .join("main")
        .join("fx")
        .join("settings.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let mut root = match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => {
            serde_json::from_str::<Value>(&text).map_err(|_| "fx settings could not be read".to_string())?
        }
        _ => json!({}),
    };
    if !root.is_object() {
        root = json!({});
    }
    if let Some(builtin) = spec.builtin {
        root["provider"] = json!(builtin);
    } else {
        if !valid_base_url(base_url) {
            return Err("the base URL must be http or https".into());
        }
        let auth = if spec.id == "ollama" && secret.is_empty() {
            json!({ "type": "none" })
        } else {
            json!({ "type": "bearer", "env": spec.env_name })
        };
        root["providers"][spec.id] = json!({
            "protocol": "openai-chat-completions",
            "base_url": base_url.trim_end_matches('/'),
            "auth": auth,
        });
        root["provider"] = json!(spec.id);
    }
    let text = serde_json::to_string_pretty(&root).map_err(|err| err.to_string())?;
    if !secret.is_empty() && text.contains(secret) {
        return Err("the connection file would have contained the key".into());
    }
    std::fs::write(&path, text).map_err(|err| err.to_string())?;
    Ok(())
}

fn valid_base_url(url: &str) -> bool {
    let url = url.trim();
    (url.starts_with("https://") || url.starts_with("http://"))
        && !url.contains(' ')
        && !url.contains('\n')
        && url.len() < 300
}

/// The worker id behind `originChatId`, when the caller is a worker. Main and
/// plain chats return `None`, so their writes skip the report check.
fn origin_worker(project: &Path, params: &Value) -> Option<String> {
    let origin = text_field(params, "originChatId")?;
    arbos_layout::agents(project)
        .into_iter()
        .find(|agent| agent.chat_id == origin && agent.role == "worker")
        .map(|agent| agent.id)
}

/// The `model` of a spawn or send, checked against the models on offer. No
/// model means nothing to check.
fn check_params_model(params: &Value, offered: &[String]) -> Result<(), String> {
    match text_field(params, "model") {
        Some(model) => check_worker_model(&model, offered),
        None => Ok(()),
    }
}

/// A worker's model must be one the provider offers. The refusal names the
/// models on offer so Main can pick from them.
fn check_worker_model(requested: &str, offered: &[String]) -> Result<(), String> {
    let requested = requested.trim();
    if offered.iter().any(|id| id == requested) {
        return Ok(());
    }
    Err(format!(
        "the model {requested} is not on offer; pick one of: {}",
        offered.join(", ")
    ))
}

/// The optional fields of `agents.send`. At least one is present.
#[derive(Debug)]
struct SendFields {
    text: Option<String>,
    rename: Option<String>,
    model: Option<String>,
}

/// Reads `text`, `rename`, and `model` from an `agents.send` call. A model
/// tends to write the message under `message` or `prompt`; both are read
/// as `text`, and `text` wins when more than one is present.
fn send_fields(params: &Value) -> Result<SendFields, String> {
    let text = text_field(params, "text")
        .or_else(|| text_field(params, "message"))
        .or_else(|| text_field(params, "prompt"));
    let rename = text_field(params, "rename");
    let model = text_field(params, "model");
    if text.is_none() && rename.is_none() && model.is_none() {
        return Err("agents.send needs text: the message for the worker".into());
    }
    Ok(SendFields {
        text,
        rename,
        model,
    })
}

fn worker_model(model: &str) -> String {
    let model = model.trim();
    if model.is_empty() {
        "glm-5.3".into()
    } else {
        model.to_owned()
    }
}

fn text_field(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn context_path(params: &Value) -> Result<String, String> {
    let raw = text_field(params, "path").ok_or_else(|| "name the context file".to_string())?;
    let raw = raw.trim_start_matches('/');
    let relative = raw
        .strip_prefix(".arbos/context/")
        .or_else(|| raw.strip_prefix("context/"))
        .unwrap_or(raw);
    if relative.is_empty() {
        return Err("name the context file".into());
    }
    Ok(relative.to_owned())
}

fn goal_row_json(project: &Path) -> Result<Value, String> {
    let row = arbos_goal::goal_row(project);
    Ok(json!({ "goal": row }))
}

fn live_row(project: &Path) -> Value {
    let live = arbos_live::live(project);
    json!({
        "unread": live.unread,
        "step": live.step,
        "stepSource": live.step_source,
    })
}

fn agent_rows(agents: &[AgentView]) -> Vec<Value> {
    agents.iter().map(agent_row).collect()
}

fn agent_row(agent: &AgentView) -> Value {
    json!({
        "name": agent.id,
        "displayName": agent.name,
        "model": agent.model,
        "role": agent.role,
        "status": agent.status,
        "chatId": agent.chat_id,
        "brief": agent.brief,
        "result": agent.result,
        "resultPointer": format!("agents/{}/agent.toml", agent.id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arbos_goal::GoalState;

    fn project_with_open_step() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let context = dir.path().join(".arbos").join("context");
        std::fs::create_dir_all(&context).unwrap();
        std::fs::write(
            context.join("status.md"),
            "# Status\n\n- [ ] Oak is ready.\n",
        )
        .unwrap();
        arbos_goal::start_goal(dir.path(), "Ship the thing").unwrap();
        dir
    }

    fn main_allowed(_: &Value) -> Result<(), String> {
        Ok(())
    }

    #[test]
    fn main_cannot_set_force() {
        let dir = project_with_open_step();
        let params = json!({ "force": true, "originChatId": "chat-main" });
        let err = complete_goal(dir.path(), &params, main_allowed).unwrap_err();
        assert!(err.contains("only a person"), "{err}");
        assert_eq!(
            arbos_goal::read_goal(dir.path()).unwrap().state,
            GoalState::Active
        );
    }

    #[test]
    fn the_person_forces_and_main_is_checked() {
        let dir = project_with_open_step();
        let params =
            json!({ "originChatId": "chat-main", "reason": "Oak reported and the gate is green." });
        let err = complete_goal(dir.path(), &params, main_allowed).unwrap_err();
        assert!(err.contains("- Oak is ready."), "{err}");
        let refused =
            complete_goal(dir.path(), &params, |_| Err("workers cannot".into())).unwrap_err();
        assert_eq!(refused, "workers cannot");
        let done = complete_goal(dir.path(), &json!({ "force": true }), |_| {
            panic!("a forced call does not look for Main")
        })
        .unwrap();
        assert_eq!(done.state, GoalState::Complete);
    }

    fn offered() -> Vec<String> {
        ["glm-5.3", "glm-5.2", "kimi-k2"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_model_on_offer_is_accepted() {
        assert_eq!(check_worker_model("glm-5.2", &offered()), Ok(()));
        assert_eq!(check_worker_model("  kimi-k2 ", &offered()), Ok(()));
    }

    #[test]
    fn a_model_not_on_offer_is_refused_with_the_list() {
        let err = check_worker_model("glm-4.7", &offered()).unwrap_err();
        assert_eq!(
            err,
            "the model glm-4.7 is not on offer; pick one of: glm-5.3, glm-5.2, kimi-k2"
        );
        // Close is not the same model.
        assert!(check_worker_model("glm-5", &offered()).is_err());
        assert!(check_worker_model("GLM-5.3", &offered()).is_err());
    }

    #[test]
    fn a_spawn_with_a_model_not_on_offer_is_refused_with_the_list() {
        let spawn = json!({ "op": "spawn", "brief": "Write the note", "model": "glm-4.7" });
        let err = check_params_model(&spawn, &offered()).unwrap_err();
        assert_eq!(
            err,
            "the model glm-4.7 is not on offer; pick one of: glm-5.3, glm-5.2, kimi-k2"
        );
        let known = json!({ "op": "spawn", "brief": "Write the note", "model": "glm-5.2" });
        assert_eq!(check_params_model(&known, &offered()), Ok(()));
        // No model: the spawn keeps its default and nothing is checked.
        let default = json!({ "op": "spawn", "brief": "Write the note" });
        assert_eq!(check_params_model(&default, &offered()), Ok(()));
        let blank = json!({ "op": "spawn", "brief": "Write the note", "model": "  " });
        assert_eq!(check_params_model(&blank, &offered()), Ok(()));
    }

    #[test]
    fn a_send_reads_message_as_text_and_the_empty_error_names_the_field() {
        let sent = json!({ "name": "oak", "message": " Restart on the new brief. " });
        let fields = send_fields(&sent).unwrap();
        assert_eq!(fields.text.as_deref(), Some("Restart on the new brief."));
        assert!(fields.rename.is_none() && fields.model.is_none());
        let prompted = json!({ "name": "oak", "prompt": "Try again." });
        assert_eq!(
            send_fields(&prompted).unwrap().text.as_deref(),
            Some("Try again.")
        );
        // text wins over its aliases.
        let both = json!({ "name": "oak", "text": "From text.", "message": "From message." });
        assert_eq!(
            send_fields(&both).unwrap().text.as_deref(),
            Some("From text.")
        );
        // A rename or a model alone is still a send.
        let renamed = json!({ "name": "oak", "rename": "Oak the tester" });
        assert!(send_fields(&renamed).unwrap().text.is_none());
        let empty = send_fields(&json!({ "name": "oak", "text": "  " })).unwrap_err();
        assert_eq!(empty, "agents.send needs text: the message for the worker");
    }

    /// Every regular file under `root`, recursively, whose bytes contain `needle`.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn files_containing(root: &Path, needle: &[u8]) -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if std::fs::read(&path)
                    .is_ok_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle))
                {
                    found.push(path);
                }
            }
        }
        found.sort();
        found
    }

    /// The keychain is not hermetic on macOS; the Linux file store is.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_saved_tool_token_is_not_in_the_reply_or_under_the_project() {
        let store = tempfile::tempdir().unwrap();
        let project = project_with_open_step();
        std::fs::create_dir_all(project.path().join(".arbos").join("agents").join("oak")).unwrap();
        // The lib tests share one process; the store directory is read only here.
        unsafe { std::env::set_var("ARBOS_SECRETS_DIR", store.path()) };
        for (provider, var_name, token) in [
            ("github", "GH_TOKEN", "test-token-github-0123456789abcdef"),
            (
                "onepassword",
                "OP_SERVICE_ACCOUNT_TOKEN",
                "test-token-onepassword-0123456789abcdef",
            ),
        ] {
            let before = std::env::var_os(var_name);
            let reply = save_byok(
                project.path(),
                &json!({ "provider": provider, "secret": token }),
            )
            .unwrap();
            // save_tool_key exports the token for the next fx child; another
            // test in this process may read that variable, so put it back now.
            unsafe {
                match &before {
                    Some(value) => std::env::set_var(var_name, value),
                    None => std::env::remove_var(var_name),
                }
            }
            let text = serde_json::to_string(&reply).unwrap();
            assert!(!text.contains(token), "{text}");
            assert_eq!(reply["ok"], true);
            assert_eq!(reply["provider"], provider);
            assert_eq!(reply["stored"], true);
            assert_eq!(reply["place"], "file");
            assert!(files_containing(project.path(), token.as_bytes()).is_empty());
            assert_eq!(
                files_containing(store.path(), token.as_bytes()),
                vec![store.path().join(provider)]
            );
            assert!(!store.path().starts_with(project.path()));
        }
        // A tool key has no fx connection, so Main's settings file stays absent.
        assert!(
            !project
                .path()
                .join(".arbos")
                .join("agents")
                .join("main")
                .join("fx")
                .join("settings.json")
                .exists()
        );
    }
}
