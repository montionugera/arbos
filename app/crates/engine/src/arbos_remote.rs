//! A project's home machine, and the engine calls that reach it.
//!
//! `~/.arbos/machines.toml` names each install. The service token for
//! Cloudflare Access stays in the mode 600 secrets directory. SSH copies the
//! project tree. The wake and the worker run go through the home engine.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeron_proto::{ChatConfig, HarnessId, RunRequest, SandboxLevel};

use crate::arbos_layout;
use crate::doc_host::DocHost;
use crate::sessions::SessionsEngine;
use crate::workspace_host::WorkspaceHost;

const JOBS_WAIT: Duration = Duration::from_secs(3);

#[derive(Clone)]
struct RemoteState {
    doc: DocHost,
    workspace: WorkspaceHost,
    sessions: SessionsEngine,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Machine {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub credential: String,
    #[serde(default)]
    pub ssh: String,
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub key: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MachinesFile {
    #[serde(default)]
    machine: Vec<Machine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectIndex {
    name: String,
    path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProjectsFile {
    #[serde(default)]
    project: Vec<ProjectIndex>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Job {
    id: String,
    machine: String,
    project: String,
    name: String,
    brief: String,
    status: String,
    #[serde(default)]
    result: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct JobFile {
    #[serde(default)]
    jobs: Vec<Job>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WorkerFile {
    /// Name to last-seen unix seconds.
    #[serde(default)]
    seen: std::collections::BTreeMap<String, i64>,
}

fn file_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub fn this_machine() -> String {
    std::env::var("ARBOS_MACHINE_NAME")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "local".into())
}

pub fn is_remote_machine(machine: &str) -> bool {
    let machine = machine.trim();
    !machine.is_empty() && machine != "local" && machine != this_machine()
}

/// This install runs the scheduler for the project.
pub fn owns_project(project: &Path) -> bool {
    let home = arbos_layout::home_of(project);
    if home == "local" {
        return this_machine() == "local";
    }
    home == this_machine()
}

pub(crate) fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ARBOS_CONFIG_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Path::new(&home).join(".arbos")
}

fn machines_path() -> PathBuf {
    config_dir().join("machines.toml")
}

fn projects_path() -> PathBuf {
    config_dir().join("projects.toml")
}

fn jobs_path() -> PathBuf {
    config_dir().join("jobs.json")
}

fn workers_path() -> PathBuf {
    config_dir().join("workers.json")
}

fn read_toml<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    let Ok(text) = fs::read_to_string(path) else {
        return T::default();
    };
    toml::from_str(&text).unwrap_or_default()
}

fn write_toml<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let text = toml::to_string_pretty(value).map_err(|err| err.to_string())?;
    fs::write(path, text).map_err(|err| err.to_string())
}

pub fn remember_project(name: &str, path: &Path) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    let Ok(path) = path.canonicalize() else {
        return;
    };
    let Some(path) = path.to_str() else {
        return;
    };
    let _guard = file_lock();
    let mut file = read_toml::<ProjectsFile>(&projects_path());
    if let Some(row) = file.project.iter_mut().find(|row| row.name == name) {
        row.path = path.to_owned();
    } else {
        file.project.push(ProjectIndex {
            name: name.to_owned(),
            path: path.to_owned(),
        });
    }
    let _ = write_toml(&projects_path(), &file);
}

/// Projects the phone may list. A path that is not an Arbos project is skipped.
/// The rows have no secrets.
pub fn phone_projects() -> Vec<Value> {
    let file = read_toml::<ProjectsFile>(&projects_path());
    let mut rows = Vec::new();
    for row in file.project {
        let path = PathBuf::from(&row.path);
        if !path.join(".arbos").join("project.toml").is_file() {
            continue;
        }
        let name = arbos_layout::project_name(&path).unwrap_or(row.name);
        let live = crate::arbos_live::live(&path);
        rows.push(json!({
            "name": name,
            "path": row.path,
            "home": arbos_layout::home_of(&path),
            "mainChatId": arbos_layout::main_chat_id(&path),
            "unread": live.unread,
            "step": live.step,
        }));
    }
    rows
}

fn rpc_slot() -> &'static Mutex<Option<Arc<dyn zeron_rpc::RpcService>>> {
    static SLOT: Mutex<Option<Arc<dyn zeron_rpc::RpcService>>> = Mutex::new(None);
    &SLOT
}

/// Hand the phone websocket the same service the desktop IPC uses.
pub fn attach_rpc(service: Arc<dyn zeron_rpc::RpcService>) {
    let mut slot = rpc_slot().lock().unwrap_or_else(|poison| poison.into_inner());
    *slot = Some(service);
}

fn attached_rpc() -> Option<Arc<dyn zeron_rpc::RpcService>> {
    rpc_slot()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
}

pub fn find_project(name: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("name the project".into());
    }
    let direct = PathBuf::from(name);
    if direct.join(".arbos").join("project.toml").is_file() {
        return Ok(direct);
    }
    let file = read_toml::<ProjectsFile>(&projects_path());
    let matches: Vec<_> = file
        .project
        .into_iter()
        .filter(|row| row.name == name)
        .collect();
    if matches.len() > 1 {
        return Err("that project name is not unique".into());
    }
    let Some(row) = matches.into_iter().next() else {
        return Err("that project is not in the index".into());
    };
    let path = PathBuf::from(row.path);
    if !path.join(".arbos").join("project.toml").is_file() {
        return Err("the indexed folder is not an Arbos project".into());
    }
    Ok(path)
}

