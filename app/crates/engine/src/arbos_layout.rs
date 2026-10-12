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
The arbos tools are mcp_arbos_agents_spawn, mcp_arbos_agents_send, mcp_arbos_agents_list, mcp_arbos_agents_result, mcp_arbos_agents_transcript, mcp_arbos_context_read, and mcp_arbos_context_write. \
A tool works only after mcp_select_tool names it. The schema is ready on the next step, so call the tool then. \
To spawn one worker: mcp_select_tool {\"name\":\"mcp_arbos_agents_spawn\"}, then mcp_arbos_agents_spawn. \
Repeat that pair for every worker. Spawn returns at once. machine is \"local\" or the name of a registered install. \
Start every independent worker before you wait for any of them. \
Every message from the person follows one rule. \
Answerable with no tool call or one quick call? Answer. \
Otherwise delegate the whole request now, one worker per independent stream, in the same turn. \
Pass paths, not content. End the turn after dispatch. \
Do not poll a worker; its completion will wake you. \
mcp_arbos_agents_transcript shows a worker's last few turns. Look once, with a small tail, and do not read a worker's files to follow it. \
When a worker finishes you see a Worked line. \
A Worked line can also be a subscription wake, with no message from the person. Answer that wake in one sentence. \
The status page is rewritten when a worker reports. Do not write status.md yourself. \
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
The path is relative to context, for example internal/note.md. \
Then reply with that path and stop. \
Read `.arbos/context/GOALS.md` first. \
Write what you learn to `.arbos/context/`.";

/// The house rules every worker gets after [`WORKER_PROMPT`]. `{id}` is the
/// worker's agent id. [`worker_prompt`] fills it in.
pub const WORKER_HOUSE_RULES: &str = "\
You are a worker, not Main. Main owns `status.md` and `preferences.md`; do not edit them. \
Write a deliverable to the exact path the brief assigns under `context/docs/`. \
Write media under `context/media/`. \
Write everything else, such as notes, evidence, and working files, under `context/internal/`. \
Start every report under `docs/` or `internal/` with this frontmatter and nothing before it:\n\
---\narbos:\n  agentId: \"{id}\"\n---\n\
Edit only a report whose frontmatter carries your own agentId. Otherwise write a new file and link the old one. \
End your reply with the outcome, the links, the blockers, and one line that starts with `Internal:` naming the files you wrote under `internal/`.";

/// The full worker system prompt for one agent id.
pub fn worker_prompt(agent_id: &str) -> String {
    format!(
        "{WORKER_PROMPT} {}",
        WORKER_HOUSE_RULES.replace("{id}", agent_id)
    )
}

pub const CHAT_PROMPT: &str = "\
Read `.arbos/context/GOALS.md` first. \
Write what you learn to `.arbos/context/`. \
Keep `status.md` current.";

const GITIGNORE: &str = "runtime/\n";

/// Folders under `context/`. `docs/` holds deliverables, `media/` holds
/// stills and recordings, `internal/` holds everything else a worker writes.
/// `research/` stays for the split skill.
pub const CONTEXT_DIRS: [&str; 4] = ["docs", "internal", "media", "research"];

/// Folders whose files are reports: a worker's write there must carry its
/// own `arbos: agentId` frontmatter.
const REPORT_DIRS: [&str; 2] = ["docs", "internal"];

/// The fewest turns `agents.transcript` returns, the default, and the most.
pub const TRANSCRIPT_TAIL_MIN: usize = 1;
pub const TRANSCRIPT_TAIL_DEFAULT: usize = 10;
pub const TRANSCRIPT_TAIL_MAX: usize = 50;
/// Longest text kept per transcript field. The rest is replaced with `…`.
const TRANSCRIPT_TEXT_BYTES: usize = 2000;

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
    ensure_context_dirs(&root)?;
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

/// Create every folder in [`CONTEXT_DIRS`] under `root/context`, each with a
/// `.gitkeep` so an empty folder is still committed. Existing files stay.
fn ensure_context_dirs(root: &Path) -> Result<(), String> {
    let context = root.join("context");
    for dir in CONTEXT_DIRS {
        let path = context.join(dir);
        fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        write_if_missing(&path.join(".gitkeep"), "")?;
    }
    Ok(())
}

/// Write one file under `.arbos/context/`. The relative path cannot leave
/// that directory.
pub fn write_context(project: &Path, relative: &str, text: &str) -> Result<(), String> {
    let _guard = layout_lock();
    let relative = relative.trim();
    if relative == "status.md" || relative == "archived.md" {
        return Err("Main writes the status page".into());
    }
    write_context_unlocked(project, relative, text)
}

/// A worker's write. A report under `docs/` or `internal/` must start with
/// the worker's own `arbos: agentId` frontmatter, and the worker cannot
/// replace a report that another agent wrote.
pub fn write_worker_context(
    project: &Path,
    agent_id: &str,
    relative: &str,
    text: &str,
) -> Result<(), String> {
    let _guard = layout_lock();
    let relative = relative.trim();
    if relative == "status.md" || relative == "archived.md" {
        return Err("Main writes the status page".into());
    }
    let path = context_file(project, relative)?;
    let existing = fs::read_to_string(&path).ok();
    check_worker_report(relative, agent_id, text, existing.as_deref())?;
    write_context_unlocked(project, relative, text)
}

fn write_context_unlocked(project: &Path, relative: &str, text: &str) -> Result<(), String> {
    let path = context_file(project, relative)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&path, text).map_err(|e| e.to_string())
}

