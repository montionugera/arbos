//! Relay-backed source: HTTP for the registry (`/pair`, `/projects`, `/blob`)
//! and one WebSocket per room on `arbos-relay` (`crdt-sync.md` §7–§9).
//! Frames come from `arbos_core::wire`; the chat doc is the same Loro schema
//! `arbos-core::doc` writes on the box side. Each project also has a project
//! doc room (`{project}/project`) that carries `threads`, `status`,
//! `settings` and `memory`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use arbos_core::wire::{
    Ack, BoxState as WireBox, ErrorHeader, Frame, FrameType, Hello, RowHeader, RowsReq,
    State as WireState, StatusHeader, WakeHeader, PROJECT_DOC_CHAT_ID,
};
use arbos_core::{wire, PROTO_VERSION};
use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep, timeout, Instant};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::credentials::{self, DeviceCredentials};
use super::doc::ChatDoc;
use super::{ClientCommand, SyncEvent, SyncSource};
use crate::model::{
    new_id, now_ms, Attachment, BoxState, ChannelBinding, ChannelPairing, ChannelPlatform,
    ChannelsView, Chat, Connector, ConnectorProvider, ConnectorsView, Effort, Id, Link,
    LocalDevice, LocalView, Machine, MachineState, Message, MessageStatus, Part, Project,
    ProjectDocView, Role, Routine, RoutineRun, Skill, ThreadInfo,
};

const HELLO_DEADLINE: Duration = Duration::from_secs(15);
const PRESENCE_EVERY: Duration = Duration::from_secs(15);
const REGISTRY_POLL: Duration = Duration::from_secs(15);
const BACKOFF_MIN: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(16);
const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(30);

pub struct RelaySource {
    http_url: String,
    ws_url: String,
}

impl RelaySource {
    /// Accepts `http://`, `https://`, `ws://` or `wss://`; derives the other.
    pub fn from_env(url: String) -> Self {
        let url = url.trim().trim_end_matches('/').to_string();
        let (http_url, ws_url) = if let Some(rest) = url.strip_prefix("ws://") {
            (format!("http://{rest}"), url.clone())
        } else if let Some(rest) = url.strip_prefix("wss://") {
            (format!("https://{rest}"), url.clone())
        } else if let Some(rest) = url.strip_prefix("https://") {
            (url.clone(), format!("wss://{rest}"))
        } else if let Some(rest) = url.strip_prefix("http://") {
            (url.clone(), format!("ws://{rest}"))
        } else {
            (format!("http://{url}"), format!("ws://{url}"))
        };
        Self { http_url, ws_url }
    }
}

enum RoomCmd {
    Wake(&'static str),
}

struct Room {
    cmds: mpsc::UnboundedSender<RoomCmd>,
    task: JoinHandle<()>,
}

struct Shared {
    http: reqwest::Client,
    http_url: String,
    ws_url: String,
    creds: RwLock<Option<DeviceCredentials>>,
    /// Chat docs keyed by chat id; project docs keyed by `proj:{project_id}`.
    docs: Mutex<HashMap<Id, ChatDoc>>,
    rooms: Mutex<HashMap<Id, Room>>,
    projects: Mutex<Vec<Project>>,
    chats: Mutex<Vec<Chat>>,
    /// Client-minted message ids whose push the relay has not `ACK`ed (S3).
    unacked: Mutex<HashSet<Id>>,
    events: mpsc::UnboundedSender<SyncEvent>,
    subscriptions: Mutex<Vec<loro::Subscription>>,
    browser_watch: Mutex<HashMap<Id, JoinHandle<()>>>,
    machine: Mutex<Machine>,
}

fn project_doc_key(project_id: &str) -> String {
    format!("proj:{project_id}")
}

impl Shared {
    fn token(&self) -> String {
        self.creds
            .read()
            .unwrap()
            .as_ref()
            .map(|c| c.device_token.clone())
            .unwrap_or_default()
    }

    fn device_id(&self) -> String {
        self.creds
            .read()
            .unwrap()
            .as_ref()
            .map(|c| c.device_id.clone())
            .unwrap_or_else(|| "mac:unpaired".into())
    }

    fn notice(&self, text: impl Into<String>) {
        let _ = self.events.send(SyncEvent::Notice(text.into()));
    }

    async fn get_json(&self, path: &str) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{}{}", self.http_url, path))
            .bearer_auth(self.token())
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("GET {path}: HTTP {status} {body}"));
        }
        serde_json::from_str(&body).with_context(|| format!("GET {path}: bad JSON"))
    }

    async fn post_json(&self, path: &str, body: Value) -> Result<Value> {
        let resp = self
            .http
            .post(format!("{}{}", self.http_url, path))
            .bearer_auth(self.token())
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("POST {path}: HTTP {status} {text}"));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }
}

impl SyncSource for RelaySource {
    fn name(&self) -> &'static str {
        "relay"
    }

    fn run(
        self: Box<Self>,
        events: mpsc::UnboundedSender<SyncEvent>,
        mut commands: mpsc::UnboundedReceiver<ClientCommand>,
    ) -> BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            let shared = Arc::new(Shared {
                http: reqwest::Client::builder()
                    .timeout(Duration::from_secs(30))
                    .build()
                    .context("http client")?,
                http_url: self.http_url.clone(),
                ws_url: self.ws_url.clone(),
                creds: RwLock::new(credentials::load(&self.http_url)),
                docs: Mutex::new(HashMap::new()),
                rooms: Mutex::new(HashMap::new()),
                projects: Mutex::new(Vec::new()),
                chats: Mutex::new(Vec::new()),
                unacked: Mutex::new(HashSet::new()),
                events: events.clone(),
                subscriptions: Mutex::new(Vec::new()),
                browser_watch: Mutex::new(HashMap::new()),
                machine: Mutex::new(Machine::default()),
            });
            let _ = events.send(SyncEvent::Link(Link::Connecting));

            if shared.creds.read().unwrap().is_some() {
                start_after_pairing(&shared);
            } else {
                let _ = events.send(SyncEvent::NeedsPairing {
                    relay_url: self.http_url.clone(),
                });
            }

            while let Some(cmd) = commands.recv().await {
                handle_command(&shared, cmd);
            }
            Ok(())
        })
    }
}

/// `GET /skills` → `SyncEvent::Skills` (F5).
async fn refresh_skills(shared: &Arc<Shared>) {
    match shared.get_json("/skills").await {
        Ok(v) => {
            let skills: Vec<Skill> = v
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| serde_json::from_value::<Skill>(s.clone()).ok())
                        .collect()
                })
                .unwrap_or_default();
            let _ = shared.events.send(SyncEvent::Skills(skills));
        }
        Err(err) => shared.notice(format!("Skills: {err:#}")),
    }
}