pub fn add_machine(machine: Machine) -> Result<(), String> {
    let name = machine.name.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err("the machine name is empty".into());
    }
    let url = machine.url.trim().trim_end_matches('/').to_owned();
    if !(url.starts_with("https://") || url.starts_with("http://127.0.0.1")) {
        return Err("the machine url must be https".into());
    }
    let _guard = file_lock();
    let mut file = read_toml::<MachinesFile>(&machines_path());
    file.machine.retain(|row| row.name != name);
    file.machine.push(Machine {
        name: name.to_owned(),
        url,
        credential: machine.credential.trim().to_owned(),
        ssh: machine.ssh.trim().to_owned(),
        dir: machine.dir.trim().to_owned(),
        key: machine.key.trim().to_owned(),
    });
    write_toml(&machines_path(), &file)
}

fn machine_named(name: &str) -> Result<Machine, String> {
    let file = read_toml::<MachinesFile>(&machines_path());
    file.machine
        .into_iter()
        .find(|row| row.name == name)
        .ok_or_else(|| "that machine is not in machines.toml".to_string())
}

fn safe_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '@' | '.' | '/' | '_' | '-' | ':'))
}

/// Copy `.arbos/` to the machine and tell that engine to run it.
pub async fn move_project(name: &str, to: &str) -> Result<String, String> {
    let project = find_project(name)?;
    let machine = machine_named(to.trim())?;
    arbos_layout::set_home(&project, machine.name.as_str())?;
    arbos_layout::commit_after_turn(&project);
    let remote = copy_over_ssh(&project, &machine)?;
    adopt(&machine, &remote, name.trim()).await?;
    Ok(format!("{} home is {}", name.trim(), machine.name))
}

fn copy_over_ssh(project: &Path, machine: &Machine) -> Result<String, String> {
    if !safe_token(&machine.ssh) || !safe_token(&machine.dir) {
        return Err("the machine needs an ssh target and a directory".into());
    }
    let folder = project
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| safe_token(name))
        .ok_or_else(|| "the project folder name is not usable".to_string())?;
    let dest = format!(
        "{}/{}",
        machine.dir.trim_end_matches('/'),
        folder
    );
    let status = ssh(machine, &format!("mkdir -p {dest}"))?;
    if !status {
        return Err("the remote directory was not created".into());
    }
    let mut tar = Command::new("tar")
        .args(["-czf", "-", "-C"])
        .arg(project)
        .arg(".arbos")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;
    let Some(stdout) = tar.stdout.take() else {
        return Err("the project tree was not copied".into());
    };
    let mut remote = ssh_command(machine);
    remote
        .arg(&machine.ssh)
        .arg(format!("tar -xzf - -C {dest}"))
        .stdin(stdout)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let copied = remote.output().map_err(|err| err.to_string())?;
    let tar_done = tar.wait().map_err(|err| err.to_string())?;
    if !tar_done.success() || !copied.status.success() {
        return Err("the project tree was not copied".into());
    }
    Ok(dest)
}