/// The frontmatter rule for one worker write. `existing` is the file that is
/// there now, if any. A path outside the report folders always passes.
pub fn check_worker_report(
    relative: &str,
    agent_id: &str,
    text: &str,
    existing: Option<&str>,
) -> Result<(), String> {
    if !is_report_path(relative) {
        return Ok(());
    }
    let claimed = report_agent_id(text).ok_or_else(|| {
        format!("a report must start with frontmatter: ---, arbos:, agentId: \"{agent_id}\", ---")
    })?;
    if claimed != agent_id {
        return Err(format!(
            "the frontmatter names {claimed}; this worker is {agent_id}"
        ));
    }
    if let Some(existing) = existing {
        match report_agent_id(existing) {
            Some(owner) if owner == agent_id => {}
            Some(owner) => {
                return Err(format!(
                    "that report belongs to {owner}; write a new file and link this one"
                ));
            }
            None => {
                return Err(
                    "that file is not a worker report; write a new file and link this one".into(),
                );
            }
        }
    }
    Ok(())
}

fn is_report_path(relative: &str) -> bool {
    let relative = relative.trim().trim_start_matches('/');
    let first = Path::new(relative)
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str());
    let Some(first) = first else {
        return false;
    };
    REPORT_DIRS.contains(&first) && Path::new(relative).components().count() > 1
}

/// The agent id a report claims in its leading frontmatter:
///
/// ```text
/// ---
/// arbos:
///   agentId: "oak"
/// ---
/// ```
///
/// Nothing may come before the first `---`. Quotes around the id are
/// optional. A one-line `arbos: agentId: "oak"` is accepted too.
pub fn report_agent_id(text: &str) -> Option<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut in_arbos = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            return None;
        }
        if let Some(rest) = trimmed.strip_prefix("arbos:") {
            in_arbos = true;
            if let Some(id) = frontmatter_value(rest.trim(), "agentId:") {
                return Some(id);
            }
            continue;
        }
        if !line.starts_with([' ', '\t']) {
            in_arbos = false;
        }
        if in_arbos && let Some(id) = frontmatter_value(trimmed, "agentId:") {
            return Some(id);
        }
    }
    None
}

