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
Repeat that pair for every worker. Spawn returns at once. machine is \"local\" or the name of a registered install. \
Start every independent worker before you wait for any of them. \
When a worker finishes you see a Worked line. \
A Worked line can also be a subscription wake, with no message from the person. Answer that wake in one sentence. \
Tick the matching line in context/status.md with mcp_arbos_context_write. \
Do not tick a line before that worker reports. \
The person already has one plan sentence. Do not write another paragraph after a worker reports. \
Do not name a file, a path, GOALS.md, or status.md in text the person reads. \
Read `.arbos/context/GOALS.md` first. \
Write what you learn to `.arbos/context/`. \
Keep `status.md` current. \
The checklist is only the active goal's steps. Do not add a step from an older goal. \
When you tick one step, leave the other steps of this goal in place. \
A `/goal` line already includes the workers to spawn. Send them at once. Do not wait for `/skill`. \
An Active goal in GOALS.md stays active after a worker reports. Do not remove it. \
Goals: `.arbos/context/GOALS.md`. Status: `.arbos/context/status.md`. Read them before acting.";

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
    /// `local`, or the install name that runs this worker.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    machine: String,
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
    pub machine: String,
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
    write_if_missing(
        &root.join("subscriptions").join("morning.toml"),
        "kind = \"timer\"\nhour = 8\nminute = 0\nprompt = \"Morning. Read context/status.md and say what is next.\"\n",
    )?;
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
    if let Some(file) = read_project(project) {
        crate::arbos_remote::remember_project(&file.name, project);
    }
    Ok(())
}

/// Home machine recorded in `project.toml`. Empty and missing both mean `local`.
pub fn home_of(project: &Path) -> String {
    read_project(project)
        .map(|file| {
            let home = file.home.trim();
            if home.is_empty() {
                "local".into()
            } else {
                home.to_owned()
            }
        })
        .unwrap_or_else(|| "local".into())
}

/// Point the rail and the scheduler at `home`. The name is a machine, not a path.
pub fn set_home(project: &Path, home: &str) -> Result<(), String> {
    let _guard = layout_lock();
    let home = home.trim();
    if home.is_empty()
        || !home
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err("the machine name is empty".into());
    }
    let path = root_of(project).join("project.toml");
    let mut file = read_project(project).ok_or_else(|| "project.toml is missing".to_string())?;
    file.home = home.to_owned();
    write_toml(&path, &file)
}

pub fn project_id(project: &Path) -> Option<String> {
    read_project(project).map(|file| file.id)
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

/// What `/goal` and `/skill` do with a composer send.
pub enum ComposerCommand {
    /// Ordinary text. Send it as typed.
    Passthrough,
    /// The project files changed. The transcript keeps the typed line.
    Local,
    /// The transcript keeps the typed line. The agent receives this prompt.
    Model(String),
}

/// The Active section of `GOALS.md`, plus the skill that has run for it.
pub struct GoalView {
    pub title: Option<String>,
    pub skill: Option<String>,
}

const SPLIT_SKILL: &str = "\
name: split
write: research/skill-split.md
line: Skill split ran.
worker: oak research/oak.md Oak is ready.
worker: pine research/pine.md Pine is ready.
---
Split the active goal into two workers.
Spawn both before you wait.
When a worker reports, tick its checklist line.
Leave the Active goal in place.
";

/// Read the goal a person can still see. A finished step does not clear it.
pub fn goal_view(project: &Path) -> GoalView {
    let Ok(text) = fs::read_to_string(root_of(project).join("context").join("GOALS.md")) else {
        return GoalView {
            title: None,
            skill: None,
        };
    };
    let mut title = None;
    let mut skill = None;
    let mut in_active = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            in_active = heading.trim() == "Active";
            continue;
        }
        if !in_active {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Skill:") {
            let name = rest.trim();
            if !name.is_empty() {
                skill = Some(name.to_owned());
            }
            continue;
        }
        if title.is_none() && !trimmed.is_empty() {
            title = Some(trimmed.to_owned());
        }
    }
    GoalView { title, skill }
}

