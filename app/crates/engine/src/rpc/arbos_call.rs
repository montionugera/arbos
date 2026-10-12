//! `ArbosCall` — the `arbos` MCP tools. The engine owns every write under
//! `.arbos/` and every worker process.

use std::path::Path;

use serde_json::{Value, json};
use zeron_doc::SessionCommandPayload;
use zeron_proto::{ChatConfig, HarnessId, RunRequest, SandboxLevel};

use super::EngineRpc;
use crate::arbos_goal;
use crate::arbos_layout::{self, AgentView};
use crate::new_id;
use crate::{arbos_live, arbos_wakes};

impl EngineRpc {
    pub(super) fn arbos_call(&self, params: Value) -> Result<Value, String> {
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
            "spawn" => self.spawn_worker(project_path, &params),
            "send" => self.send_worker(project_path, &params),
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
                self.require_coordinator(project_path, &params)?;
                arbos_goal::complete_goal(project_path)?;
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
            "seen" => {
                arbos_live::mark_seen(project_path)?;
                Ok(live_row(project_path))
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
        let text = text_field(params, "text");
        let rename = text_field(params, "rename");
        let model = text_field(params, "model");
        if text.is_none() && rename.is_none() && model.is_none() {
            return Err("the message is empty".into());
        }
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

/// Settings → Keys. Writes the fx connection into the main agent's home and
/// the secret into the keychain or the mode 600 file. The secret is not
/// copied into the connection file or the reply.
fn save_byok(project: &Path, params: &Value) -> Result<Value, String> {
    let provider = text_field(params, "provider").ok_or_else(|| "name the provider".to_string())?;
    let spec = byok_spec(&provider).ok_or_else(|| "unknown provider".to_string())?;
    let secret = params
        .get("secret")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
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