fn frontmatter_value(line: &str, key: &str) -> Option<String> {
    let value = line.strip_prefix(key)?.trim();
    let value = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
        .unwrap_or(value)
        .trim();
    if value.is_empty() {
        return None;
    }
    Some(value.to_owned())
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
    prompt.push_str("The person already started the goal. Do not wait for another command.\n");
    prompt.push_str(
        "Spawn these workers at once, before you wait for any of them, even if their files already exist. Do not write their files yourself.\n",
    );
    for (name, file, line) in &spec.workers {
        prompt.push_str(&format!(
            "Name one worker {name}. Its only job is to write {file} with the line {line}.\n"
        ));
    }
    prompt.push_str(
        "When each reports, the status page updates on its own. Do not write status.md.\n",
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

#[derive(Clone)]
struct PageItem {
    done: bool,
    label: String,
    target: String,
    readout: String,
    rank: u64,
}

fn file_rank(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn context_first_line(project: &Path, relative: &str) -> Option<String> {
    let path = context_file(project, relative).ok()?;
    let text = fs::read_to_string(path).ok()?;
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    if line.chars().count() > 72 {
        return None;
    }
    Some(line.to_owned())
}

fn title_case_name(name: &str) -> String {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_uppercase().collect::<String>() + chars.as_str()
}

fn page_line(item: &PageItem) -> String {
    let mark = if item.done { "x" } else { " " };
    if item.target.is_empty() {
        format!("- [{mark}] {} — {}", item.label, item.readout)
    } else {
        format!(
            "- [{mark}] [{}]({}) — {}",
            item.label, item.target, item.readout
        )
    }
}

fn tldr_line(item: &PageItem) -> String {
    if item.target.is_empty() {
        format!("- {} — {}", item.label, item.readout)
    } else {
        format!("- [{}]({}) — {}", item.label, item.target, item.readout)
    }
}

/// Keep every open item. Keep the three newest done items. The rest move.
fn cap_done(items: Vec<PageItem>) -> (Vec<PageItem>, Vec<PageItem>) {
    let mut open = Vec::new();
    let mut done = Vec::new();
    for item in items {
        if item.done {
            done.push(item);
        } else {
            open.push(item);
        }
    }
    done.sort_by(|left, right| right.rank.cmp(&left.rank));
    let overflow = if done.len() > 3 {
        done.split_off(3)
    } else {
        Vec::new()
    };
    done.sort_by(|left, right| left.rank.cmp(&right.rank));
    open.append(&mut done);
    (open, overflow)
}

fn atomic_context_write(path: &Path, body: &str) -> Result<(), String> {
    let tmp = path.with_extension("md.tmp");
    fs::write(&tmp, body).map_err(|err| err.to_string())?;
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err.to_string());
    }
    Ok(())
}

fn append_archived(project: &Path, section: &str, lines: &[String]) -> Result<(), String> {
    if lines.is_empty() {
        return Ok(());
    }
    let path = root_of(project).join("context").join("archived.md");
    let mut body = fs::read_to_string(&path).unwrap_or_else(|_| "# Archived\n".to_owned());
    if !body.contains("# Archived") {
        body = format!("# Archived\n\n{body}");
    }
    let fresh: Vec<&String> = lines.iter().filter(|line| !body.contains(line.as_str())).collect();
    if fresh.is_empty() {
        return Ok(());
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&format!("\n## {section}\n\n"));
    for line in fresh {
        body.push_str(line);
        body.push('\n');
    }
    atomic_context_write(&path, &body)
}

/// Rewrite `status.md` from the goal and the workers. Main is the only writer.
pub fn publish_status_page(project: &Path) -> Result<(), String> {
    let _guard = layout_lock();
    publish_status_page_unlocked(project)
}

fn publish_status_page_unlocked(project: &Path) -> Result<(), String> {
    let root = root_of(project);
    if !root.join("project.toml").is_file() {
        return Err("not a project".into());
    }
    let view = goal_view(project);
    let heading = view
        .title
        .as_deref()
        .map(|title| title.trim().trim_end_matches('.').trim().to_owned())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "This project".to_owned());
    let mut now = Vec::new();
    let mut skill_ids = Vec::new();
    if let Some(skill) = view.skill.as_deref() {
        if let Ok(spec) = load_skill(project, skill) {
            let target = spec.write_path.clone();
            let rank = context_file(project, &target)
                .map(|path| file_rank(&path))
                .unwrap_or(0);
            let readout = context_first_line(project, &target).unwrap_or(spec.write_line);
            now.push(PageItem {
                done: true,
                label: title_case_name(&spec.name),
                target,
                readout,
                rank,
            });
            let finished = finished_worker_ids(project);
            for (name, file, line) in spec.workers {
                let id = slug(&name);
                skill_ids.push(id.clone());
                let done = finished.iter().any(|finished_id| finished_id == &id);
                let rank = context_file(project, &file)
                    .map(|path| file_rank(&path))
                    .unwrap_or(0);
                let readout = context_first_line(project, &file).unwrap_or(line);
                now.push(PageItem {
                    done,
                    label: title_case_name(&name),
                    target: file,
                    readout,
                    rank,
                });
            }
        }
    }
    let (now, _now_overflow) = cap_done(now);
    let mut earlier = Vec::new();
    for agent in list_agents(&root) {
        if agent.role != "worker"
            || agent.status != "done"
            || skill_ids.iter().any(|id| id == &agent.id)
        {
            continue;
        }
        let target = format!("research/{}.md", agent.id);
        let rank = file_rank(&root.join("agents").join(&agent.id).join("agent.toml"));
        let readout = context_first_line(project, &target).unwrap_or_else(|| {
            let line = agent.result.lines().map(str::trim).find(|line| !line.is_empty());
            match line {
                Some(line) if !line.contains('/') && line.chars().count() <= 72 => {
                    let mut text = line.trim_end_matches('.').to_owned();
                    text.push('.');
                    text
                }
                _ => "Done.".to_owned(),
            }
        });
        earlier.push(PageItem {
            done: true,
            label: title_case_name(&agent.id),
            target: if context_first_line(project, &target).is_some() {
                target
            } else {
                String::new()
            },
            readout,
            rank,
        });
    }
    let (earlier, earlier_overflow) = cap_done(earlier);
    let overflow_lines: Vec<String> = earlier_overflow.iter().map(page_line).collect();
    append_archived(project, "Earlier", &overflow_lines)?;
    let mut kept = Vec::new();
    kept.extend(now.iter().cloned());
    kept.extend(earlier.iter().cloned());
    let mut body = String::from("[Goals](GOALS.md)\n\n");
    if kept.len() >= 6 {
        let mut recent = kept.clone();
        recent.sort_by(|left, right| right.rank.cmp(&left.rank));
        recent.truncate(4);
        body.push_str("<tldr>\n");
        for item in &recent {
            body.push_str(&tldr_line(item));
            body.push('\n');
        }
        body.push_str("</tldr>\n\n");
    }
    if !now.is_empty() {
        body.push_str(&format!("## {heading}\n\n"));
        for item in &now {
            body.push_str(&page_line(item));
            body.push('\n');
        }
        body.push('\n');
    }
    if !earlier.is_empty() {
        body.push_str("## Earlier\n\n");
        for item in &earlier {
            body.push_str(&page_line(item));
            body.push('\n');
        }
        body.push('\n');
    }
    let path = root.join("context").join("status.md");
    let current = fs::read_to_string(&path).unwrap_or_default();
    if current == body {
        return Ok(());
    }
    atomic_context_write(&path, &body)
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

/// How full the window must be before Main summarizes on its own.
/// This is the `auto_compact_percent` value in the fx settings file.
/// A missing or out-of-range value stays at 80, the same default fx uses.
pub fn auto_compact_percent(project: &Path) -> u8 {
    let path = root_of(project)
        .join("agents")
        .join("main")
        .join("fx")
        .join(".fx")
        .join("settings.json");
    let Ok(text) = fs::read_to_string(path) else {
        return 80;
    };
    parse_auto_compact_percent(&text).unwrap_or(80)
}

fn parse_auto_compact_percent(text: &str) -> Option<u8> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let number = value.get("auto_compact_percent")?.as_u64()?;
    if (10..=80).contains(&number) {
        Some(number as u8)
    } else {
        None
    }
}