fn ssh(machine: &Machine, remote_command: &str) -> Result<bool, String> {
    let mut command = ssh_command(machine);
    command
        .arg(&machine.ssh)
        .arg(remote_command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = command.output().map_err(|err| err.to_string())?;
    Ok(output.status.success())
}

fn ssh_command(machine: &Machine) -> Command {
    let mut command = Command::new("ssh");
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg("-o")
        .arg("IdentitiesOnly=yes");
    if !machine.key.trim().is_empty() {
        command.arg("-i").arg(machine.key.trim());
    }
    command
}

fn access_headers(credential: &str) -> Result<(String, String), String> {
    let name = credential.trim();
    if name.is_empty() {
        return Err("the machine has no credential name".into());
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err("the credential name is not usable".into());
    }
    let dir = zeron_harness::secrets::secrets_dir()?;
    let text = fs::read_to_string(dir.join(name)).map_err(|_| "the credential file is missing")?;
    let mut client_id = String::new();
    let mut client_secret = String::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("client_id=") {
            client_id = value.trim().to_owned();
        } else if let Some(value) = line.strip_prefix("client_secret=") {
            client_secret = value.trim().to_owned();
        }
    }
    if client_id.is_empty() || client_secret.is_empty() {
        return Err("the credential file is incomplete".into());
    }
    Ok((client_id, client_secret))
}

fn http_client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|_| "the http client did not start".to_string())
}

async fn authed_request(
    machine: &Machine,
    method: reqwest::Method,
    url: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let client = http_client(Duration::from_secs(30))?;
    let mut request = client.request(method, url);
    if machine.url.starts_with("https://") {
        let (id, secret) = access_headers(&machine.credential)?;
        request = request
            .header("CF-Access-Client-Id", id)
            .header("CF-Access-Client-Secret", secret);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.map_err(|_| "the remote engine did not answer".to_string())?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(safe_remote_error(&text));
    }
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text).map_err(|_| "the remote engine sent a bad reply".to_string())
}

fn safe_remote_error(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    if text.len() > 180
        || lower.contains("token")
        || lower.contains("secret")
        || lower.contains("authorization")
    {
        return "the remote engine rejected the request".into();
    }
    let cleaned: String = text
        .chars()
        .filter(|ch| ch.is_ascii() && !ch.is_control())
        .collect();
    if cleaned.is_empty() {
        "the remote engine rejected the request".into()
    } else {
        cleaned
    }
}

async fn adopt(machine: &Machine, path: &str, name: &str) -> Result<(), String> {
    let url = format!("{}/v1/adopt", machine.url.trim_end_matches('/'));
    let reply = authed_request(
        machine,
        reqwest::Method::POST,
        &url,
        Some(json!({ "path": path, "name": name })),
    )
    .await?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("the remote engine did not adopt the project".into());
    }
    Ok(())
}

pub async fn spawn_cli(
    project_name: &str,
    machine: &str,
    brief: &str,
    worker_name: &str,
) -> Result<String, String> {
    let project = find_project(project_name)?;
    let home = arbos_layout::home_of(&project);
    let target = machine_named(&home)?;
    let remote_path = remote_project_path(&target, &project)?;
    let url = format!("{}/v1/spawn", target.url.trim_end_matches('/'));
    let reply = authed_request(
        &target,
        reqwest::Method::POST,
        &url,
        Some(json!({
            "path": remote_path,
            "machine": machine,
            "brief": brief,
            "name": worker_name,
        })),
    )
    .await?;
    let name = reply
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(worker_name);
    Ok(format!("spawned {name} on {machine}"))
}

fn remote_project_path(machine: &Machine, project: &Path) -> Result<String, String> {
    let folder = project
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| safe_token(name))
        .ok_or_else(|| "the project folder name is not usable".to_string())?;
    if !safe_token(&machine.dir) {
        return Err("the machine directory is missing".into());
    }
    Ok(format!(
        "{}/{}",
        machine.dir.trim_end_matches('/'),
        folder
    ))
}

/// Queue a worker on this engine. The named install picks the job up.
pub fn enqueue_local(machine: &str, project: &Path, name: &str, brief: &str) -> Result<(), String> {
    if !worker_fresh(machine) {
        return Err("machine is not registered".into());
    }
    let Some(path) = project.to_str() else {
        return Err("the project path is not usable".into());
    };
    let _guard = file_lock();
    let mut file = read_jobs();
    file.jobs.push(Job {
        id: crate::new_id(),
        machine: machine.to_owned(),
        project: path.to_owned(),
        name: name.to_owned(),
        brief: brief.to_owned(),
        status: "queued".into(),
        result: String::new(),
    });
    write_jobs(&file)
}

