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
You keep `.arbos/context/status.md` current. \
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
}

#[derive(Debug, Clone)]
pub struct TurnPrep {
    pub fx_home: String,
    pub system_prompt: String,
    pub agent_id: String,
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
        write_toml(
            &main,
            &AgentFile {
                id: "main".into(),
                role: "coordinator".into(),
                chat_id: String::new(),
                model: "glm-5.3".into(),
            },
        )?;
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
    let system_prompt = if agent.role == "coordinator" {
        COORDINATOR_PROMPT
    } else {
        CHAT_PROMPT
    };
    Some(TurnPrep {
        fx_home: fx.display().to_string(),
        system_prompt: system_prompt.to_owned(),
        agent_id: agent.id,
    })
}

/// Stamp the coordinator agent with the chat created for Main.
pub fn claim_main(project: &Path, chat_id: &str) -> Result<(), String> {
    let _guard = layout_lock();
    let path = root_of(project).join("agents").join("main").join("agent.toml");
    let mut agent = read_agent(&path).unwrap_or(AgentFile {
        id: "main".into(),
        role: "coordinator".into(),
        chat_id: String::new(),
        model: "glm-5.3".into(),
    });
    if agent.chat_id.is_empty() {
        agent.chat_id = chat_id.to_owned();
        write_toml(&path, &agent)?;
    }
    Ok(())
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