/// `/goal …` records the goal and tells Main to send the workers.
/// `/skill …` runs that skill, then tells Main.
pub fn prepare_composer_command(project: &Path, prompt: &str) -> ComposerCommand {
    if !root_of(project).join("project.toml").is_file() {
        return ComposerCommand::Passthrough;
    }
    let trimmed = prompt.trim();
    if let Some(title) = trimmed.strip_prefix("/goal ") {
        let title = title.trim();
        if title.is_empty() {
            return ComposerCommand::Passthrough;
        }
        return match begin_goal(project, title) {
            Ok(prompt) => ComposerCommand::Model(prompt),
            Err(_) => ComposerCommand::Passthrough,
        };
    }
    if let Some(rest) = trimmed.strip_prefix("/skill ") {
        let Some(name) = rest.split_whitespace().next() else {
            return ComposerCommand::Passthrough;
        };
        return match run_skill(project, name) {
            Ok(prompt) => ComposerCommand::Model(prompt),
            Err(_) => ComposerCommand::Passthrough,
        };
    }
    ComposerCommand::Passthrough
}

/// Record the goal, run the split skill, and hand Main the spawn prompt.
/// The transcript keeps the typed `/goal` line. No second command is required.
fn begin_goal(project: &Path, title: &str) -> Result<String, String> {
    let _guard = layout_lock();
    if !root_of(project).join("project.toml").is_file() {
        return Err("not a project".into());
    }
    let spec = load_skill(project, "split")?;
    let path = context_file(project, &spec.write_path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&path, format!("{}\n", spec.write_line)).map_err(|e| e.to_string())?;
    write_goals(project, title, Some(&spec.name))?;
    install_goal_steps(project, &spec)?;
    Ok(skill_prompt(&spec, Some(title)))
}

fn run_skill(project: &Path, name: &str) -> Result<String, String> {
    let _guard = layout_lock();
    let spec = load_skill(project, name)?;
    let path = context_file(project, &spec.write_path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&path, format!("{}\n", spec.write_line)).map_err(|e| e.to_string())?;
    install_goal_steps(project, &spec)?;
    let title = goal_title_unlocked(project);
    if let Some(title) = title.as_deref() {
        write_goals(project, title, Some(&spec.name))?;
    }
    Ok(skill_prompt(&spec, title.as_deref()))
}

fn skill_step(name: &str) -> String {
    format!("Run the {name} skill")
}

fn skill_prompt(spec: &SkillSpec, goal: Option<&str>) -> String {
    let mut prompt = String::new();
    if let Some(goal) = goal {
        prompt.push_str(&format!("The active goal is: {goal}.\n"));
    }
    prompt.push_str(&format!(
        "The {} skill has already run. {} says: {}.\n",
        spec.name, spec.write_path, spec.write_line
    ));
    prompt.push_str("The goal stays active. Do not edit GOALS.md.\n");
    prompt.push_str(
        "The person already started the goal. Do not wait for another command.\n",
    );
    prompt.push_str(
        "Spawn these workers at once, before you wait for any of them, even if their files already exist. Do not write their files yourself.\n",
    );
    for (name, file, line) in &spec.workers {
        prompt.push_str(&format!(
            "Name one worker {name}. Its only job is to write {file} with the line {line}.\n"
        ));
    }
    prompt.push_str(
        "When each reports, tick that line in context/status.md. Do not tick a line before that worker reports.\n",
    );
    prompt.push_str(
        "The checklist already lists only this goal. Do not add a step from an older goal.\n",
    );
    if !spec.body.is_empty() {
        prompt.push_str(&spec.body);
        prompt.push('\n');
    }
    let sentence = worker_plan_sentence(&spec.workers);
    prompt.push_str(&format!(
        "The only sentence the person reads from you is: {sentence}\n"
    ));
    prompt.push_str(
        "Write that sentence once. Do not write a second sentence. Do not name a file, a path, GOALS.md, or status.md.\n",
    );
    prompt
}

/// The one sentence Main shows for the split workers.
pub fn plan_sentence(project: &Path) -> Option<String> {
    let spec = load_skill(project, "split").ok()?;
    Some(worker_plan_sentence(&spec.workers))
}