pub fn forward_spawn(project: &Path, machine: &str, name: &str, brief: &str) -> Result<(), String> {
    let home_name = arbos_layout::home_of(project);
    let home = machine_named(&home_name)?;
    let remote_path = remote_project_path(&home, project)?;
    let machine = machine.to_owned();
    let name = name.to_owned();
    let brief = brief.to_owned();
    let result = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "the request runtime did not start".to_string())?;
        runtime.block_on(async move {
            let url = format!("{}/v1/spawn", home.url.trim_end_matches('/'));
            authed_request(
                &home,
                reqwest::Method::POST,
                &url,
                Some(json!({
                    "path": remote_path,
                    "machine": machine,
                    "brief": brief,
                    "name": name,
                })),
            )
            .await
            .map(|_| ())
        })
    })
    .join()
    .map_err(|_| "the request thread stopped".to_string())?;
    result
}

fn read_jobs() -> JobFile {
    let Ok(text) = fs::read_to_string(jobs_path()) else {
        return JobFile::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_jobs(file: &JobFile) -> Result<(), String> {
    if let Some(parent) = jobs_path().parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let text = serde_json::to_string_pretty(file).map_err(|err| err.to_string())?;
    fs::write(jobs_path(), text).map_err(|err| err.to_string())
}

fn worker_fresh(name: &str) -> bool {
    let file = read_workers();
    let Some(seen) = file.seen.get(name) else {
        return false;
    };
    chrono::Utc::now().timestamp().saturating_sub(*seen) < 120
}

fn read_workers() -> WorkerFile {
    let Ok(text) = fs::read_to_string(workers_path()) else {
        return WorkerFile::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_workers(file: &WorkerFile) -> Result<(), String> {
    if let Some(parent) = workers_path().parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let text = serde_json::to_string_pretty(file).map_err(|err| err.to_string())?;
    fs::write(workers_path(), text).map_err(|err| err.to_string())
}

fn note_seen(name: &str) -> Result<(), String> {
    let _guard = file_lock();
    let mut file = read_workers();
    file.seen.insert(name.to_owned(), chrono::Utc::now().timestamp());
    write_workers(&file)
}

fn claim_job(machine: &str) -> Option<Job> {
    let _guard = file_lock();
    let mut file = read_jobs();
    let job = file
        .jobs
        .iter_mut()
        .find(|job| job.machine == machine && job.status == "queued")?;
    job.status = "running".into();
    let claimed = job.clone();
    let _ = write_jobs(&file);
    Some(claimed)
}

fn finish_job(id: &str, status: &str, result: &str) -> Result<(), String> {
    let _guard = file_lock();
    let mut file = read_jobs();
    if let Some(job) = file.jobs.iter_mut().find(|job| job.id == id) {
        job.status = status.to_owned();
        job.result = clip(result);
    }
    write_jobs(&file)
}

fn clip(text: &str) -> String {
    let mut end = 500.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

pub fn note_worked(cwd: &str, id: &str, text: &str) {
    let path = Path::new(cwd)
        .join(".arbos")
        .join("runtime")
        .join("worked.jsonl");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let line = json!({ "id": id, "text": text });
    let Ok(mut text) = serde_json::to_string(&line) else {
        return;
    };
    text.push('\n');
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(text.as_bytes());
    }
}

fn read_worked(project: &Path) -> Vec<Value> {
    let path = project.join(".arbos").join("runtime").join("worked.jsonl");
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value.get("id").and_then(Value::as_str).is_some())
        .collect()
}

/// Pull Worked lines from a project whose home is another machine.
pub fn mirror(host: &DocHost, workspace: &WorkspaceHost) {
    let Ok(chats) = workspace.read_chats() else {
        return;
    };
    for chat in chats {
        if chat.archived || !chat.id.ends_with("-main") {
            continue;
        }
        let Some(cwd) = chat.cwd else {
            continue;
        };
        let project = PathBuf::from(&cwd);
        if !project.join(".arbos").join("project.toml").is_file() || owns_project(&project) {
            continue;
        }
        let home = arbos_layout::home_of(&project);
        let Ok(machine) = machine_named(&home) else {
            continue;
        };
        let Ok(remote) = remote_project_path(&machine, &project) else {
            continue;
        };
        let chat_id = chat.id.clone();
        let host = host.clone();
        let lines = fetch_lines(&machine, &remote);
        let Ok(lines) = lines else {
            tracing::debug!(home = %home, "remote worked lines were not read");
            continue;
        };
        apply_lines(&host, &chat_id, &lines);
    }
}

fn fetch_lines(machine: &Machine, remote_path: &str) -> Result<Vec<Value>, String> {
    let machine = machine.clone();
    let remote_path = remote_path.to_owned();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "the request runtime did not start".to_string())?;
        runtime.block_on(async move {
            let url = format!(
                "{}/v1/lines?path={}",
                machine.url.trim_end_matches('/'),
                remote_path
            );
            let reply = authed_request(&machine, reqwest::Method::GET, &url, None).await?;
            Ok(reply
                .get("lines")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default())
        })
    })
    .join()
    .map_err(|_| "the request thread stopped".to_string())?
}