fn start_after_pairing(shared: &Arc<Shared>) {
    let skills_shared = shared.clone();
    tokio::spawn(async move { refresh_skills(&skills_shared).await });
    let models_shared = shared.clone();
    tokio::spawn(async move {
        if let Ok(v) = models_shared.get_json("/proxy/v1/models").await {
            let mut ids: Vec<String> = v
                .get("data")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m.get("id").and_then(Value::as_str))
                        .filter(|id| !id.contains(':'))
                        .filter(|id| {
                            id.starts_with("anthropic/")
                                || id.starts_with("openai/")
                                || id.starts_with("google/")
                                || id.starts_with("x-ai/")
                        })
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            ids.sort();
            ids.dedup();
            if !ids.is_empty() {
                let _ = models_shared.events.send(SyncEvent::Models(ids));
            }
        }
    });
    {
        let shared = shared.clone();
        tokio::spawn(async move {
            refresh_connectors(&shared).await;
        });
    }
    let shared = shared.clone();
    tokio::spawn(async move {
        let mut poll = interval(REGISTRY_POLL);
        loop {
            poll.tick().await;
            if let Err(err) = refresh_registry(&shared).await {
                let text = format!("{err:#}");
                if text.contains("HTTP 401") {
                    // Token revoked or relay re-initialised: pair again.
                    *shared.creds.write().unwrap() = None;
                    let _ = shared.events.send(SyncEvent::NeedsPairing {
                        relay_url: shared.http_url.clone(),
                    });
                    return;
                }
                shared.notice(format!("Relay: {text}"));
                let _ = shared.events.send(SyncEvent::Link(Link::Reconnecting));
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Registry (HTTP)
// ---------------------------------------------------------------------------

fn effort_from(v: &Value) -> Effort {
    match v.as_str().unwrap_or("") {
        "high" | "xhigh" | "max" => Effort::High,
        "medium" => Effort::Medium,
        _ => Effort::Low,
    }
}

fn effort_to(e: Effort) -> &'static str {
    match e {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
    }
}

fn box_state_from(state: &str, box_state: &str) -> BoxState {
    match state {
        "awake" => BoxState::Awake,
        "waking" => BoxState::Waking,
        "hibernated_light" => BoxState::HibernatedLight,
        "hibernated_deep" => BoxState::HibernatedDeep,
        "stopped_by_user" => BoxState::StoppedByUser,
        "error" => BoxState::Error,
        _ => match box_state {
            "awake" => BoxState::Awake,
            "waking" => BoxState::Waking,
            "error" => BoxState::Error,
            _ => BoxState::HibernatedLight,
        },
    }
}

fn parse_chat(v: &Value) -> Option<Chat> {
    Some(Chat {
        id: v.get("id")?.as_str()?.to_string(),
        project_id: v.get("project_id")?.as_str()?.to_string(),
        title: v
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("New chat")
            .to_string(),
        is_pinned: v.get("is_pinned").and_then(Value::as_bool).unwrap_or(false),
        is_system: false,
        unread: false,
        last_message_at: v.get("created_at").and_then(Value::as_i64).unwrap_or(0) as u64,
    })
}

/// `GET /projects` then `GET /projects/{id}` for chats, models and spend.
async fn refresh_registry(shared: &Arc<Shared>) -> Result<()> {
    let list = shared.get_json("/projects").await?;
    let items = list.as_array().cloned().unwrap_or_default();
    let mut projects: Vec<Project> = Vec::new();
    let mut all_chats: Vec<Chat> = Vec::new();
    for item in items {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let detail = shared.get_json(&format!("/projects/{id}")).await?;
        let p = detail.get("project").cloned().unwrap_or(item.clone());
        let models = p.get("models").cloned().unwrap_or(Value::Null);
        let talking = models.get("talking_model").cloned().unwrap_or(Value::Null);
        let state = box_state_from(
            p.get("state").and_then(Value::as_str).unwrap_or(""),
            detail
                .get("box")
                .or(item.get("box"))
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        let previous = shared
            .projects
            .lock()
            .unwrap()
            .iter()
            .find(|x| x.id == id)
            .cloned();
        projects.push(Project {
            id: id.to_string(),
            name: p
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Project")
                .to_string(),
            icon: p
                .get("icon")
                .and_then(Value::as_str)
                .unwrap_or("Ω")
                .to_string(),
            color: "#8b7cf6".into(),
            state,
            state_detail: previous.as_ref().and_then(|x| x.state_detail.clone()),
            wake_progress: None,
            model: talking
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| previous.as_ref().map(|x| x.model.clone()))
                .unwrap_or_else(|| "anthropic/claude-haiku-5.5".into()),
            effort: effort_from(talking.get("effort").unwrap_or(&Value::Null)),
            spend_today_usd: detail
                .get("spend_today")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            spend_cap_usd: p
                .get("spend_cap_daily")
                .and_then(Value::as_f64)
                .unwrap_or(25.0),
        });
        let chats: Vec<Chat> = detail
            .get("chats")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(parse_chat).collect())
            .unwrap_or_default();
        all_chats.extend(chats);
    }

    // Keep local-only tabs (minted, not yet created) and unread flags.
    {
        let mut local = shared.chats.lock().unwrap();
        let mut merged: Vec<Chat> = Vec::new();
        for c in &all_chats {
            let mut c = c.clone();
            if let Some(prev) = local.iter().find(|x| x.id == c.id) {
                c.unread = prev.unread;
                c.last_message_at = c.last_message_at.max(prev.last_message_at);
                if c.title == "New chat" && prev.title != "New chat" {
                    c.title = prev.title.clone();
                }
            }
            merged.push(c);
        }
        *local = merged;
    }
    {
        let spend: f64 = projects.iter().map(|p| p.spend_today_usd).sum();
        let cap = projects.first().map(|p| p.spend_cap_usd).unwrap_or(25.0);
        let mut m = shared.machine.lock().unwrap();
        m.spend_today_usd = spend;
        m.spend_cap_usd = cap;
        if m.spec.is_none() {
            m.spec = Some("arbos-std · 2 vCPU · 4 GiB · 10 GiB".into());
        }
        let snapshot = m.clone();
        drop(m);
        let _ = shared.events.send(SyncEvent::Machine(snapshot));
    }
    *shared.projects.lock().unwrap() = projects.clone();
    let _ = shared.events.send(SyncEvent::Projects(projects.clone()));
    emit_chats(shared);
    for p in &projects {
        ensure_room(shared, &p.id, &project_doc_key(&p.id), PROJECT_DOC_CHAT_ID);
    }
    for c in &all_chats {
        ensure_room(shared, &c.project_id, &c.id, &c.id);
    }
    Ok(())
}

fn emit_chats(shared: &Shared) {
    let chats = shared.chats.lock().unwrap().clone();
    let projects: Vec<Id> = shared
        .projects
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.id.clone())
        .collect();
    for project_id in projects {
        let mut mine: Vec<Chat> = chats
            .iter()
            .filter(|c| c.project_id == project_id)
            .cloned()
            .collect();
        mine.sort_by_key(|c| (!c.is_pinned, c.last_message_at));
        let _ = shared.events.send(SyncEvent::Chats {
            project_id,
            chats: mine,
        });
    }
}

// ---------------------------------------------------------------------------
// Rooms (WebSocket)
// ---------------------------------------------------------------------------

fn apply_pending(shared: &Shared, message: &mut Message) {
    if message.role == Role::User {
        message.pending = shared.unacked.lock().unwrap().contains(&message.id);
    }
}

fn emit_snapshot(shared: &Shared, chat_id: &str) {
    let docs = shared.docs.lock().unwrap();
    let Some(doc) = docs.get(chat_id) else { return };
    let mut messages = doc.read_all();
    for m in &mut messages {
        apply_pending(shared, m);
    }
    let _ = shared.events.send(SyncEvent::ChatSnapshot {
        chat_id: chat_id.to_string(),
        messages,
    });
}

fn emit_project_doc(shared: &Shared, project_id: &str) {
    let docs = shared.docs.lock().unwrap();
    let Some(doc) = docs.get(&project_doc_key(project_id)) else {
        return;
    };
    let to_json = |v: loro::LoroValue| serde_json::to_value(v).unwrap_or(Value::Null);
    let threads_v = to_json(doc.doc.get_map("threads").get_deep_value());
    let mut threads: Vec<ThreadInfo> = threads_v
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(id, v)| {
                    let mut t: ThreadInfo = serde_json::from_value(v.clone()).ok()?;
                    t.id = id.clone();
                    Some(t)
                })
                .collect()
        })
        .unwrap_or_default();
    threads.sort_by_key(|t| std::cmp::Reverse(t.last_turn_at.unwrap_or(0)));
    let memory = to_json(doc.doc.get_map("memory").get_deep_value());
    let view = ProjectDocView {
        threads,
        status: to_json(doc.doc.get_map("status").get_deep_value()),
        settings: to_json(doc.doc.get_map("settings").get_deep_value()),
        memory_t1: memory
            .get("t1")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        model_note: view_settings_note(&doc.doc),
    };
    let _ = shared.events.send(SyncEvent::ProjectDoc {
        project_id: project_id.to_string(),
        view,
    });
}