fn worker_plan_sentence(workers: &[(String, String, String)]) -> String {
    let names: Vec<String> = workers
        .iter()
        .filter_map(|(name, _, _)| {
            let name = name.trim();
            let mut chars = name.chars();
            let first = chars.next()?;
            Some(first.to_uppercase().collect::<String>() + chars.as_str())
        })
        .collect();
    match names.as_slice() {
        [] => "The workers are on this goal.".to_owned(),
        [one] => format!("{one} is on this goal."),
        [first, second] => format!("{first} and {} are on this goal.", second.to_lowercase()),
        [first, rest @ ..] => {
            let mut middle: Vec<String> = rest[..rest.len() - 1]
                .iter()
                .map(|name| name.to_lowercase())
                .collect();
            middle.insert(0, first.clone());
            let last = rest[rest.len() - 1].to_lowercase();
            format!("{} and {last} are on this goal.", middle.join(", "))
        }
    }
}

struct SkillSpec {
    name: String,
    write_path: String,
    write_line: String,
    workers: Vec<(String, String, String)>,
    body: String,
}

fn load_skill(project: &Path, name: &str) -> Result<SkillSpec, String> {
    let path = skill_file(project, name)?;
    if !path.is_file() {
        if slug(name) != "split" {
            return Err("that skill is not in this project".into());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&path, SPLIT_SKILL).map_err(|e| e.to_string())?;
    }
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    parse_skill(&text)
}

fn parse_skill(text: &str) -> Result<SkillSpec, String> {
    let (head, body) = text.split_once("\n---\n").unwrap_or((text, ""));
    let mut name = String::new();
    let mut write_path = String::new();
    let mut write_line = String::new();
    let mut workers = Vec::new();
    for line in head.lines() {
        let line = line.trim();
        if line.is_empty() || line == "---" {
            continue;
        }
        if let Some(rest) = line.strip_prefix("name:") {
            name = rest.trim().to_owned();
        } else if let Some(rest) = line.strip_prefix("write:") {
            write_path = rest.trim().to_owned();
        } else if let Some(rest) = line.strip_prefix("line:") {
            write_line = rest.trim().to_owned();
        } else if let Some(rest) = line.strip_prefix("worker:") {
            let mut parts = rest.split_whitespace();
            let worker_name = parts.next().unwrap_or("").to_owned();
            let file = parts.next().unwrap_or("").to_owned();
            let worker_line = parts.collect::<Vec<_>>().join(" ");
            if worker_name.is_empty() || file.is_empty() || worker_line.is_empty() {
                return Err("a worker needs a name, a file, and a line".into());
            }
            workers.push((worker_name, file, worker_line));
        } else {
            return Err("the skill has a line that is not a step".into());
        }
    }
    if name.is_empty() || write_path.is_empty() || write_line.is_empty() || workers.is_empty() {
        return Err("the skill is missing a step".into());
    }
    Ok(SkillSpec {
        name,
        write_path,
        write_line,
        workers,
        body: body.trim().to_owned(),
    })
}

fn skill_file(project: &Path, name: &str) -> Result<PathBuf, String> {
    let name = slug(name);
    if name.is_empty() {
        return Err("name the skill".into());
    }
    Ok(root_of(project).join("skills").join(name).join("SKILL.md"))
}

fn write_goals(project: &Path, title: &str, skill: Option<&str>) -> Result<(), String> {
    let mut body = format!("# Goals\n\n## Active\n\n{title}\n");
    if let Some(skill) = skill {
        body.push_str(&format!("\nSkill: {skill}\n"));
    }
    body.push_str("\n## Standing\n\nStanding goals for this project.\n");
    fs::write(root_of(project).join("context").join("GOALS.md"), body).map_err(|e| e.to_string())
}

fn goal_title_unlocked(project: &Path) -> Option<String> {
    goal_view(project).title
}

/// The checklist a person sees is this goal only. Older goals stay in the
/// notes. They do not keep a checkbox under the current goal.
fn install_goal_steps(project: &Path, spec: &SkillSpec) -> Result<(), String> {
    let mut steps = vec![(true, skill_step(&spec.name))];
    let mut names = Vec::new();
    for (name, _, line) in &spec.workers {
        steps.push((false, line.clone()));
        names.push(name.clone());
    }
    replace_goal_checklist(project, &steps)?;
    reopen_workers(project, &names);
    Ok(())
}

