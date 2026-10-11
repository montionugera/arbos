//! One folder's `.arbos/` tree. The engine creates it, points each fx process
//! at `agents/<id>/fx`, and commits the tree after a turn. `runtime/` stays
//! out of that git repo.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

pub const COORDINATOR_PROMPT: &str = "\
You are the coordinator for this project. \
Plan the work and delegate it. Do not edit project code. \
Context files are yours to keep current. \
The arbos tools are mcp_arbos_agents_spawn, mcp_arbos_agents_send, mcp_arbos_agents_list, mcp_arbos_agents_result, mcp_arbos_context_read, and mcp_arbos_context_write. \
A tool works only after mcp_select_tool names it. The schema is ready on the next step, so call the tool then. \
To spawn one worker: mcp_select_tool {\"name\":\"mcp_arbos_agents_spawn\"}, then mcp_arbos_agents_spawn. \
Repeat that pair for every worker. Spawn returns at once. machine must be \"local\". \
Start every independent worker before you wait for any of them. \
When a worker finishes you see a Worked line. \
Then summarise in a few sentences and tick the matching line in context/status.md with mcp_arbos_context_write. \
Do not tick a line before that worker reports. \
Read `.arbos/context/GOALS.md` first. \
Write what you learn to `.arbos/context/`. \
Keep `status.md` current.";

pub const WORKER_PROMPT: &str = "\
You are a worker. Do the brief. Do not spawn agents and do not edit project code. \
Write the requested file with mcp_arbos_context_write. \
Before that call, call mcp_select_tool with name mcp_arbos_context_write. \
The path is relative to context, for example research/note.md. \
Then reply with that path and stop. \
Read `.arbos/context/GOALS.md` first. \
Write what you learn to `.arbos/context/`. \
Keep `status.md` current.";

pub const CHAT_PROMPT: &str = "\
Read `.arbos/context/GOALS.md` first. \
Write what you learn to `.arbos/context/`. \
Keep `status.md` current.";

const GITIGNORE: &str = "runtime/\n";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectFile {
    id: String,
    name: String,
    icon: String,
    home: String,
    model: String,
    created: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentFile {
    id: String,
    role: String,
    #[serde(default)]
    chat_id: String,
    model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    parent: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    brief: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    result: String,
}

/// One row of `agents/*/agent.toml` for the tab strip and the list tool.
#[derive(Debug, Clone)]
pub struct AgentView {
    pub id: String,
    pub role: String,
    pub chat_id: String,
    pub status: String,
    pub brief: String,
    pub result: String,
}

/// A worker turn just ended. `main_chat_id` is the coordinator to wake.
#[derive(Debug, Clone)]
pub struct WorkerDone {
    pub id: String,
    pub main_chat_id: String,
    pub result: String,
}

#[derive(Debug, Clone)]
pub struct TurnPrep {
    pub fx_home: String,
    pub system_prompt: String,
    pub agent_id: String,
    pub role: String,
}

fn layout_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn root_of(project: &Path) -> PathBuf {
    project.join(".arbos")
}

/// Create the layout if this folder does not have one yet. Existing files
/// stay as they are.
pub fn ensure(project: &Path, name: Option<&str>) -> Result<(), String> {
    let _guard = layout_lock();
    if !project.is_dir() {
        return Err("project folder is missing".into());
    }
    let root = root_of(project);
    fs::create_dir_all(root.join("context").join("research")).map_err(|e| e.to_string())?;
    fs::create_dir_all(root.join("subscriptions")).map_err(|e| e.to_string())?;
    fs::create_dir_all(root.join("agents").join("main").join("fx")).map_err(|e| e.to_string())?;
    fs::create_dir_all(root.join("runtime")).map_err(|e| e.to_string())?;
    write_if_missing(&root.join(".gitignore"), GITIGNORE)?;
    write_if_missing(
        &root.join("context").join("GOALS.md"),
        "# Goals\n\nStanding goals for this project.\n",
    )?;
    write_if_missing(
        &root.join("context").join("status.md"),
        "# Status\n\n- [ ] Open the project\n- [ ] Keep this checklist current\n",
    )?;
    write_if_missing(
        &root.join("context").join("preferences.md"),
        "# Preferences\n\nHow work should be done in this project.\n",
    )?;
    write_if_missing(&root.join("context").join("research").join(".gitkeep"), "")?;
    write_if_missing(&root.join("subscriptions").join(".gitkeep"), "")?;
    let project_file = root.join("project.toml");
    if !project_file.is_file() {
        let display = name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| folder_name(project));
        let file = ProjectFile {
            id: uuid::Uuid::new_v4().to_string(),
            name: display,
            icon: "repo".into(),
            home: "local".into(),
            model: "glm-5.3".into(),
            created: chrono::Utc::now().to_rfc3339(),
        };
        write_toml(&project_file, &file)?;
    }
    let main = root.join("agents").join("main").join("agent.toml");
    if !main.is_file() {
        write_toml(&main, &coordinator_agent())?;
    }
    if !root.join(".git").exists() {
        if !git(&root, &["init"]) {
            return Err("git init failed".into());
        }
        let _ = git(&root, &["add", "-A"]);
        if !git(&root, &["commit", "-m", "Create the project layout"]) {
            return Err("initial project commit failed".into());
        }
    }
    Ok(())
}