fn view_settings_note(doc: &loro::LoroDoc) -> String {
    match doc.get_map("settings").get("modelNote") {
        Some(loro::ValueOrContainer::Value(loro::LoroValue::String(s))) => s.to_string(),
        Some(_) | None => String::new(),
    }
}

/// Open a room once; `key` is the docs map key, `room_chat` the URL segment.
fn ensure_room(shared: &Arc<Shared>, project_id: &str, key: &str, room_chat: &str) {
    if shared.rooms.lock().unwrap().contains_key(key) {
        return;
    }
    let doc = ChatDoc::empty(key);
    let (local_tx, local_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (change_tx, change_rx) = mpsc::unbounded_channel::<super::doc::Touched>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<RoomCmd>();
    let is_project_doc = room_chat == PROJECT_DOC_CHAT_ID;

    let sub = doc.doc.subscribe_root(Arc::new(move |event| {
        let touched = ChatDoc::touched(&event);
        if is_project_doc || touched.list_changed || !touched.messages.is_empty() {
            let _ = change_tx.send(touched);
        }
    }));
    let local_sub = doc
        .doc
        .subscribe_local_update(Box::new(move |bytes| local_tx.send(bytes.clone()).is_ok()));
    {
        let mut subs = shared.subscriptions.lock().unwrap();
        subs.push(sub);
        subs.push(local_sub);
    }
    shared.docs.lock().unwrap().insert(key.to_string(), doc);

    let shared2 = shared.clone();
    let key2 = key.to_string();
    let project2 = project_id.to_string();
    tokio::spawn(async move {
        forward_changes(shared2, project2, key2, is_project_doc, change_rx).await;
    });

    let shared3 = shared.clone();
    let project = project_id.to_string();
    let key3 = key.to_string();
    let room_chat = room_chat.to_string();
    let task = tokio::spawn(async move {
        room_loop(shared3, project, key3, room_chat, local_rx, cmd_rx).await;
    });
    shared
        .rooms
        .lock()
        .unwrap()
        .insert(key.to_string(), Room { cmds: cmd_tx, task });
}

async fn forward_changes(
    shared: Arc<Shared>,
    project_id: Id,
    key: Id,
    is_project_doc: bool,
    mut rx: mpsc::UnboundedReceiver<super::doc::Touched>,
) {
    while let Some(touched) = rx.recv().await {
        if is_project_doc {
            emit_project_doc(&shared, &project_id);
            apply_project_doc_status(&shared, &project_id);
            continue;
        }
        if touched.list_changed {
            emit_snapshot(&shared, &key);
            mark_activity(&shared, &key);
            continue;
        }
        let docs = shared.docs.lock().unwrap();
        let Some(doc) = docs.get(&key) else { continue };
        for ix in touched.messages {
            if let Some(mut message) = doc.read_message(ix) {
                apply_pending(&shared, &mut message);
                let _ = shared.events.send(SyncEvent::MessageUpsert {
                    chat_id: key.clone(),
                    message: Box::new(message),
                });
            }
        }
    }
}

fn mark_activity(shared: &Shared, chat_id: &str) {
    let mut chats = shared.chats.lock().unwrap();
    if let Some(c) = chats.iter_mut().find(|c| c.id == chat_id) {
        c.last_message_at = now_ms();
    }
}

fn box_state_from_word(word: &str) -> Option<BoxState> {
    Some(match word {
        "awake" => BoxState::Awake,
        "waking" => BoxState::Waking,
        "error" => BoxState::Error,
        "asleep" | "hibernated_light" => BoxState::HibernatedLight,
        "hibernated_deep" => BoxState::HibernatedDeep,
        "stopped" | "stopped_by_user" => BoxState::StoppedByUser,
        _ => return None,
    })
}

fn machine_state_from_word(word: &str) -> MachineState {
    match word {
        "awake" => MachineState::Awake,
        "waking" | "starting" | "creating" => MachineState::Waking,
        "asleep" | "hibernated_light" | "hibernated_deep" => MachineState::Asleep,
        "stopped" | "stopped_by_user" => MachineState::Stopped,
        "error" => MachineState::Error,
        _ => MachineState::Unknown,
    }
}

/// Project doc `status`: `box {state, since}` is the user's machine (mirrored
/// into every project doc); `workspace {state}` is this project. Older docs
/// carry `box` as a bare word — read that as both.
fn apply_project_doc_status(shared: &Shared, project_id: &str) {
    let status = {
        let docs = shared.docs.lock().unwrap();
        let Some(doc) = docs.get(&project_doc_key(project_id)) else {
            return;
        };
        serde_json::to_value(doc.doc.get_map("status").get_deep_value()).unwrap_or(Value::Null)
    };
    match status.get("box") {
        Some(Value::Object(b)) => {
            let word = b.get("state").and_then(Value::as_str).unwrap_or("");
            let since = b.get("since").and_then(Value::as_u64);
            let detail = b.get("detail").and_then(Value::as_str).map(str::to_string);
            set_machine(shared, machine_state_from_word(word), since, detail);
        }
        Some(Value::String(word)) => {
            // Legacy shape: one box per project. Treat it as the machine too.
            set_machine(shared, machine_state_from_word(word), None, None);
            if let Some(state) = box_state_from_word(word) {
                set_project_state(shared, project_id, state, None);
            }
        }
        Some(_) | None => {}
    }
    if let Some(ws) = status.get("workspace").and_then(Value::as_object) {
        if let Some(state) = ws
            .get("state")
            .and_then(Value::as_str)
            .and_then(box_state_from_word)
        {
            set_project_state(shared, project_id, state, None);
        }
    }
    if let Some(spend) = status.get("spendTodayUsd").and_then(Value::as_f64) {
        let mut m = shared.machine.lock().unwrap();
        m.spend_today_usd = spend;
    }
}

fn set_machine(shared: &Shared, state: MachineState, since: Option<u64>, detail: Option<String>) {
    let snapshot = {
        let mut m = shared.machine.lock().unwrap();
        let changed = m.state != state || m.detail != detail;
        if m.state != state {
            m.since = since.or(Some(now_ms()));
        } else if since.is_some() {
            m.since = since;
        }
        m.state = state;
        m.detail = detail;
        m.progress = None;
        if !changed {
            return;
        }
        m.clone()
    };
    let _ = shared.events.send(SyncEvent::Machine(snapshot));
}

fn set_project_state(shared: &Shared, project_id: &str, state: BoxState, detail: Option<String>) {
    {
        let mut projects = shared.projects.lock().unwrap();
        if let Some(p) = projects.iter_mut().find(|p| p.id == project_id) {
            if p.state == state && p.state_detail == detail {
                return;
            }
            p.state = state;
            p.state_detail = detail.clone();
            p.wake_progress = None;
        }
    }
    let _ = shared.events.send(SyncEvent::ProjectStatus {
        project_id: project_id.to_string(),
        state,
        detail,
        progress: None,
    });
}

/// Dial, handshake, backfill, then stream — forever, with backoff (§8.6).
async fn room_loop(
    shared: Arc<Shared>,
    project_id: Id,
    key: Id,
    room_chat: String,
    mut local_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut cmd_rx: mpsc::UnboundedReceiver<RoomCmd>,
) {
    let mut cursor: i64 = 0;
    let mut backoff = BACKOFF_MIN;
    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    loop {
        let connected_at = Instant::now();
        let outcome = run_session(
            &shared,
            &project_id,
            &key,
            &room_chat,
            &mut cursor,
            &mut pending,
            &mut local_rx,
            &mut cmd_rx,
        )
        .await;
        if let Err(err) = outcome {
            eprintln!("relay room {project_id}/{room_chat}: {err:#}");
        }
        let _ = shared.events.send(SyncEvent::Link(Link::Reconnecting));
        if connected_at.elapsed() > BACKOFF_RESET_AFTER {
            backoff = BACKOFF_MIN;
        }
        sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    shared: &Arc<Shared>,
    project_id: &str,
    key: &str,
    room_chat: &str,
    cursor: &mut i64,
    pending: &mut Vec<(String, Vec<u8>)>,
    local_rx: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    cmd_rx: &mut mpsc::UnboundedReceiver<RoomCmd>,
) -> Result<()> {
    let room = wire::room_name(project_id, room_chat);
    let url = format!("{}/room/{}?token={}", shared.ws_url, room, shared.token());
    let (ws, _) = timeout(HELLO_DEADLINE, connect_async(&url))
        .await
        .context("dial timed out")??;
    let (mut tx, mut rx) = ws.split();

    let hello = Hello {
        cursor: *cursor,
        device: shared.device_id(),
        room: room.clone(),
        proto: PROTO_VERSION,
    };
    tx.send(binary(Frame::new(
        FrameType::Hello,
        serde_json::to_value(&hello)?,
        Vec::new(),
    )))
    .await?;

    let state = timeout(HELLO_DEADLINE, async {
        while let Some(msg) = rx.next().await {
            if let WsMessage::Binary(bytes) = msg? {
                if let Some(frame) = Frame::decode(bytes)? {
                    if frame.ty == FrameType::State {
                        let header: WireState = serde_json::from_value(frame.header)?;
                        return Ok::<_, anyhow::Error>(Some((header, frame.payload)));
                    }
                }
            }
        }
        Ok(None)
    })
    .await
    .context("STATE did not arrive")??
    .context("socket closed before STATE")?;
    let (state, frontier) = state;
    set_project_state(shared, project_id, map_box(state.box_state), None);
    set_machine(shared, machine_from_wire(state.box_state), None, None);

    // Checkpoint-on-connect (§7.3): fetch when our replica does not contain
    // the checkpoint frontier, or when our cursor is below it.
    let contained = {
        let docs = shared.docs.lock().unwrap();
        match (docs.get(key), arbos_core::doc::decode_vv(&frontier)) {
            (Some(doc), Some(vv)) => doc.doc.oplog_vv().includes_vv(&vv),
            (Some(_), None) | (None, _) => false,
        }
    };
    if state.checkpoint_seq > 0 && (!contained || *cursor < state.checkpoint_seq) {
        match fetch_checkpoint(shared, &room).await {
            Ok((seq, bytes)) => {
                let docs = shared.docs.lock().unwrap();
                if let Some(doc) = docs.get(key) {
                    match doc.doc.import(&bytes) {
                        Ok(_) => *cursor = (*cursor).max(seq.max(state.checkpoint_seq)),
                        Err(err) => eprintln!("relay room {room}: checkpoint import failed: {err}"),
                    }
                }
            }
            Err(err) => eprintln!("relay room {room}: checkpoint fetch failed: {err:#}"),
        }
    }
    tx.send(binary(Frame::new(
        FrameType::RowsReq,
        serde_json::to_value(RowsReq {
            after: *cursor,
            exclude_own: true,
        })?,
        Vec::new(),
    )))
    .await?;

    for (batch_id, bytes) in pending.iter() {
        tx.send(binary(Frame::new(
            FrameType::Push,
            json!({ "batchId": batch_id }),
            bytes.clone(),
        )))
        .await?;
    }

    let mut presence = interval(PRESENCE_EVERY);
    loop {
        tokio::select! {
            msg = rx.next() => {
                let Some(msg) = msg else { anyhow::bail!("socket closed") };
                let WsMessage::Binary(bytes) = msg? else { continue };
                let Some(frame) = Frame::decode(bytes)? else { continue };
                match frame.ty {
                    FrameType::Row => {
                        let header: RowHeader = serde_json::from_value(frame.header)?;
                        let docs = shared.docs.lock().unwrap();
                        if let Some(doc) = docs.get(key) {
                            if let Err(err) = doc.doc.import(&frame.payload) {
                                eprintln!("relay room {room}: import seq {} failed: {err}", header.seq);
                            }
                        }
                        *cursor = (*cursor).max(header.seq);
                    }
                    FrameType::RowsDone => {
                        let head_seq = frame.header.get("headSeq").and_then(Value::as_i64).unwrap_or(*cursor);
                        *cursor = (*cursor).max(head_seq);
                        let _ = shared.events.send(SyncEvent::Link(Link::Live));
                        if room_chat == PROJECT_DOC_CHAT_ID {
                            emit_project_doc(shared, project_id);
                        } else {
                            emit_snapshot(shared, key);
                        }
                    }
                    FrameType::Ack => {
                        let header: Ack = serde_json::from_value(frame.header)?;
                        pending.retain(|(id, _)| *id != header.batch_id);
                        *cursor = (*cursor).max(header.seq);
                        let had: Vec<Id> = shared.unacked.lock().unwrap().drain().collect();
                        if !had.is_empty() && room_chat != PROJECT_DOC_CHAT_ID {
                            emit_snapshot(shared, key);
                        }
                    }
                    FrameType::Status => {
                        let header: StatusHeader = serde_json::from_value(frame.header)?;
                        set_project_state(shared, project_id, map_box(header.box_state), header.detail.clone());
                        set_machine(shared, machine_from_wire(header.box_state), None, header.detail);
                    }
                    FrameType::Error => {
                        let header: ErrorHeader = serde_json::from_value(frame.header)?;
                        if header.code == "lagging" {
                            tx.send(binary(Frame::new(
                                FrameType::RowsReq,
                                serde_json::to_value(RowsReq { after: *cursor, exclude_own: true })?,
                                Vec::new(),
                            ))).await?;
                        } else {
                            anyhow::bail!("relay error {}: {}", header.code, header.message);
                        }
                    }
                    FrameType::ProbeOk | FrameType::Presence | FrameType::State => {}
                    FrameType::Hello | FrameType::RowsReq | FrameType::Push | FrameType::Probe | FrameType::Wake => {}
                }
            }
            bytes = local_rx.recv() => {
                let Some(bytes) = bytes else { anyhow::bail!("doc dropped") };
                if bytes.len() > wire::ROW_MAX {
                    eprintln!("relay room {room}: local update {} bytes exceeds row cap; dropped", bytes.len());
                    continue;
                }
                let batch_id = new_id();
                tx.send(binary(Frame::new(
                    FrameType::Push,
                    json!({ "batchId": batch_id }),
                    bytes.clone(),
                ))).await?;
                pending.push((batch_id, bytes));
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { anyhow::bail!("room closed") };
                match cmd {
                    RoomCmd::Wake(reason) => {
                        tx.send(binary(Frame::new(
                            FrameType::Wake,
                            serde_json::to_value(WakeHeader { reason: reason.to_string() })?,
                            Vec::new(),
                        ))).await?;
                    }
                }
            }
            _ = presence.tick() => {
                tx.send(binary(Frame::new(
                    FrameType::Presence,
                    json!({ "device": shared.device_id(), "at": now_ms() }),
                    Vec::new(),
                ))).await?;
            }
        }
    }
}

async fn fetch_checkpoint(shared: &Shared, room: &str) -> Result<(i64, Vec<u8>)> {
    let resp = shared
        .http
        .get(format!("{}/room/{}/checkpoint", shared.http_url, room))
        .bearer_auth(shared.token())
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }
    let seq = resp
        .headers()
        .get("x-arbos-checkpoint-seq")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let bytes = resp.bytes().await?.to_vec();
    Ok((seq, bytes))
}

fn binary(frame: Frame) -> WsMessage {
    WsMessage::Binary(frame.encode())
}

fn machine_from_wire(state: WireBox) -> MachineState {
    match state {
        WireBox::Awake => MachineState::Awake,
        WireBox::Waking => MachineState::Waking,
        WireBox::Asleep => MachineState::Asleep,
        WireBox::Error => MachineState::Error,
    }
}

fn map_box(state: WireBox) -> BoxState {
    match state {
        WireBox::Awake => BoxState::Awake,
        WireBox::Waking => BoxState::Waking,
        WireBox::Asleep => BoxState::HibernatedLight,
        WireBox::Error => BoxState::Error,
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn project_of(shared: &Shared, chat_id: &str) -> Option<Id> {
    shared
        .chats
        .lock()
        .unwrap()
        .iter()
        .find(|c| c.id == chat_id)
        .map(|c| c.project_id.clone())
}

fn project_asleep(shared: &Shared, project_id: &str) -> bool {
    shared
        .projects
        .lock()
        .unwrap()
        .iter()
        .find(|p| p.id == project_id)
        .map(|p| p.state.is_asleep() || p.state == BoxState::Error)
        .unwrap_or(false)
}

fn send_wake(shared: &Arc<Shared>, project_id: &str, reason: &'static str) {
    {
        let rooms = shared.rooms.lock().unwrap();
        if let Some(room) = rooms.get(&project_doc_key(project_id)) {
            let _ = room.cmds.send(RoomCmd::Wake(reason));
        }
    }
    let shared = shared.clone();
    let project_id = project_id.to_string();
    tokio::spawn(async move {
        set_project_state(
            &shared,
            &project_id,
            BoxState::Waking,
            Some("Waking sandbox…".into()),
        );
        if let Err(err) = shared
            .post_json(&format!("/projects/{project_id}/wake"), json!({}))
            .await
        {
            shared.notice(format!("Wake failed: {err:#}"));
            set_project_state(
                &shared,
                &project_id,
                BoxState::Error,
                Some(format!("{err:#}")),
            );
        }
    });
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "md" | "txt" | "rs" | "toml" | "json" | "ts" | "py" => "text/plain",
        _ => "application/octet-stream",
    }
}

fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Upload attachments as blobs, then write the user row and its `send`
/// command in one commit (crdt-sync §6.2).
#[allow(clippy::too_many_arguments)]
async fn send_message(
    shared: Arc<Shared>,
    chat_id: Id,
    message_id: Id,
    text: String,
    attachments: Vec<PathBuf>,
    command: &'static str,
    answer: Option<(String, serde_json::Value)>,
) {
    let project_id = project_of(&shared, &chat_id).unwrap_or_default();
    let mut parts: Vec<Part> = Vec::new();
    for path in attachments {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment")
            .to_string();
        let key = format!("att-{}-{}", new_id().to_lowercase(), safe_name(&name));
        let mime = mime_for(&path);
        let blob_ref = match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let put = shared
                    .http
                    .put(format!("{}/blob/{}/{}", shared.http_url, project_id, key))
                    .bearer_auth(shared.token())
                    .header("content-type", mime)
                    .body(bytes)
                    .send()
                    .await;
                match put {
                    Ok(r) if r.status().is_success() => Some(key),
                    Ok(r) => {
                        shared.notice(format!("Attachment upload failed: HTTP {}", r.status()));
                        None
                    }
                    Err(err) => {
                        shared.notice(format!("Attachment upload failed: {err}"));
                        None
                    }
                }
            }
            Err(err) => {
                shared.notice(format!("Could not read {}: {err}", path.display()));
                None
            }
        };
        parts.push(Part::Attachment {
            id: new_id(),
            attachment: Attachment {
                name,
                mime: mime.into(),
                blob_ref,
            },
        });
    }
    parts.push(Part::Text {
        id: new_id(),
        text: text.clone(),
    });
    shared.unacked.lock().unwrap().insert(message_id.clone());
    {
        let docs = shared.docs.lock().unwrap();
        let Some(doc) = docs.get(&chat_id) else {
            return;
        };
        let msg = Message {
            id: message_id.clone(),
            chat_id: chat_id.clone(),
            role: Role::User,
            sender: "user".into(),
            status: MessageStatus::Done,
            turn: None,
            decision: None,
            subagent_id: None,
            subagent_name: None,
            in_reply_to: None,
            routine_name: None,
            usage: None,
            return_contract: None,
            waiting_user: false,
            waiting_approval: false,
            model: None,
            effort: None,
            created_at: now_ms(),
            completed_at: None,
            pending: false,
            parts,
            hidden_parts: Vec::new(),
        };
        let _ = doc.insert_message(&msg);
        let payload = match &answer {
            Some((qid, value)) => {
                json!({ "messageId": message_id, "questionId": qid, "value": value, "text": text })
            }
            None => json!({ "messageId": message_id }),
        };
        let _ = doc.push_command(command, payload, &shared.device_id());
        let untitled = shared
            .chats
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.id == chat_id && !c.is_pinned && c.title == "New chat");
        if untitled {
            let title = crate::app::title_from(&text);
            let _ = doc.push_command("retitle", json!({ "title": title }), &shared.device_id());
            if let Some(c) = shared
                .chats
                .lock()
                .unwrap()
                .iter_mut()
                .find(|c| c.id == chat_id)
            {
                c.title = title;
            }
        }
        doc.commit();
    }
    mark_activity(&shared, &chat_id);
    if project_asleep(&shared, &project_id) {
        send_wake(&shared, &project_id, "send");
    }
}