fn replace_goal_checklist(project: &Path, steps: &[(bool, String)]) -> Result<(), String> {
    let path = root_of(project).join("context").join("status.md");
    let current = fs::read_to_string(&path).unwrap_or_default();
    let mut rest = String::new();
    for line in current.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "# Status" {
            continue;
        }
        if is_checkbox(trimmed) {
            continue;
        }
        rest.push_str(trimmed);
        rest.push('\n');
    }
    let mut body = String::from("# Status\n\n");
    for (done, text) in steps {
        let mark = if *done { "- [x]" } else { "- [ ]" };
        body.push_str(&format!("{mark} {text}\n"));
    }
    if !rest.trim().is_empty() {
        body.push('\n');
        body.push_str(rest.trim_end());
        body.push('\n');
    }
    fs::write(path, body).map_err(|e| e.to_string())
}

fn is_checkbox(trimmed: &str) -> bool {
    trimmed.starts_with("- [ ]") || trimmed.starts_with("- [x]") || trimmed.starts_with("- [X]")
}

/// A new goal has not finished these workers yet. A previous "done" must
/// not check the new steps.
fn reopen_workers(project: &Path, names: &[String]) {
    let root = root_of(project);
    for name in names {
        let path = root.join("agents").join(slug(name)).join("agent.toml");
        let Some(mut agent) = read_agent(&path) else {
            continue;
        };
        if agent.role != "worker" {
            continue;
        }
        agent.status = "idle".into();
        agent.result.clear();
        let _ = write_toml(&path, &agent);
    }
}

/// Mark this worker's checklist line done. A trailing period does not matter.
fn tick_worker_ready(project: &Path, worker_id: &str) {
    let mut chars = worker_id.chars();
    let Some(first) = chars.next() else {
        return;
    };
    let name = first.to_uppercase().collect::<String>() + chars.as_str();
    let want = format!("{name} is ready");
    let path = root_of(project).join("context").join("status.md");
    let Ok(current) = fs::read_to_string(&path) else {
        return;
    };
    let mut changed = false;
    let lines: Vec<String> = current
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            let rest = trimmed
                .strip_prefix("- [ ]")
                .or_else(|| trimmed.strip_prefix("- [x]"))
                .or_else(|| trimmed.strip_prefix("- [X]"));
            let Some(rest) = rest else {
                return line.to_owned();
            };
            let text = rest.trim().trim_end_matches('.').trim();
            if text != want || !trimmed.starts_with("- [ ]") {
                return line.to_owned();
            }
            changed = true;
            format!("- [x] {}", rest.trim())
        })
        .collect();
    if !changed {
        return;
    }
    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    let _ = fs::write(path, out);
}

/// Checkbox rows the person sees. An active goal shows only its own steps.
/// A worker step is checked when that worker has finished this goal.
pub fn status_items(project: &Path) -> Vec<(bool, String)> {
    let _guard = layout_lock();
    reconcile_done_workers(project);
    if let Some(items) = active_goal_steps(project) {
        return items;
    }
    file_checkboxes(project)
}

fn active_goal_steps(project: &Path) -> Option<Vec<(bool, String)>> {
    let skill = goal_view(project).skill?;
    if skill.is_empty() {
        return None;
    }
    let spec = load_skill(project, &skill).ok()?;
    let finished = finished_worker_ids(project);
    let mut items = vec![(true, skill_step(&spec.name))];
    for (name, _, line) in spec.workers {
        let done = finished.iter().any(|id| id == &slug(&name));
        items.push((done, line));
    }
    Some(items)
}

fn finished_worker_ids(project: &Path) -> Vec<String> {
    list_agents(&root_of(project))
        .into_iter()
        .filter(|agent| agent.role == "worker" && agent.status == "done")
        .map(|agent| agent.id)
        .collect()
}