/// A failed summary must not drop the fx session. Only a written summary does.
pub fn tombstone_after_summary(result: &Result<String, String>) -> bool {
    result.is_ok()
}

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
    let start = lines
        .iter()
        .rposition(|line| line.trim().starts_with("/goal "))
        .unwrap_or(0);
    let mut last_user = String::new();
    let mut earlier = Vec::new();
    for line in &lines[start..] {
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
    let title = goal_view(project)
        .title
        .unwrap_or_else(|| "the goal".to_owned());
    let title = title.trim().trim_end_matches('.').trim();
    let open = file_checkboxes(project)
        .iter()
        .filter(|(done, _)| !done)
        .count();
    let sentence = if open == 0 {
        format!("The goal is {title}, and nothing is left.")
    } else {
        format!("The goal is {title}, and {open} steps are left.")
    };
    Some(format!(
        "{summary}\n\nThe person says: {prompt}\n\
         Read the Goals path and the Status path before you answer.\n\
         A step is only a line that starts with \"- [ ]\" or \"- [x]\".\n\
         Unchecked steps on disk: {open}.\n\
         Ignore every other section of the Status file.\n\
         Do not spawn a worker. Do not write. The goal stays as it is.\n\
         Reply with this sentence and no other words: {sentence}\n\
         Do not mention workers. Do not use the words checklist, tick, report, file, path, or status.\n"
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
        "coordinator" => COORDINATOR_PROMPT.to_owned(),
        "worker" => worker_prompt(&agent.id),
        _ => CHAT_PROMPT.to_owned(),
    };
    Some(TurnPrep {
        fx_home: fx.display().to_string(),
        system_prompt,
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
        let _ = publish_status_page_unlocked(project);
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

/// One turn of a worker's fx conversation, as `agents.transcript` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptTurn {
    /// 1-based position in the whole conversation.
    pub index: usize,
    pub user: String,
    pub reply: String,
    pub tools: Vec<String>,
    /// `completed`, `interrupted`, or `open` for a turn still running.
    pub ended: String,
}

/// The last few turns of one agent's conversation.
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptView {
    pub name: String,
    pub total_turns: usize,
    pub turns: Vec<TranscriptTurn>,
}

/// The last `tail` turns of a worker's fx `events.jsonl`. `tail` is clamped
/// to [`TRANSCRIPT_TAIL_MIN`]..=[`TRANSCRIPT_TAIL_MAX`]; `None` means
/// [`TRANSCRIPT_TAIL_DEFAULT`]. Reads one file; the rest of the log stays out
/// of Main's context.
pub fn worker_transcript(
    project: &Path,
    name: &str,
    tail: Option<usize>,
) -> Result<TranscriptView, String> {
    let id = slug(name);
    if id.is_empty() {
        return Err("name the worker".into());
    }
    let dir = root_of(project).join("agents").join(&id);
    if !dir.join("agent.toml").is_file() {
        return Err("no such worker".into());
    }
    let tail = clamp_tail(tail);
    let Some(log) = latest_events_log(&dir.join("fx")) else {
        return Ok(TranscriptView {
            name: id,
            total_turns: 0,
            turns: Vec::new(),
        });
    };
    let text = fs::read_to_string(&log).map_err(|e| e.to_string())?;
    let turns = parse_transcript(&text);
    let total_turns = turns.len();
    let skip = total_turns.saturating_sub(tail);
    Ok(TranscriptView {
        name: id,
        total_turns,
        turns: turns.into_iter().skip(skip).collect(),
    })
}

fn clamp_tail(tail: Option<usize>) -> usize {
    tail.unwrap_or(TRANSCRIPT_TAIL_DEFAULT)
        .clamp(TRANSCRIPT_TAIL_MIN, TRANSCRIPT_TAIL_MAX)
}

/// fx keeps `<home>/.fx/sessions/<session>/events.jsonl`. A worker normally
/// has one session; when it has more, the newest log wins.
fn latest_events_log(fx_home: &Path) -> Option<PathBuf> {
    let sessions = fs::read_dir(fx_home.join(".fx").join("sessions")).ok()?;
    sessions
        .flatten()
        .map(|entry| entry.path().join("events.jsonl"))
        .filter(|path| path.is_file())
        .max_by_key(|path| fs::metadata(path).and_then(|meta| meta.modified()).ok())
}

/// Group fx conversation frames into turns. A turn opens on `user` and
/// closes on `turn_completed` or `interrupted`. Lines that do not parse are
/// skipped; fx guards its own log.
fn parse_transcript(text: &str) -> Vec<TranscriptTurn> {
    let mut turns: Vec<TranscriptTurn> = Vec::new();
    let mut open: Option<TranscriptTurn> = None;
    for line in text.lines() {
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(event) = frame.get("event").and_then(|event| event.as_object()) else {
            continue;
        };
        let Some((kind, payload)) = event.iter().next() else {
            continue;
        };
        let string = |key: &str| {
            payload
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        match kind.as_str() {
            "user" => {
                if let Some(turn) = open.take() {
                    turns.push(close_turn(turn, "open"));
                }
                open = Some(TranscriptTurn {
                    index: turns.len() + 1,
                    user: clip_text(&string("text")),
                    reply: String::new(),
                    tools: Vec::new(),
                    ended: "open".into(),
                });
            }
            "assistant" => {
                if let Some(turn) = open.as_mut() {
                    let text = string("text");
                    if !text.trim().is_empty() {
                        if !turn.reply.is_empty() {
                            turn.reply.push('\n');
                        }
                        turn.reply.push_str(&text);
                    }
                }
            }
            "tool_call" => {
                if let Some(turn) = open.as_mut() {
                    let name = string("tool_name");
                    if !name.is_empty() {
                        turn.tools.push(name);
                    }
                }
            }
            "turn_completed" => {
                if let Some(turn) = open.take() {
                    turns.push(close_turn(turn, "completed"));
                }
            }
            "interrupted" => {
                if let Some(turn) = open.take() {
                    turns.push(close_turn(turn, "interrupted"));
                }
            }
            _ => {}
        }
    }
    if let Some(turn) = open {
        turns.push(close_turn(turn, "open"));
    }
    turns
}

fn close_turn(mut turn: TranscriptTurn, ended: &str) -> TranscriptTurn {
    turn.reply = clip_text(&turn.reply);
    turn.ended = ended.to_owned();
    turn
}

fn clip_text(text: &str) -> String {
    let text = text.trim();
    if text.len() <= TRANSCRIPT_TEXT_BYTES {
        return text.to_owned();
    }
    let mut end = TRANSCRIPT_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
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

#[cfg(test)]
mod summary_failure_tests {
    use super::{
        auto_compact_percent, ensure, main_chat_id, parse_auto_compact_percent,
        tombstone_after_summary, write_conversation_summary,
    };
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn a_failed_summary_does_not_tombstone_and_leaves_the_file() {
        let err: Result<String, String> = Err("rename failed".into());
        assert!(!tombstone_after_summary(&err));
        let ok: Result<String, String> = Ok("Summarized conversation.".into());
        assert!(tombstone_after_summary(&ok));

        let root = std::env::temp_dir().join(format!("arbos-summary-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        ensure(&root, Some("Summary")).unwrap();
        let chat = main_chat_id(&root);
        write_conversation_summary(&root, &chat, &["/goal Clear the gate.".into()]).unwrap();
        let path = root.join(".arbos").join("runtime").join("main-summary.md");
        let before = fs::read(&path).unwrap();
        let failed = write_conversation_summary(&root, "not-main", &["/goal Other.".into()]);
        assert!(failed.is_err());
        assert!(!tombstone_after_summary(&failed));
        assert_eq!(fs::read(&path).unwrap(), before);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn auto_compact_percent_reads_the_settings_file() {
        assert_eq!(parse_auto_compact_percent(r#"{"auto_compact_percent":10}"#), Some(10));
        assert_eq!(parse_auto_compact_percent(r#"{"auto_compact_percent":80}"#), Some(80));
        assert_eq!(parse_auto_compact_percent(r#"{"auto_compact_percent":4}"#), None);
        assert_eq!(parse_auto_compact_percent(r#"{"auto_compact_percent":90}"#), None);
        assert_eq!(parse_auto_compact_percent("{}"), None);

        let root = std::env::temp_dir().join(format!("arbos-compact-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        ensure(&root, Some("Compact")).unwrap();
        assert_eq!(auto_compact_percent(&root), 80);
        let settings = root
            .join(".arbos")
            .join("agents")
            .join("main")
            .join("fx")
            .join(".fx");
        fs::create_dir_all(&settings).unwrap();
        fs::write(
            settings.join("settings.json"),
            r#"{"permission_mode":"yolo","auto_compact_percent":10}"#,
        )
        .unwrap();
        assert_eq!(auto_compact_percent(&root), 10);
        let _ = fs::remove_dir_all(PathBuf::from(&root));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OAK_REPORT: &str = "---\narbos:\n  agentId: \"oak\"\n---\n# Note\n";

    #[test]
    fn context_layout_has_docs_internal_media_and_research() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".arbos");
        ensure_context_dirs(&root).unwrap();
        for name in CONTEXT_DIRS {
            let path = root.join("context").join(name);
            assert!(path.is_dir(), "{name} is missing");
            assert!(path.join(".gitkeep").is_file(), "{name} has no .gitkeep");
        }
        assert!(root.join("context").join("internal").is_dir());
        // A second call leaves an existing file alone.
        fs::write(root.join("context").join("internal").join("a.md"), "kept").unwrap();
        ensure_context_dirs(&root).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("context").join("internal").join("a.md")).unwrap(),
            "kept"
        );
    }

    #[test]
    fn worker_prompt_carries_the_house_rules_and_the_id() {
        let prompt = worker_prompt("oak");
        assert!(prompt.starts_with(WORKER_PROMPT));
        assert!(prompt.contains("You are a worker, not Main."));
        assert!(prompt.contains("Main owns `status.md` and `preferences.md`"));
        assert!(prompt.contains("Read `.arbos/context/GOALS.md` first."));
        assert!(prompt.contains("context/docs/"));
        assert!(prompt.contains("context/media/"));
        assert!(prompt.contains("context/internal/"));
        assert!(prompt.contains("---\narbos:\n  agentId: \"oak\"\n---"));
        assert!(!prompt.contains("{id}"));
        assert!(prompt.contains("`Internal:`"));
        assert!(!prompt.contains("Keep `status.md` current"));
        assert!(
            COORDINATOR_PROMPT.contains("Answerable with no tool call or one quick call? Answer.")
        );
        assert!(COORDINATOR_PROMPT.contains("Do not poll a worker; its completion will wake you."));
        assert!(COORDINATOR_PROMPT.contains("mcp_arbos_agents_transcript"));
    }

    #[test]
    fn frontmatter_names_the_agent() {
        assert_eq!(report_agent_id(OAK_REPORT).as_deref(), Some("oak"));
        assert_eq!(
            report_agent_id("---\narbos:\n  agentId: pine\n---\n").as_deref(),
            Some("pine")
        );
        assert_eq!(
            report_agent_id("---\narbos: agentId: 'elm'\n---\nbody").as_deref(),
            Some("elm")
        );
        assert_eq!(
            report_agent_id("\u{feff}---\ntitle: x\narbos:\n  agentId: \"oak\"\n---\n").as_deref(),
            Some("oak")
        );
        // Text before the first fence, no fence, a closed block without the
        // key, or a key outside the arbos map all fail.
        assert_eq!(
            report_agent_id("# Note\n---\narbos:\n  agentId: \"oak\"\n---\n"),
            None
        );
        assert_eq!(report_agent_id("arbos:\n  agentId: \"oak\"\n"), None);
        assert_eq!(
            report_agent_id("---\ntitle: x\n---\narbos:\n  agentId: \"oak\"\n"),
            None
        );
        assert_eq!(
            report_agent_id("---\nother:\n  agentId: \"oak\"\n---\n"),
            None
        );
        assert_eq!(report_agent_id("---\narbos:\n  agentId: \"\"\n---\n"), None);
        assert_eq!(report_agent_id(""), None);
    }

    #[test]
    fn worker_reports_need_their_own_frontmatter() {
        // Outside docs/ and internal/ nothing is checked.
        assert!(check_worker_report("research/oak.md", "oak", "one line", None).is_ok());
        assert!(check_worker_report("status.md", "oak", "# Status", None).is_ok());
        // A report needs frontmatter with this worker's id.
        assert!(check_worker_report("internal/note.md", "oak", "# Note", None).is_err());
        assert!(check_worker_report("docs/note.md", "oak", "# Note", None).is_err());
        let pine = OAK_REPORT.replace("oak", "pine");
        assert!(check_worker_report("internal/note.md", "oak", &pine, None).is_err());
        assert!(check_worker_report("internal/note.md", "oak", OAK_REPORT, None).is_ok());
        assert!(check_worker_report("docs/deep/er/note.md", "oak", OAK_REPORT, None).is_ok());
        // An existing report: only the owner may replace it.
        assert!(
            check_worker_report("internal/note.md", "oak", OAK_REPORT, Some(OAK_REPORT)).is_ok()
        );
        assert!(check_worker_report("internal/note.md", "oak", OAK_REPORT, Some(&pine)).is_err());
        assert!(
            check_worker_report("internal/note.md", "oak", OAK_REPORT, Some("# Main's doc"))
                .is_err()
        );
    }

    #[test]
    fn worker_write_enforces_the_frontmatter_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        fs::create_dir_all(root_of(project).join("context")).unwrap();
        assert!(write_worker_context(project, "oak", "internal/note.md", "# Note").is_err());
        assert!(
            !root_of(project)
                .join("context")
                .join("internal")
                .join("note.md")
                .exists()
        );
        write_worker_context(project, "oak", "internal/note.md", OAK_REPORT).unwrap();
        assert_eq!(
            read_context(project, "internal/note.md").unwrap(),
            OAK_REPORT
        );
        let pine = OAK_REPORT.replace("oak", "pine");
        assert!(write_worker_context(project, "pine", "internal/note.md", &pine).is_err());
        assert_eq!(
            read_context(project, "internal/note.md").unwrap(),
            OAK_REPORT
        );
        write_worker_context(project, "pine", "internal/pine.md", &pine).unwrap();
        // Main's own writes are not checked.
        write_context(project, "docs/plan.md", "# Plan").unwrap();
        write_worker_context(project, "oak", "research/oak.md", "Oak is ready.").unwrap();
    }

    fn frame(seq: usize, kind: &str, payload: serde_json::Value) -> String {
        let mut event = serde_json::Map::new();
        event.insert(kind.to_owned(), payload);
        serde_json::json!({
            "schema_version": 3,
            "seq": seq,
            "timestamp_ms": 1_700_000_000_000u64 + seq as u64,
            "event": event,
        })
        .to_string()
    }

    /// `turns` completed turns, each with one tool call, then one open turn.
    fn events_log(turns: usize) -> String {
        let mut seq = 0;
        let mut lines = Vec::new();
        let mut next = |kind: &str, payload: serde_json::Value| {
            seq += 1;
            lines.push(frame(seq, kind, payload));
        };
        for n in 1..=turns {
            next(
                "user",
                serde_json::json!({ "text": format!("ask {n}"), "images": [], "work_id": null }),
            );
            next(
                "tool_call",
                serde_json::json!({ "call_id": format!("c{n}"), "tool_name": "read_file", "arguments_json": "{}" }),
            );
            next(
                "tool_result",
                serde_json::json!({ "call_id": format!("c{n}"), "tool_name": "read_file", "status": "ok" }),
            );
            next(
                "assistant",
                serde_json::json!({ "text": format!("part {n}") }),
            );
            next(
                "assistant",
                serde_json::json!({ "text": format!("reply {n}") }),
            );
            next("turn_completed", serde_json::json!({ "files": [] }));
        }
        next("user", serde_json::json!({ "text": "still going" }));
        next("assistant", serde_json::json!({ "text": "" }));
        lines.push("not json".into());
        lines.join("\n") + "\n"
    }

    fn worker_with_log(project: &Path, id: &str, log: &str) {
        let dir = root_of(project).join("agents").join(id);
        let session = dir.join("fx").join(".fx").join("sessions").join("S1");
        fs::create_dir_all(&session).unwrap();
        fs::write(session.join("events.jsonl"), log).unwrap();
        write_toml(
            &dir.join("agent.toml"),
            &AgentFile {
                id: id.into(),
                role: "worker".into(),
                chat_id: "chat".into(),
                model: "m".into(),
                parent: "main".into(),
                brief: "b".into(),
                status: "working".into(),
                result: String::new(),
                machine: "local".into(),
            },
        )
        .unwrap();
    }

    #[test]
    fn transcript_groups_frames_into_turns() {
        let turns = parse_transcript(&events_log(2));
        assert_eq!(turns.len(), 3);
        assert_eq!(
            turns[0],
            TranscriptTurn {
                index: 1,
                user: "ask 1".into(),
                reply: "part 1\nreply 1".into(),
                tools: vec!["read_file".into()],
                ended: "completed".into(),
            }
        );
        assert_eq!(turns[1].index, 2);
        assert_eq!(turns[2].user, "still going");
        assert_eq!(turns[2].reply, "");
        assert_eq!(turns[2].ended, "open");
        let interrupted = [
            frame(1, "user", serde_json::json!({ "text": "go" })),
            frame(2, "interrupted", serde_json::json!({ "reason": "user" })),
        ]
        .join("\n");
        assert_eq!(parse_transcript(&interrupted)[0].ended, "interrupted");
        let long = "x".repeat(TRANSCRIPT_TEXT_BYTES + 50);
        let clipped = parse_transcript(&frame(1, "user", serde_json::json!({ "text": long })));
        assert_eq!(clipped[0].user.chars().count(), TRANSCRIPT_TEXT_BYTES + 1);
        assert!(clipped[0].user.ends_with('…'));
    }

    #[test]
    fn transcript_tail_is_bounded() {
        assert_eq!(clamp_tail(None), TRANSCRIPT_TAIL_DEFAULT);
        assert_eq!(clamp_tail(Some(0)), TRANSCRIPT_TAIL_MIN);
        assert_eq!(clamp_tail(Some(3)), 3);
        assert_eq!(clamp_tail(Some(usize::MAX)), TRANSCRIPT_TAIL_MAX);

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        // 60 completed turns plus one open turn: more than the largest tail.
        worker_with_log(project, "oak", &events_log(60));

        let view = worker_transcript(project, "oak", Some(3)).unwrap();
        assert_eq!(view.name, "oak");
        assert_eq!(view.total_turns, 61);
        assert_eq!(view.turns.len(), 3);
        assert_eq!(view.turns[0].index, 59);
        assert_eq!(view.turns[0].user, "ask 59");
        assert_eq!(view.turns[2].index, 61);
        assert_eq!(view.turns[2].ended, "open");

        let default = worker_transcript(project, "Oak", None).unwrap();
        assert_eq!(default.turns.len(), TRANSCRIPT_TAIL_DEFAULT);
        assert_eq!(default.turns[0].index, 61 - TRANSCRIPT_TAIL_DEFAULT + 1);

        let most = worker_transcript(project, "oak", Some(10_000)).unwrap();
        assert_eq!(most.turns.len(), TRANSCRIPT_TAIL_MAX);
        assert_eq!(most.turns.last().unwrap().index, 61);

        let least = worker_transcript(project, "oak", Some(0)).unwrap();
        assert_eq!(least.turns.len(), 1);

        // A short log returns everything it has.
        worker_with_log(project, "pine", &events_log(2));
        let short = worker_transcript(project, "pine", Some(10)).unwrap();
        assert_eq!(short.total_turns, 3);
        assert_eq!(short.turns.len(), 3);

        // A worker with no fx session yet, and a worker that does not exist.
        let dir = root_of(project).join("agents").join("elm");
        fs::create_dir_all(dir.join("fx")).unwrap();
        fs::copy(
            root_of(project)
                .join("agents")
                .join("pine")
                .join("agent.toml"),
            dir.join("agent.toml"),
        )
        .unwrap();
        let empty = worker_transcript(project, "elm", None).unwrap();
        assert_eq!(empty.total_turns, 0);
        assert!(empty.turns.is_empty());
        assert!(worker_transcript(project, "ash", None).is_err());
        assert!(worker_transcript(project, "", None).is_err());
    }
}