fn handle_command(shared: &Arc<Shared>, cmd: ClientCommand) {
    match cmd {
        ClientCommand::Pair { code } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let name = hostname();
                match shared
                    .post_json("/pair", json!({ "code": code.trim(), "name": name }))
                    .await
                {
                    Ok(v) => {
                        let creds = DeviceCredentials {
                            relay_url: shared.http_url.clone(),
                            device_id: v
                                .get("device_id")
                                .and_then(Value::as_str)
                                .unwrap_or("device")
                                .to_string(),
                            device_token: v
                                .get("device_token")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        };
                        if creds.device_token.is_empty() {
                            let _ = shared
                                .events
                                .send(SyncEvent::PairingFailed("relay returned no token".into()));
                            return;
                        }
                        if let Err(err) = credentials::store(&creds) {
                            shared.notice(format!("Token saved in memory only: {err:#}"));
                        }
                        *shared.creds.write().unwrap() = Some(creds.clone());
                        let _ = shared.events.send(SyncEvent::Paired {
                            device_id: creds.device_id,
                        });
                        start_after_pairing(&shared);
                    }
                    Err(err) => {
                        let _ = shared
                            .events
                            .send(SyncEvent::PairingFailed(format!("{err:#}")));
                    }
                }
            });
        }
        ClientCommand::OpenChat { chat_id } | ClientCommand::MarkRead { chat_id } => {
            let mut chats = shared.chats.lock().unwrap();
            if let Some(c) = chats.iter_mut().find(|c| c.id == chat_id) {
                c.unread = false;
            }
        }
        ClientCommand::Send {
            chat_id,
            message_id,
            text,
            attachments,
        } => {
            tokio::spawn(send_message(
                shared.clone(),
                chat_id,
                message_id,
                text,
                attachments,
                "send",
                None,
            ));
        }
        ClientCommand::Answer {
            chat_id,
            message_id,
            text,
            question_id,
            value,
        } => {
            let answer = question_id.map(|qid| (qid, value.unwrap_or(json!(null))));
            tokio::spawn(send_message(
                shared.clone(),
                chat_id,
                message_id,
                text,
                Vec::new(),
                "answer",
                answer,
            ));
        }
        ClientCommand::Stop { chat_id } => {
            let docs = shared.docs.lock().unwrap();
            if let Some(doc) = docs.get(&chat_id) {
                let _ = doc.push_command("stop", json!({}), &shared.device_id());
                doc.commit();
            }
        }
        ClientCommand::Approve {
            chat_id,
            approval_id,
        } => {
            let docs = shared.docs.lock().unwrap();
            if let Some(doc) = docs.get(&chat_id) {
                let _ = doc.push_command(
                    "approve",
                    json!({ "approvalId": approval_id }),
                    &shared.device_id(),
                );
                doc.commit();
            }
        }
        ClientCommand::Reject {
            chat_id,
            approval_id,
            reason,
        } => {
            let docs = shared.docs.lock().unwrap();
            if let Some(doc) = docs.get(&chat_id) {
                let mut payload = json!({ "approvalId": approval_id });
                if let Some(r) = reason.filter(|r| !r.trim().is_empty()) {
                    payload["reason"] = json!(r);
                }
                let _ = doc.push_command("reject", payload, &shared.device_id());
                doc.commit();
            }
        }
        ClientCommand::Retry {
            chat_id,
            message_id,
        } => {
            let docs = shared.docs.lock().unwrap();
            if let Some(doc) = docs.get(&chat_id) {
                let _ = doc.push_command(
                    "retry",
                    json!({ "messageId": message_id }),
                    &shared.device_id(),
                );
                doc.commit();
            }
        }
        ClientCommand::NewChat {
            project_id,
            chat_id: local_id,
        } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                match shared
                    .post_json(
                        &format!("/projects/{project_id}/chats"),
                        json!({ "title": "New chat" }),
                    )
                    .await
                {
                    Ok(v) => {
                        let Some(chat) = v.get("chat").and_then(parse_chat) else {
                            shared.notice("Relay returned no chat");
                            return;
                        };
                        shared.chats.lock().unwrap().push(chat.clone());
                        ensure_room(&shared, &project_id, &chat.id, &chat.id);
                        let _ = shared.events.send(SyncEvent::ChatCreated {
                            project_id: project_id.clone(),
                            local_id,
                            chat,
                        });
                    }
                    Err(err) => shared.notice(format!("New chat failed: {err:#}")),
                }
            });
        }
        ClientCommand::CloseChat { chat_id } => {
            let project_id = project_of(shared, &chat_id);
            shared
                .chats
                .lock()
                .unwrap()
                .retain(|c| c.id != chat_id || c.is_pinned);
            emit_chats(shared);
            if let Some(project_id) = project_id {
                let shared = shared.clone();
                tokio::spawn(async move {
                    // Soft close on the relay; 409 = pinned, which the UI never offers.
                    let resp = shared
                        .http
                        .delete(format!(
                            "{}/projects/{}/chats/{}",
                            shared.http_url, project_id, chat_id
                        ))
                        .bearer_auth(shared.token())
                        .send()
                        .await;
                    match resp {
                        Ok(r) if r.status().is_success() || r.status().as_u16() == 404 => {
                            if let Some(room) = shared.rooms.lock().unwrap().remove(&chat_id) {
                                drop(room);
                            }
                            shared.docs.lock().unwrap().remove(&chat_id);
                        }
                        Ok(r) if r.status().as_u16() == 409 => {
                            shared.notice("The pinned chat cannot be closed");
                            if let Err(err) = refresh_registry(&shared).await {
                                shared.notice(format!("Relay: {err:#}"));
                            }
                        }
                        Ok(r) => shared.notice(format!("Close chat: HTTP {}", r.status())),
                        Err(err) => shared.notice(format!("Close chat: {err}")),
                    }
                });
            }
        }
        ClientCommand::SetModel {
            project_id,
            model,
            effort,
        } => {
            // Mirror into the project doc `settings` too (crdt-sync §6.3): the
            // box reads the doc first, so both must agree.
            {
                let docs = shared.docs.lock().unwrap();
                if let Some(doc) = docs.get(&project_doc_key(&project_id)) {
                    let settings = doc.doc.get_map("settings");
                    let _ = settings.insert("agentModel", model.as_str());
                    let _ = settings.insert("agentEffort", effort_to(effort));
                    let models: loro::LoroValue = serde_json::from_value(json!({
                        "talking_model": { "id": model, "effort": effort_to(effort) }
                    }))
                    .unwrap_or(loro::LoroValue::Null);
                    let _ = settings.insert("models", models);
                    doc.commit();
                }
            }
            {
                let mut projects = shared.projects.lock().unwrap();
                if let Some(p) = projects.iter_mut().find(|p| p.id == project_id) {
                    p.model = model.clone();
                    p.effort = effort;
                }
                let snapshot = projects.clone();
                drop(projects);
                let _ = shared.events.send(SyncEvent::Projects(snapshot));
            }
            // Canonical path: the registry. Slots merge one by one.
            let shared = shared.clone();
            tokio::spawn(async move {
                let body = json!({
                    "models": { "talking_model": { "id": model, "effort": effort_to(effort) } }
                });
                let resp = shared
                    .http
                    .patch(format!("{}/projects/{}", shared.http_url, project_id))
                    .bearer_auth(shared.token())
                    .json(&body)
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {}
                    Ok(r) => shared.notice(format!("Model change not saved: HTTP {}", r.status())),
                    Err(err) => shared.notice(format!("Model change not saved: {err}")),
                }
            });
        }
        ClientCommand::CreateProject { name } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let provider = std::env::var("ARBOS_BOX_PROVIDER").ok();
                let mut body = json!({ "name": name });
                if let Some(p) = provider {
                    body["provider"] = json!(p);
                }
                match shared.post_json("/projects", body).await {
                    Ok(_) => {
                        if let Err(err) = refresh_registry(&shared).await {
                            shared.notice(format!("Relay: {err:#}"));
                        }
                    }
                    Err(err) => shared.notice(format!("New project failed: {err:#}")),
                }
            });
        }
        ClientCommand::MachineStart | ClientCommand::MachineStop => {
            let start = matches!(cmd, ClientCommand::MachineStart);
            let shared = shared.clone();
            tokio::spawn(async move {
                // Planned user-level endpoint first; fall back to per-project
                // start/stop until the relay grows `/machine`.
                let verb = if start { "start" } else { "stop" };
                let primary = shared
                    .post_json(&format!("/machine/{verb}"), json!({}))
                    .await;
                if primary.is_ok() {
                    set_machine(
                        &shared,
                        if start {
                            MachineState::Waking
                        } else {
                            MachineState::Stopped
                        },
                        None,
                        None,
                    );
                    return;
                }
                let ids: Vec<Id> = shared
                    .projects
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|p| p.id.clone())
                    .collect();
                set_machine(
                    &shared,
                    if start {
                        MachineState::Waking
                    } else {
                        MachineState::Stopped
                    },
                    None,
                    None,
                );
                for id in ids {
                    if let Err(err) = shared
                        .post_json(&format!("/projects/{id}/{verb}"), json!({}))
                        .await
                    {
                        shared.notice(format!("Machine {verb}: {err:#}"));
                    }
                }
                if let Err(err) = refresh_registry(&shared).await {
                    shared.notice(format!("Relay: {err:#}"));
                }
            });
        }
        ClientCommand::FetchBlob {
            project_id,
            key,
            save,
        } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let resp = shared
                    .http
                    .get(format!("{}/blob/{}/{}", shared.http_url, project_id, key))
                    .bearer_auth(shared.token())
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {
                        let mime = r
                            .headers()
                            .get("content-type")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("application/octet-stream")
                            .to_string();
                        let bytes = r.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
                        let saved_to = if save {
                            let path = credentials::downloads_dir().join(safe_name(&key));
                            match tokio::fs::write(&path, &bytes).await {
                                Ok(()) => Some(path),
                                Err(err) => {
                                    shared.notice(format!("Could not save: {err}"));
                                    None
                                }
                            }
                        } else {
                            None
                        };
                        let _ = shared.events.send(SyncEvent::Blob {
                            project_id,
                            key,
                            mime,
                            bytes: Arc::new(bytes),
                            saved_to,
                        });
                    }
                    Ok(r) => shared.notice(format!("Blob {key}: HTTP {}", r.status())),
                    Err(err) => shared.notice(format!("Blob {key}: {err}")),
                }
            });
        }
        ClientCommand::RefreshConnectors => {
            let shared = shared.clone();
            tokio::spawn(async move {
                refresh_connectors(&shared).await;
            });
        }
        ClientCommand::ConnectorConnect { provider, token } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let body = match token {
                    Some(t) => json!({ "token": t }),
                    None => json!({}),
                };
                match shared
                    .post_json(&format!("/connectors/{provider}/connect"), body)
                    .await
                {
                    Ok(v) => {
                        if let Some(url) = v.get("authUrl").and_then(Value::as_str) {
                            let _ = shared.events.send(SyncEvent::ConnectorAuth {
                                provider: provider.clone(),
                                auth_url: url.to_string(),
                            });
                        } else if let Some(c) = v.get("connector") {
                            let status = c["status"].as_str().unwrap_or("");
                            if status == "connected" {
                                shared.notice(format!("{provider} connected."));
                            } else {
                                shared.notice(format!(
                                    "{provider}: {status} {}",
                                    c["last_error"].as_str().unwrap_or("")
                                ));
                            }
                        }
                        // The loopback callback lands out of band: poll the list for a while.
                        for wait in [0u64, 4, 8, 15, 30, 60] {
                            sleep(Duration::from_secs(wait)).await;
                            refresh_connectors(&shared).await;
                        }
                    }
                    Err(err) => shared.notice(format!("Connect {provider}: {err:#}")),
                }
            });
        }
        ClientCommand::ConnectorDisconnect { id } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let resp = shared
                    .http
                    .delete(format!("{}/connectors/{}", shared.http_url, id))
                    .bearer_auth(shared.token())
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {}
                    Ok(r) => shared.notice(format!("Disconnect: HTTP {}", r.status())),
                    Err(err) => shared.notice(format!("Disconnect: {err}")),
                }
                refresh_connectors(&shared).await;
            });
        }
        ClientCommand::WatchBrowser { project_id, on } => {
            let mut watches = shared.browser_watch.lock().unwrap();
            if let Some(task) = watches.remove(&project_id) {
                task.abort();
            }
            if on {
                let shared2 = shared.clone();
                let pid = project_id.clone();
                watches.insert(
                    project_id,
                    tokio::spawn(async move {
                        // No session yet closes the socket at once; keep trying
                        // quietly while the panel is open.
                        let mut first = true;
                        loop {
                            if let Err(err) = watch_browser(shared2.clone(), pid.clone()).await {
                                if first {
                                    shared2.notice(format!("Browser stream: {err:#} — retrying"));
                                    first = false;
                                }
                            }
                            sleep(Duration::from_secs(5)).await;
                        }
                    }),
                );
            }
        }
        ClientCommand::RoutinesRefresh { project_id } => {
            let shared = shared.clone();
            tokio::spawn(async move { refresh_routines(&shared, &project_id).await });
        }
        ClientCommand::LocalRefresh => {
            let shared = shared.clone();
            tokio::spawn(async move { refresh_local(&shared).await });
        }
        ClientCommand::LocalSetAllowReads { device_id, allow } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let resp = shared
                    .http
                    .patch(format!("{}/local/devices/{}", shared.http_url, device_id))
                    .bearer_auth(shared.token())
                    .json(&json!({ "allow_reads": allow }))
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {}
                    Ok(r) => shared.notice(format!("This Mac: HTTP {}", r.status())),
                    Err(err) => shared.notice(format!("This Mac: {err}")),
                }
                refresh_local(&shared).await;
            });
        }
        ClientCommand::LocalForget { device_id } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let resp = shared
                    .http
                    .delete(format!("{}/local/devices/{}", shared.http_url, device_id))
                    .bearer_auth(shared.token())
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {}
                    Ok(r) => shared.notice(format!("Forget helper: HTTP {}", r.status())),
                    Err(err) => shared.notice(format!("Forget helper: {err}")),
                }
                refresh_local(&shared).await;
            });
        }
        ClientCommand::RoutineCreate {
            project_id,
            name,
            prompt,
            chat_id,
            trigger,
        } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let body = json!({
                    "name": name, "prompt": prompt, "chatId": chat_id,
                    "trigger": serde_json::to_value(&trigger).unwrap_or(Value::Null),
                });
                match shared
                    .post_json(&format!("/projects/{project_id}/routines"), body)
                    .await
                {
                    Ok(_) => refresh_routines(&shared, &project_id).await,
                    Err(err) => shared.notice(format!("Routine not created: {err:#}")),
                }
            });
        }
        ClientCommand::RoutineSetEnabled {
            project_id,
            routine_id,
            enabled,
        } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let resp = shared
                    .http
                    .patch(format!(
                        "{}/projects/{}/routines/{}",
                        shared.http_url, project_id, routine_id
                    ))
                    .bearer_auth(shared.token())
                    .json(&json!({ "enabled": enabled }))
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {}
                    Ok(r) => shared.notice(format!("Routine not updated: HTTP {}", r.status())),
                    Err(err) => shared.notice(format!("Routine not updated: {err}")),
                }
                refresh_routines(&shared, &project_id).await;
            });
        }
        ClientCommand::RoutineRunNow {
            project_id,
            routine_id,
        } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                if let Err(err) = shared
                    .post_json(
                        &format!("/projects/{project_id}/routines/{routine_id}/run"),
                        json!({}),
                    )
                    .await
                {
                    shared.notice(format!("Run now failed: {err:#}"));
                }
                refresh_routine_runs(&shared, &project_id, &routine_id).await;
                refresh_routines(&shared, &project_id).await;
            });
        }
        ClientCommand::RoutineRuns {
            project_id,
            routine_id,
        } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                refresh_routine_runs(&shared, &project_id, &routine_id).await;
            });
        }
        ClientCommand::RoutineDelete {
            project_id,
            routine_id,
        } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let resp = shared
                    .http
                    .delete(format!(
                        "{}/projects/{}/routines/{}",
                        shared.http_url, project_id, routine_id
                    ))
                    .bearer_auth(shared.token())
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {}
                    Ok(r) => shared.notice(format!("Routine not deleted: HTTP {}", r.status())),
                    Err(err) => shared.notice(format!("Routine not deleted: {err}")),
                }
                refresh_routines(&shared, &project_id).await;
            });
        }
        ClientCommand::SkillsRefresh => {
            let shared = shared.clone();
            tokio::spawn(async move { refresh_skills(&shared).await });
        }
        ClientCommand::SkillSave { skill } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let body = json!({
                    "slug": skill.slug, "name": skill.name, "description": skill.description,
                    "tags": skill.tags, "when_to_use": skill.when_to_use, "body": skill.body,
                    "shared": skill.shared, "origin_project_id": skill.origin_project_id,
                });
                let resp = if skill.id.is_empty() {
                    shared.post_json("/skills", body).await
                } else {
                    let r = shared
                        .http
                        .put(format!("{}/skills/{}", shared.http_url, skill.id))
                        .bearer_auth(shared.token())
                        .json(&body)
                        .send()
                        .await;
                    match r {
                        Ok(r) if r.status().is_success() => Ok(Value::Null),
                        Ok(r) => Err(anyhow!(
                            "PUT /skills: HTTP {} {}",
                            r.status(),
                            r.text().await.unwrap_or_default()
                        )),
                        Err(err) => Err(err.into()),
                    }
                };
                match resp {
                    Ok(_) => shared.notice(format!("Skill \"{}\" saved", skill.name)),
                    Err(err) => shared.notice(format!("Skill not saved: {err:#}")),
                }
                refresh_skills(&shared).await;
            });
        }
        ClientCommand::SkillDelete { id } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let r = shared
                    .http
                    .delete(format!("{}/skills/{}", shared.http_url, id))
                    .bearer_auth(shared.token())
                    .send()
                    .await;
                match r {
                    Ok(r) if r.status().is_success() || r.status().as_u16() == 404 => {}
                    Ok(r) => shared.notice(format!("Delete skill: HTTP {}", r.status())),
                    Err(err) => shared.notice(format!("Delete skill: {err}")),
                }
                refresh_skills(&shared).await;
            });
        }
        ClientCommand::RefreshChannels => {
            let shared = shared.clone();
            tokio::spawn(async move {
                refresh_channels(&shared).await;
            });
        }
        ClientCommand::ChannelPairNew {
            project_id,
            chat_id,
        } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                match shared
                    .post_json(
                        "/channels/pair/new",
                        json!({ "projectId": project_id, "chatId": chat_id }),
                    )
                    .await
                {
                    Ok(_) => refresh_channels(&shared).await,
                    Err(err) => shared.notice(format!("Pairing code: {err:#}")),
                }
            });
        }
        ClientCommand::ChannelUnbind { id } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let resp = shared
                    .http
                    .delete(format!("{}/channels/bindings/{}", shared.http_url, id))
                    .bearer_auth(shared.token())
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {}
                    Ok(r) => shared.notice(format!("Unbind: HTTP {}", r.status())),
                    Err(err) => shared.notice(format!("Unbind: {err}")),
                }
                refresh_channels(&shared).await;
            });
        }
    }
}