fn apply_lines(host: &DocHost, chat_id: &str, lines: &[Value]) {
    let Ok(handle) = host.open(chat_id) else {
        return;
    };
    for line in lines {
        let Some(id) = line.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(text) = line.get("text").and_then(Value::as_str) else {
            continue;
        };
        if !text.starts_with("Worked") {
            continue;
        }
        let _ = handle.write_system_message(&format!("remote-{id}"), text, crate::now_ms());
    }
}

/// Listen for home-engine calls, and poll for worker jobs when this install
/// has a home url.
pub fn spawn(doc: DocHost, workspace: WorkspaceHost, sessions: SessionsEngine) {
    let state = RemoteState {
        doc,
        workspace,
        sessions,
    };
    if let Ok(port) = std::env::var("ARBOS_REMOTE_PORT") {
        if let Ok(port) = port.trim().parse::<u16>() {
            if port != 0 {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(err) = serve(state, port).await {
                        tracing::warn!(error = %err, "remote engine port did not bind");
                    }
                });
            }
        }
    }
    if std::env::var("ARBOS_HOME_URL")
        .ok()
        .is_some_and(|url| !url.trim().is_empty())
    {
        tokio::spawn(worker_loop(state));
    }
}

async fn serve(state: RemoteState, port: u16) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|err| err.to_string())?;
    tracing::info!(port, "arbos remote engine listening");
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let state = state.clone();
        tokio::spawn(async move {
            if phone_rpc_upgrade(&stream).await {
                if let Some(service) = attached_rpc() {
                    zeron_rpc::serve_remote_ws(stream, service).await;
                } else {
                    let _ = write_unavailable(stream).await;
                }
                return;
            }
            let _ = handle_connection(state, stream).await;
        });
    }
}

async fn phone_rpc_upgrade(stream: &tokio::net::TcpStream) -> bool {
    let Some(headers) = peek_headers(stream).await else {
        return false;
    };
    let mut lines = headers.lines();
    let request = lines.next().unwrap_or("");
    let path_ok = request.starts_with("GET /v1/rpc ")
        || request.starts_with("GET /v1/rpc?")
        || request == "GET /v1/rpc";
    if !path_ok {
        return false;
    }
    headers.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("upgrade:") && lower.contains("websocket")
    })
}

async fn peek_headers(stream: &tokio::net::TcpStream) -> Option<String> {
    let mut buf = [0u8; 8192];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let n = stream.peek(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        if let Some(end) = buf[..n]
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        {
            return String::from_utf8(buf[..end + 4].to_vec()).ok();
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn write_unavailable(mut stream: tokio::net::TcpStream) -> Result<(), String> {
    let body = br#"{"error":"the engine is not ready"}"#;
    let head = format!(
        "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .await
        .map_err(|err| err.to_string())?;
    stream
        .write_all(body)
        .await
        .map_err(|err| err.to_string())?;
    Ok(())
}

async fn handle_connection(
    state: RemoteState,
    mut stream: tokio::net::TcpStream,
) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 64 * 1024];
    let mut collected = Vec::new();
    loop {
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|err| err.to_string())?;
        if n == 0 {
            break;
        }
        collected.extend_from_slice(&buf[..n]);
        if collected.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if collected.len() > 1024 * 1024 {
            break;
        }
    }
    let header_end = collected
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or(collected.len());
    let header = String::from_utf8_lossy(&collected[..header_end]).to_string();
    let mut body = if header_end + 4 <= collected.len() {
        collected[header_end + 4..].to_vec()
    } else {
        Vec::new()
    };
    let length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0)
        .min(8 * 1024 * 1024);
    while body.len() < length {
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|err| err.to_string())?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    body.truncate(length);
    let mut lines = header.lines();
    let request = lines.next().unwrap_or("");
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let (route, query) = target.split_once('?').unwrap_or((target, ""));
    let reply = dispatch(&state, method, route, query, &body);
    let (status, bytes, content_type) = match reply {
        Ok(Reply::Json(value)) => (
            200,
            serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec()),
            "application/json",
        ),
        Ok(Reply::Bytes(bytes)) => (200, bytes, "application/gzip"),
        Err(err) => (
            400,
            serde_json::to_vec(&json!({ "error": safe_remote_error(&err) })).unwrap_or_default(),
            "application/json",
        ),
    };
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        if status == 200 { "OK" } else { "Bad Request" },
        bytes.len()
    );
    stream
        .write_all(head.as_bytes())
        .await
        .map_err(|err| err.to_string())?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|err| err.to_string())?;
    Ok(())
}