/// The icon string stored in `project.toml`: `emoji:…`, `image:…`, or `repo`.
pub fn icon_of(project: &Path) -> String {
    read_project(project)
        .map(|file| file.icon)
        .unwrap_or_else(|| "repo".into())
}

pub fn project_name(project: &Path) -> Option<String> {
    read_project(project).map(|file| file.name)
}

/// Set the rail icon. `emoji:` takes the glyph, `image:` takes a file on
/// disk, and `repo` uses the repository's own icon.
pub fn set_icon(project: &Path, icon: &str) -> Result<(), String> {
    let _guard = layout_lock();
    let icon = normalize_icon(icon)?;
    let path = root_of(project).join("project.toml");
    let mut file = read_project(project).ok_or_else(|| "project.toml is missing".to_string())?;
    file.icon = icon;
    write_toml(&path, &file)
}

/// Write one file under `.arbos/context/`. The relative path cannot leave
/// that directory.
pub fn write_context(project: &Path, relative: &str, text: &str) -> Result<(), String> {
    let _guard = layout_lock();
    let path = context_file(project, relative)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&path, text).map_err(|e| e.to_string())
}

pub fn read_context(project: &Path, relative: &str) -> Result<String, String> {
    let path = context_file(project, relative)?;
    fs::read_to_string(&path).map_err(|e| e.to_string())
}

/// Checkbox rows from `context/status.md`, in file order.
pub fn status_items(project: &Path) -> Vec<(bool, String)> {
    let Ok(text) = fs::read_to_string(root_of(project).join("context").join("status.md")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let rest = trimmed
                .strip_prefix("- [ ]")
                .map(|rest| (false, rest))
                .or_else(|| trimmed.strip_prefix("- [x]").map(|rest| (true, rest)))
                .or_else(|| trimmed.strip_prefix("- [X]").map(|rest| (true, rest)))?;
            Some((rest.0, rest.1.trim().to_string()))
        })
        .collect()
}

/// Bind this chat to an agent directory and return the fx home plus the
/// system prompt. Missing `.arbos/project.toml` means this cwd is not a project.
pub fn prepare_turn(project: &Path, chat_id: &str) -> Option<TurnPrep> {
    let _guard = layout_lock();
    let root = root_of(project);
    if !root.join("project.toml").is_file() {
        return None;
    }
    let agent = bind_agent(&root, chat_id).ok()?;
    let fx = root.join("agents").join(&agent.id).join("fx");
    fs::create_dir_all(&fx).ok()?;
    let system_prompt = match agent.role.as_str() {
        "coordinator" => COORDINATOR_PROMPT,
        "worker" => WORKER_PROMPT,
        _ => CHAT_PROMPT,
    };
    Some(TurnPrep {
        fx_home: fx.display().to_string(),
        system_prompt: system_prompt.to_owned(),
        agent_id: agent.id,
        role: agent.role,
    })
}

/// Stamp the coordinator agent with the chat created for Main.
pub fn claim_main(project: &Path, chat_id: &str) -> Result<(), String> {
    let _guard = layout_lock();
    let path = root_of(project).join("agents").join("main").join("agent.toml");
    let mut agent = read_agent(&path).unwrap_or_else(coordinator_agent);
    if agent.chat_id.is_empty() {
        agent.chat_id = chat_id.to_owned();
        write_toml(&path, &agent)?;
    }
    Ok(())
}

fn coordinator_agent() -> AgentFile {
    AgentFile {
        id: "main".into(),
        role: "coordinator".into(),
        chat_id: String::new(),
        model: "glm-5.3".into(),
        parent: String::new(),
        brief: String::new(),
        status: String::new(),
        result: String::new(),
    }
}