fn file_checkboxes(project: &Path) -> Vec<(bool, String)> {
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

fn reconcile_done_workers(project: &Path) {
    let root = root_of(project);
    for agent in list_agents(&root) {
        if agent.role == "worker" && agent.status == "done" {
            tick_worker_ready(project, &agent.id);
        }
    }
}

const SUMMARY_NOTICE: &str = "Summarized conversation.";
const SUMMARY_PATHS: &str = "Goals: `.arbos/context/GOALS.md`. Status: `.arbos/context/status.md`. Read them before acting.";

/// `/summarize` and `/compact` on idle Main.
pub fn is_summarize_command(shown: &str) -> bool {
    let shown = shown.trim();
    shown == "/summarize" || shown == "/compact"
}

/// Write a summary of the conversation. The last user message is kept word
/// for word. `GOALS.md` and `status.md` are not changed. On failure the
/// caller must leave the fx session in place.
pub fn write_conversation_summary(
    project: &Path,
    chat_id: &str,
    lines: &[String],
) -> Result<String, String> {
    let _guard = layout_lock();
    let root = root_of(project);
    if !root.join("project.toml").is_file() {
        return Err("not a project".into());
    }
    if main_chat_id(project) != chat_id {
        return Err("not Main".into());
    }
    let mut last_user = String::new();
    let mut earlier = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || is_summarize_command(trimmed) {
            continue;
        }
        if trimmed.starts_with("Worked ·") || trimmed.contains("on this goal") {
            earlier.push(trimmed.to_owned());
            continue;
        }
        if !last_user.is_empty() {
            earlier.push(last_user.clone());
        }
        last_user = trimmed.to_owned();
    }
    if last_user.is_empty() && earlier.is_empty() {
        return Err("nothing to summarize".into());
    }
    let mut body = String::from("Summarized conversation.\n\n");
    if earlier.is_empty() {
        body.push_str("No earlier conversation.\n");
    } else {
        for line in &earlier {
            body.push_str(line);
            body.push('\n');
        }
    }
    body.push_str("\nLast message, word for word:\n");
    body.push_str(&last_user);
    body.push_str("\n\n");
    body.push_str(SUMMARY_PATHS);
    body.push('\n');
    let dir = root.join("runtime");
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let path = dir.join("main-summary.md");
    let tmp = dir.join("main-summary.md.tmp");
    fs::write(&tmp, &body).map_err(|err| err.to_string())?;
    if let Err(err) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(err.to_string());
    }
    Ok(SUMMARY_NOTICE.to_owned())
}

/// The prompt for the first turn after a summary. `None` when Main has not
/// been summarized. The two paths are re-injected after the summary.
pub fn prompt_after_summary(project: &Path, chat_id: &str, prompt: &str) -> Option<String> {
    if main_chat_id(project) != chat_id {
        return None;
    }
    let path = root_of(project).join("runtime").join("main-summary.md");
    let summary = fs::read_to_string(path).ok()?;
    let summary = summary.trim();
    if summary.is_empty() {
        return None;
    }
    Some(format!(
        "{summary}\n\nThe person says: {prompt}\n\
         Read the Goals path and the Status path before you answer.\n\
         Reply with one sentence under 70 characters. Name the goal and what is left.\n\
         If every step is checked, say nothing is left.\n\
         Do not write anything. Do not use the words checklist, tick, or report.\n"
    ))
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

/// Chat id the phone follows for Main.
///
/// A claimed coordinator uses the chat id in `agents/main/agent.toml`.
/// Before that claim, Main is `{project id}-main`.
pub fn main_chat_id(project: &Path) -> String {
    let path = root_of(project)
        .join("agents")
        .join("main")
        .join("agent.toml");
    let claimed = read_agent(&path)
        .map(|agent| agent.chat_id)
        .unwrap_or_default();
    let claimed = claimed.trim();
    if !claimed.is_empty() {
        return claimed.to_owned();
    }
    let id = project_id(project).unwrap_or_default();
    format!("{id}-main")
}

/// Stamp the coordinator agent with the chat created for Main.
pub fn claim_main(project: &Path, chat_id: &str) -> Result<(), String> {
    let _guard = layout_lock();
    let path = root_of(project)
        .join("agents")
        .join("main")
        .join("agent.toml");
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
        machine: String::new(),
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
        rank(&a.role).cmp(&rank(&b.role)).then(a.id.cmp(&b.id))
    });
    rows
}

/// Create or reuse `agents/<name>/`. `machine` is `local` or a registered
/// install. The chat id is filled in by [`bind_worker_chat`] when this
/// install runs the worker.
pub fn reserve_worker(
    project: &Path,
    name: &str,
    brief: &str,
    model: &str,
    machine: Option<&str>,
) -> Result<AgentView, String> {
    let _guard = layout_lock();
    let machine = machine.unwrap_or("local").trim();
    if machine.is_empty() {
        return Err("name the machine".into());
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
        machine: String::new(),
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
    agent.machine = machine.to_owned();
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
    if status == "done" {
        tick_worker_ready(project, &agent.id);
    }
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
    let path = root_of(project).join("agents").join(&id).join("agent.toml");
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
        machine: agent.machine,
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
                machine: String::new(),
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