enum Reply {
    Json(Value),
    Bytes(Vec<u8>),
}

fn dispatch(
    state: &RemoteState,
    method: &str,
    route: &str,
    query: &str,
    body: &[u8],
) -> Result<Reply, String> {
    let json_body = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body).unwrap_or(Value::Null)
    };
    match (method, route) {
        ("GET", "/v1/health") => Ok(Reply::Json(json!({
            "ok": true,
            "machine": this_machine(),
        }))),
        ("POST", "/v1/adopt") => Ok(Reply::Json(adopt_here(state, &json_body)?)),
        ("GET", "/v1/lines") => {
            let path = project_from_query(query)?;
            Ok(Reply::Json(json!({ "lines": read_worked(&path) })))
        }
        ("GET", "/v1/archive") => {
            let path = project_from_query(query)?;
            Ok(Reply::Bytes(archive_project(&path)?))
        }
        ("POST", "/v1/register") => {
            let name = text(&json_body, "name").ok_or_else(|| "name the machine".to_string())?;
            note_seen(&name)?;
            Ok(Reply::Json(json!({ "ok": true })))
        }
        ("GET", "/v1/jobs") => {
            let machine = query_value(query, "machine").unwrap_or_default();
            let job = claim_job(&machine);
            Ok(Reply::Json(json!({ "job": job })))
        }
        ("POST", "/v1/jobs/done") => {
            let id = text(&json_body, "id").ok_or_else(|| "the job id is missing".to_string())?;
            let status = text(&json_body, "status").unwrap_or_else(|| "done".into());
            let result = text(&json_body, "result").unwrap_or_default();
            finish_job(&id, &status, &result)?;
            Ok(Reply::Json(json!({ "ok": true })))
        }
        ("POST", "/v1/spawn") => Ok(Reply::Json(spawn_here(&json_body)?)),
        _ => Err("unknown route".into()),
    }
}

fn adopt_here(state: &RemoteState, body: &Value) -> Result<Value, String> {
    let path = text(body, "path").ok_or_else(|| "the project path is missing".to_string())?;
    let name = text(body, "name").unwrap_or_else(|| "project".into());
    let project = PathBuf::from(&path);
    arbos_layout::ensure(&project, Some(&name))?;
    if !owns_project(&project) {
        return Err("this install is not the project home".into());
    }
    let chat_id = existing_main(state, &path).unwrap_or_else(|| {
        let id = arbos_layout::project_id(&project).unwrap_or_else(crate::new_id);
        format!("{id}-main")
    });
    if state
        .workspace
        .chat(&chat_id)
        .map_err(|err| err.to_string())?
        .is_none()
    {
        let config = fx_config();
        state
            .workspace
            .create_chat(
                &chat_id,
                None,
                Some(state.workspace.device_id()),
                Some(config),
                Some(path.clone()),
            )
            .map_err(|err| err.to_string())?;
        state
            .workspace
            .rename_chat(&chat_id, "Main")
            .map_err(|err| err.to_string())?;
    }
    arbos_layout::claim_main(&project, &chat_id)?;
    Ok(json!({ "ok": true, "chatId": chat_id }))
}

fn existing_main(state: &RemoteState, cwd: &str) -> Option<String> {
    let chats = state.workspace.read_chats().ok()?;
    chats.into_iter().find_map(|chat| {
        if chat.archived || !chat.id.ends_with("-main") {
            return None;
        }
        (chat.cwd.as_deref() == Some(cwd)).then_some(chat.id)
    })
}

fn spawn_here(body: &Value) -> Result<Value, String> {
    let path = text(body, "path").ok_or_else(|| "the project path is missing".to_string())?;
    let machine = text(body, "machine").ok_or_else(|| "name the machine".to_string())?;
    let brief = text(body, "brief").ok_or_else(|| "the worker needs a brief".to_string())?;
    let name = text(body, "name").unwrap_or_default();
    let project = PathBuf::from(&path);
    if !project.join(".arbos").join("project.toml").is_file() {
        return Err("that folder is not an Arbos project".into());
    }
    if !worker_fresh(&machine) {
        return Err("machine is not registered".into());
    }
    let worker = arbos_layout::reserve_worker(&project, &name, &brief, "glm-5.3", Some(&machine))?;
    enqueue_local(&machine, &project, &worker.id, &brief)?;
    Ok(json!({
        "name": worker.id,
        "machine": machine,
        "status": "working",
    }))
}