/// Agents in this project, main first, then the rest by id.
pub fn agents(project: &Path) -> Vec<AgentView> {
    let _guard = layout_lock();
    let mut rows: Vec<_> = list_agents(&root_of(project))
        .into_iter()
        .map(agent_view)
        .collect();
    rows.sort_by(|a, b| {
        let rank = |role: &str| match role {
            "coordinator" => 0,
            "worker" => 1,
            _ => 2,
        };
        rank(&a.role)
            .cmp(&rank(&b.role))
            .then(a.id.cmp(&b.id))
    });
    rows
}

/// Create or reuse `agents/<name>/` for a local worker. `machine` must be
/// empty or `local`. The chat id is filled in by [`bind_worker_chat`].
pub fn reserve_worker(
    project: &Path,
    name: &str,
    brief: &str,
    model: &str,
    machine: Option<&str>,
) -> Result<AgentView, String> {
    let _guard = layout_lock();
    let machine = machine.unwrap_or("local").trim();
    if machine != "local" {
        return Err("only local workers are available".into());
    }
    let brief = brief.trim();
    if brief.is_empty() {
        return Err("the worker needs a brief".into());
    }
    let id = worker_id(project, name)?;
    let root = root_of(project);
    let dir = root.join("agents").join(&id);
    fs::create_dir_all(dir.join("fx")).map_err(|e| e.to_string())?;
    let path = dir.join("agent.toml");
    let mut agent = read_agent(&path).unwrap_or(AgentFile {
        id: id.clone(),
        role: "worker".into(),
        chat_id: String::new(),
        model: model.to_owned(),
        parent: "main".into(),
        brief: String::new(),
        status: String::new(),
        result: String::new(),
    });
    if agent.role == "coordinator" {
        return Err("main is the coordinator".into());
    }
    agent.role = "worker".into();
    agent.parent = "main".into();
    agent.brief = brief.to_owned();
    agent.model = if model.trim().is_empty() {
        "glm-5.3".into()
    } else {
        model.trim().to_owned()
    };
    agent.status = "working".into();
    agent.result.clear();
    write_toml(&path, &agent)?;
    Ok(agent_view(agent))
}

/// Point a reserved worker at the chat that will run it.
pub fn bind_worker_chat(project: &Path, worker_id: &str, chat_id: &str) -> Result<(), String> {
    let _guard = layout_lock();
    let path = root_of(project)
        .join("agents")
        .join(worker_id)
        .join("agent.toml");
    let mut agent = read_agent(&path).ok_or_else(|| "worker is missing".to_string())?;
    agent.chat_id = chat_id.to_owned();
    write_toml(&path, &agent)
}

/// Record a worker turn that just ended. Returns the coordinator to wake.
/// A second call for the same turn returns nothing.
pub fn finish_worker(
    project: &Path,
    chat_id: &str,
    status: &str,
    result: &str,
) -> Option<WorkerDone> {
    let _guard = layout_lock();
    let root = root_of(project);
    let agents = list_agents(&root);
    let current = agents.iter().find(|agent| agent.chat_id == chat_id)?;
    if current.role != "worker" || current.status != "working" {
        return None;
    }
    let main_chat_id = agents
        .iter()
        .find(|agent| agent.id == "main")
        .map(|agent| agent.chat_id.clone())
        .filter(|id| !id.is_empty())?;
    let path = root.join("agents").join(&current.id).join("agent.toml");
    let mut agent = read_agent(&path)?;
    agent.status = status.to_owned();
    agent.result = clip_result(result);
    write_toml(&path, &agent).ok()?;
    Some(WorkerDone {
        id: agent.id,
        main_chat_id,
        result: agent.result,
    })
}

/// Last stored reply for `agents.result`.
pub fn worker_result(project: &Path, name: &str) -> Result<AgentView, String> {
    let _guard = layout_lock();
    let id = slug(name);
    if id.is_empty() {
        return Err("name the worker".into());
    }
    let path = root_of(project)
        .join("agents")
        .join(&id)
        .join("agent.toml");
    read_agent(&path)
        .map(agent_view)
        .ok_or_else(|| "no such worker".to_string())
}

fn agent_view(agent: AgentFile) -> AgentView {
    AgentView {
        id: agent.id,
        role: agent.role,
        chat_id: agent.chat_id,
        status: agent.status,
        brief: agent.brief,
        result: agent.result,
    }
}

fn worker_id(project: &Path, name: &str) -> Result<String, String> {
    let slug = slug(name);
    if !slug.is_empty() && slug != "main" {
        return Ok(slug);
    }
    let root = root_of(project);
    for n in 1..100 {
        let id = format!("worker-{n}");
        if !root.join("agents").join(&id).exists() {
            return Ok(id);
        }
    }
    Err("too many workers".into())
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    out.trim_matches('-').to_owned()
}