/// Settings → Channels: `GET /channels` → one view (F7).
async fn refresh_channels(shared: &Arc<Shared>) {
    let v = match shared.get_json("/channels").await {
        Ok(v) => v,
        Err(err) => {
            // Older relays have no channels; stay quiet unless it is a real failure.
            if !format!("{err:#}").contains("HTTP 404") {
                shared.notice(format!("Channels: {err:#}"));
            }
            return;
        }
    };
    let list = |key: &str| -> Vec<Value> { v[key].as_array().cloned().unwrap_or_default() };
    let platforms: Vec<ChannelPlatform> = list("platforms")
        .into_iter()
        .filter_map(|p| serde_json::from_value(p).ok())
        .collect();
    let bindings: Vec<ChannelBinding> = list("bindings")
        .into_iter()
        .filter_map(|b| serde_json::from_value(b).ok())
        .collect();
    let pairing: Option<ChannelPairing> = serde_json::from_value(v["pairing"].clone()).ok();
    let _ = shared.events.send(SyncEvent::Channels(ChannelsView {
        mock: v["mock"].as_bool().unwrap_or(false),
        platforms,
        bindings,
        pairing,
        loaded: true,
    }));
}

async fn refresh_routines(shared: &Arc<Shared>, project_id: &str) {
    match shared
        .get_json(&format!("/projects/{project_id}/routines"))
        .await
    {
        Ok(v) => {
            let routines: Vec<Routine> = serde_json::from_value(v).unwrap_or_default();
            let _ = shared.events.send(SyncEvent::Routines {
                project_id: project_id.to_string(),
                routines,
            });
        }
        Err(err) => shared.notice(format!("Routines: {err:#}")),
    }
}