fn project_from_query(query: &str) -> Result<PathBuf, String> {
    let path = query_value(query, "path").ok_or_else(|| "the project path is missing".to_string())?;
    let path = PathBuf::from(path);
    if !path.is_absolute() || !path.join(".arbos").join("project.toml").is_file() {
        return Err("that folder is not an Arbos project".into());
    }
    Ok(path)
}

fn query_value(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=')?;
        if name == key {
            return Some(value.to_owned());
        }
    }
    None
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn archive_project(project: &Path) -> Result<Vec<u8>, String> {
    let output = Command::new("tar")
        .args(["--exclude=.arbos/runtime", "-czf", "-", "-C"])
        .arg(project)
        .arg(".arbos")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err("the project archive failed".into());
    }
    Ok(output.stdout)
}

fn fx_config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Fx,
        model: Some("glm-5.3".into()),
        reasoning: None,
        model_options: serde_json::Map::new(),
        sandbox: SandboxLevel::DangerFullAccess,
    }
}

async fn worker_loop(state: RemoteState) {
    let home_url = std::env::var("ARBOS_HOME_URL").unwrap_or_default();
    let home_url = home_url.trim().trim_end_matches('/').to_owned();
    if home_url.is_empty() {
        return;
    }
    let credential = std::env::var("ARBOS_HOME_CREDENTIAL").unwrap_or_default();
    let machine = Machine {
        name: this_machine(),
        url: home_url,
        credential,
        ssh: String::new(),
        dir: String::new(),
        key: String::new(),
    };
    loop {
        if let Err(err) = register_worker(&machine).await {
            tracing::warn!(error = %err, "worker register failed");
        }
        match claim_remote(&machine).await {
            Ok(Some(job)) => {
                let state = state.clone();
                let outcome = run_remote_job(&state, &machine, job).await;
                if let Err(err) = outcome {
                    tracing::warn!(error = %err, "worker job failed");
                }
            }
            Ok(None) => {}
            Err(err) => tracing::warn!(error = %err, "worker poll failed"),
        }
        tokio::time::sleep(JOBS_WAIT).await;
    }
}

async fn register_worker(machine: &Machine) -> Result<(), String> {
    let url = format!("{}/v1/register", machine.url);
    authed_request(
        machine,
        reqwest::Method::POST,
        &url,
        Some(json!({ "name": this_machine() })),
    )
    .await
    .map(|_| ())
}

async fn claim_remote(machine: &Machine) -> Result<Option<Job>, String> {
    let url = format!(
        "{}/v1/jobs?machine={}",
        machine.url,
        this_machine()
    );
    let reply = authed_request(machine, reqwest::Method::GET, &url, None).await?;
    let Some(job) = reply.get("job") else {
        return Ok(None);
    };
    if job.is_null() {
        return Ok(None);
    }
    serde_json::from_value(job.clone()).map(Some).map_err(|_| "the job was not readable".to_string())
}

async fn run_remote_job(state: &RemoteState, home: &Machine, job: Job) -> Result<(), String> {
    let url = format!(
        "{}/v1/archive?path={}",
        home.url,
        job.project
    );
    let client = http_client(Duration::from_secs(120))?;
    let mut request = client.get(&url);
    if home.url.starts_with("https://") {
        let (id, secret) = access_headers(&home.credential)?;
        request = request
            .header("CF-Access-Client-Id", id)
            .header("CF-Access-Client-Secret", secret);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "the project archive was not fetched".to_string())?;
    if !response.status().is_success() {
        let _ = report_job(home, &job.id, "error", "the project archive was not fetched").await;
        return Err("the project archive was not fetched".into());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "the project archive was not fetched".to_string())?;
    let job_id = job.id.clone();
    let home = home.clone();
    let state_for_prep = state.clone();
    let job_for_prep = job.clone();
    let prepared = tokio::task::spawn_blocking(move || prepare_job(&state_for_prep, &job_for_prep, &bytes))
        .await
        .map_err(|_| "the worker thread stopped".to_string())?;
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(err) => {
            let _ = report_job(&home, &job_id, "error", &err).await;
            return Err(err);
        }
    };
    // Queue on the runtime. A blocking thread has no runtime handle, and the
    // command drain does not start from there.
    let queued = state.doc.queue_command(
        &prepared.chat_id,
        zeron_doc::SessionCommandPayload::Run {
            request: RunRequest {
                prompt: prepared.brief.clone(),
                harness: Some(HarnessId::Fx),
                model: Some("glm-5.3".into()),
                reasoning: None,
                model_options: serde_json::Map::new(),
                cwd: prepared.project.display().to_string(),
                sandbox: SandboxLevel::DangerFullAccess,
                auto_approve: true,
                resume: None,
                attachments: Vec::new(),
                worktree: None,
                mcp: None,
            },
            message_id: crate::new_id(),
        },
    );
    if let Err(err) = queued {
        let err = err.to_string();
        let _ = report_job(&home, &job_id, "error", &err).await;
        return Err(err);
    }
    match wait_for_worker(state, &prepared).await {
        Ok(result) => {
            report_job(&home, &job_id, "done", &result).await?;
            Ok(())
        }
        Err(err) => {
            let _ = report_job(&home, &job_id, "error", &err).await;
            Err(err)
        }
    }
}