fn clip_result(text: &str) -> String {
    let text = text.trim();
    let mut end = 500.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut clipped = text[..end].to_owned();
    if text.len() > end {
        clipped.push('…');
    }
    clipped
}

/// Commit `.arbos` when a turn changed it. A clean tree is left alone.
pub fn commit_after_turn(project: &Path) {
    let _guard = layout_lock();
    let root = root_of(project);
    if !root.join(".git").exists() {
        return;
    }
    let _ = git(&root, &["add", "-A"]);
    if git_clean(&root) {
        return;
    }
    let _ = git(&root, &["commit", "-m", "Save the project after a turn"]);
}

fn bind_agent(root: &Path, chat_id: &str) -> Result<AgentFile, String> {
    let agents = list_agents(root);
    if let Some(agent) = agents.iter().find(|agent| agent.chat_id == chat_id) {
        return Ok(agent.clone());
    }
    if let Some(mut main) = agents.into_iter().find(|agent| agent.id == "main")
        && main.chat_id.is_empty()
    {
        main.chat_id = chat_id.to_owned();
        let path = root.join("agents").join("main").join("agent.toml");
        write_toml(&path, &main)?;
        return Ok(main);
    }
    let agents = list_agents(root);
    let mut n = 2;
    loop {
        let id = format!("chat-{n}");
        if !agents.iter().any(|agent| agent.id == id) {
            let agent = AgentFile {
                id: id.clone(),
                role: "chat".into(),
                chat_id: chat_id.to_owned(),
                model: "glm-5.3".into(),
                parent: String::new(),
                brief: String::new(),
                status: String::new(),
                result: String::new(),
            };
            let dir = root.join("agents").join(&id);
            fs::create_dir_all(dir.join("fx")).map_err(|e| e.to_string())?;
            write_toml(&dir.join("agent.toml"), &agent)?;
            return Ok(agent);
        }
        n += 1;
        if n > 99 {
            return Err("too many chats".into());
        }
    }
}

fn list_agents(root: &Path) -> Vec<AgentFile> {
    let Ok(entries) = fs::read_dir(root.join("agents")) else {
        return Vec::new();
    };
    let mut agents = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path().join("agent.toml");
        if let Some(agent) = read_agent(&path) {
            agents.push(agent);
        }
    }
    agents
}

fn read_agent(path: &Path) -> Option<AgentFile> {
    let text = fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

fn read_project(project: &Path) -> Option<ProjectFile> {
    let text = fs::read_to_string(root_of(project).join("project.toml")).ok()?;
    toml::from_str(&text).ok()
}

fn write_toml<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let text = toml::to_string_pretty(value).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| e.to_string())
}

fn write_if_missing(path: &Path, body: &str) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    fs::write(path, body).map_err(|e| e.to_string())
}

fn folder_name(project: &Path) -> String {
    project
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Project")
        .to_owned()
}

fn normalize_icon(icon: &str) -> Result<String, String> {
    let icon = icon.trim();
    if icon == "repo" {
        return Ok("repo".into());
    }
    if let Some(emoji) = icon.strip_prefix("emoji:") {
        let emoji = emoji.trim();
        if emoji.is_empty() || emoji.contains(['\n', '\r']) || emoji.chars().count() > 8 {
            return Err("pick one emoji".into());
        }
        return Ok(format!("emoji:{emoji}"));
    }
    if let Some(path) = icon.strip_prefix("image:") {
        let path = Path::new(path.trim());
        if !path.is_file() {
            return Err("image file is missing".into());
        }
        return Ok(format!("image:{}", path.display()));
    }
    Err("icon must be an emoji, an image file, or the repo icon".into())
}

fn context_file(project: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = relative.trim().trim_start_matches('/');
    if relative.is_empty() {
        return Err("name the context file".into());
    }
    let rel = Path::new(relative);
    if rel.is_absolute()
        || rel
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("that path leaves context".into());
    }
    Ok(root_of(project).join("context").join(rel))
}

fn git(root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Arbos")
        .env("GIT_AUTHOR_EMAIL", "arbos@localhost")
        .env("GIT_COMMITTER_NAME", "Arbos")
        .env("GIT_COMMITTER_EMAIL", "arbos@localhost")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn git_clean(root: &Path) -> bool {
    let Ok(output) = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()
    else {
        return true;
    };
    output.stdout.iter().all(|byte| byte.is_ascii_whitespace())
}