async fn refresh_routine_runs(shared: &Arc<Shared>, project_id: &str, routine_id: &str) {
    match shared
        .get_json(&format!(
            "/projects/{project_id}/routines/{routine_id}/runs?limit=20"
        ))
        .await
    {
        Ok(v) => {
            let runs: Vec<RoutineRun> = serde_json::from_value(v).unwrap_or_default();
            let _ = shared.events.send(SyncEvent::RoutineRuns {
                routine_id: routine_id.to_string(),
                runs,
            });
        }
        Err(err) => shared.notice(format!("Routine runs: {err:#}")),
    }
}

/// Settings → This Mac: `GET /local/devices` (F9).
async fn refresh_local(shared: &Arc<Shared>) {
    let list = match shared.get_json("/local/devices").await {
        Ok(v) => v,
        Err(err) => {
            if !format!("{err:#}").contains("HTTP 404") {
                shared.notice(format!("This Mac: {err:#}"));
            }
            return;
        }
    };
    let devices: Vec<LocalDevice> = list["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| serde_json::from_value(d.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let _ = shared.events.send(SyncEvent::LocalDevices(LocalView {
        devices,
        relay_url: shared.http_url.clone(),
        loaded: true,
    }));
}

/// Settings → Connectors: `GET /connectors/catalog` + `GET /connectors` → one view (F1).
async fn refresh_connectors(shared: &Arc<Shared>) {
    let (catalog, list) = tokio::join!(
        shared.get_json("/connectors/catalog"),
        shared.get_json("/connectors")
    );
    let (catalog, list) = match (catalog, list) {
        (Ok(c), Ok(l)) => (c, l),
        (Err(err), _) | (_, Err(err)) => {
            // Older relays have no connectors; stay quiet unless it is a real failure.
            if !format!("{err:#}").contains("HTTP 404") {
                shared.notice(format!("Connectors: {err:#}"));
            }
            return;
        }
    };
    let providers: Vec<ConnectorProvider> = catalog["providers"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| serde_json::from_value(p.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let items: Vec<Connector> = list["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| serde_json::from_value(c.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let _ = shared.events.send(SyncEvent::Connectors(ConnectorsView {
        mock: list["mock"].as_bool().unwrap_or(false),
        providers,
        items,
        loaded: true,
    }));
}