struct PreparedJob {
    chat_id: String,
    project: PathBuf,
    brief: String,
    worker_id: String,
}

async fn report_job(home: &Machine, id: &str, status: &str, result: &str) -> Result<(), String> {
    let url = format!("{}/v1/jobs/done", home.url);
    authed_request(
        home,
        reqwest::Method::POST,
        &url,
        Some(json!({ "id": id, "status": status, "result": result })),
    )
    .await
    .map(|_| ())
}

fn prepare_job(state: &RemoteState, job: &Job, archive: &[u8]) -> Result<PreparedJob, String> {
    let root = worker_root();
    let folder = Path::new(&job.project)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let project = root.join(folder);
    fs::create_dir_all(&project).map_err(|err| err.to_string())?;
    let archive_path = project.join(".arbos-archive.tar.gz");
    fs::write(&archive_path, archive).map_err(|err| err.to_string())?;
    let extracted = Command::new("tar")
        .args(["-xzf"])
        .arg(&archive_path)
        .arg("-C")
        .arg(&project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|err| err.to_string())?;
    let _ = fs::remove_file(&archive_path);
    if !extracted.success() {
        return Err("the project archive did not extract".into());
    }
    arbos_layout::ensure(&project, None)?;
    let worker = arbos_layout::reserve_worker(
        &project,
        &job.name,
        &job.brief,
        "glm-5.3",
        Some("local"),
    )?;
    let chat_id = format!(
        "{}-w-{}",
        arbos_layout::project_id(&project).unwrap_or_else(crate::new_id),
        worker.id
    );
    if state
        .workspace
        .chat(&chat_id)
        .map_err(|err| err.to_string())?
        .is_none()
    {
        state
            .workspace
            .create_chat(
                &chat_id,
                None,
                Some(state.workspace.device_id()),
                Some(fx_config()),
                Some(project.display().to_string()),
            )
            .map_err(|err| err.to_string())?;
        state
            .workspace
            .rename_chat(&chat_id, &worker.id)
            .map_err(|err| err.to_string())?;
    }
    arbos_layout::bind_worker_chat(&project, &worker.id, &chat_id)?;
    Ok(PreparedJob {
        chat_id,
        project,
        brief: job.brief.clone(),
        worker_id: worker.id,
    })
}

async fn wait_for_worker(state: &RemoteState, prepared: &PreparedJob) -> Result<String, String> {
    // A turn can read context and retry a dropped request. Three minutes
    // marked a finished worker as failed. Ten minutes covers that.
    for _ in 0..300 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let project = prepared.project.clone();
        let worker_id = prepared.worker_id.clone();
        let status = tokio::task::spawn_blocking(move || worker_status(&project, &worker_id))
            .await
            .map_err(|_| "the worker thread stopped".to_string())?;
        if let Some(result) = status? {
            return Ok(result);
        }
        let _ = state.sessions.is_working(&prepared.chat_id);
    }
    Err("the worker did not finish".into())
}

fn worker_status(project: &Path, worker_id: &str) -> Result<Option<String>, String> {
    let agent = arbos_layout::worker_result(project, worker_id)?;
    if !arbos_layout::status_is_live(&agent.status) && !agent.status.is_empty() {
        return Ok(Some(if agent.result.is_empty() {
            agent.status
        } else {
            format!("{} {}", agent.status, agent.result)
        }));
    }
    Ok(None)
}

fn worker_root() -> PathBuf {
    if let Ok(dir) = std::env::var("ARBOS_WORKER_ROOT") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    config_dir().join("worker-projects")
}