/// S16: the box's `/browser/stream` (agentd proxies agent-browser's JPEG
/// WebSocket). The URL comes from `GET /projects/{id}/status`
/// (`provider.box_url`), or `ARBOS_BROWSER_STREAM_URL` for a direct dial.
async fn watch_browser(shared: Arc<Shared>, project_id: Id) -> Result<()> {
    let url = match std::env::var("ARBOS_BROWSER_STREAM_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            let status = shared
                .get_json(&format!("/projects/{project_id}/status"))
                .await?;
            let provider = status.get("provider").cloned().unwrap_or(Value::Null);
            // Relay-proxied stream (token in the URL) first; a direct box
            // URL only as the v2 fallback.
            match provider.get("browser_stream_url").and_then(Value::as_str) {
                Some(url) if !url.is_empty() => url.to_string(),
                Some(_) | None => {
                    let box_url = provider
                        .get("box_url")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("box has no URL yet (asleep?)"))?
                        .trim_end_matches('/')
                        .to_string();
                    let ws = box_url
                        .replacen("https://", "wss://", 1)
                        .replacen("http://", "ws://", 1);
                    format!("{ws}/browser/stream")
                }
            }
        }
    };
    let (ws, _) = timeout(HELLO_DEADLINE, connect_async(&url))
        .await
        .context("browser stream dial timed out")??;
    let (_tx, mut rx) = ws.split();
    while let Some(msg) = rx.next().await {
        let jpeg: Option<Vec<u8>> = match msg? {
            WsMessage::Binary(b) => Some(b.to_vec()),
            WsMessage::Text(t) => {
                // agent-browser frames: {"type":"frame","data":"<base64 jpeg>"}
                let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::Null);
                v.get("data")
                    .and_then(Value::as_str)
                    .and_then(|d| base64_decode(d.rsplit(',').next().unwrap_or(d)))
            }
            WsMessage::Close(_) => break,
            WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_) => None,
        };
        if let Some(jpeg) = jpeg {
            let _ = shared.events.send(SyncEvent::BrowserFrame {
                project_id: project_id.clone(),
                jpeg: Arc::new(jpeg),
            });
        }
    }
    Ok(())
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' || c == b'\n' || c == b'\r' {
            continue;
        }
        let v = TABLE.iter().position(|&t| t == c)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "arbos-mac".into())
}

impl Drop for Room {
    fn drop(&mut self) {
        self.task.abort();
    }
}
