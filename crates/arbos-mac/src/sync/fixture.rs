//! Local, offline source. Every chat is a real Loro doc (`doc.rs`); the
//! "agent" is a scripted tokio task that writes into the doc the way the box
//! would: a `deciding` status, tool cards flipping from running to done, text
//! appended in 120 ms frames, a final status. The UI cannot tell it apart from
//! the relay path.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use futures::future::BoxFuture;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::sleep;

use crate::model::{Machine, MachineState, ProjectDocView, ThreadInfo};

use super::doc::ChatDoc;
use super::{ClientCommand, SyncEvent, SyncSource};
use crate::model::{
    new_id, now_ms, ApprovalRisk, ApprovalState, Attachment, BoxState, ChannelBinding,
    ChannelPairing, ChannelPlatform, ChannelPlatformState, ChannelsView, Chat, ChoiceOption,
    Connector, ConnectorAction, ConnectorProvider, ConnectorStatus, ConnectorsView, Effort,
    FieldKind, FormField, Id, Link, LocalDevice, LocalView, Message, MessageStatus, Part, Project,
    QuestionState, Role, Routine, RoutineRun, RoutineRunStatus, RoutineTrigger, Skill, StatusKind,
    ToolCall, ToolStatus, TurnKind, Widget,
};

pub const STREAM_FRAME_MS: u64 = arbos_core::STREAM_COMMIT_MS;
const DEVICE: &str = "mac:fixture";

pub const PROJECT_ARBOS: &str = "01J9ARBOS00000000000000000";
pub const PROJECT_AFFINE: &str = "01J9AFFINE0000000000000000";
pub const PROJECT_COMMS: &str = "01J9COMMS00000000000000000";

pub const CHAT_ARBOS_MAIN: &str = "01J9CHAT0ARBOS000000000000";
pub const CHAT_ARBOS_RESEARCH: &str = "01J9CHAT0RESEARCH000000000";
pub const CHAT_ARBOS_LAUNCH: &str = "01J9CHAT0LAUNCH00000000000";
pub const CHAT_ARBOS_WIDGETS: &str = "01J9CHAT0WIDGETS0000000000";
pub const CHAT_ARBOS_DRAFTS: &str = "01J9CHAT0DRAFTS00000000000";
pub const CHAT_AFFINE_MAIN: &str = "01J9CHAT0AFFINE00000000000";
pub const CHAT_COMMS_MAIN: &str = "01J9CHAT0COMMS000000000000";

pub const MODELS: [&str; 4] = [
    "GPT-6.1-Sol",
    "Claude Fable 5.1",
    "Gemini 4 Argon",
    "Grok 4.7",
];

type Docs = Arc<Mutex<HashMap<Id, ChatDoc>>>;

struct DocChanged {
    chat_id: Id,
    touched: super::doc::Touched,
}

struct Shared {
    docs: Docs,
    projects: Mutex<Vec<Project>>,
    chats: Mutex<Vec<Chat>>,
    open_chat: Mutex<Option<Id>>,
    running: Mutex<HashMap<Id, Arc<AtomicBool>>>,
    skills: Mutex<Vec<Skill>>,
    events: mpsc::UnboundedSender<SyncEvent>,
    doc_tx: mpsc::UnboundedSender<DocChanged>,
    subscriptions: Mutex<Vec<loro::Subscription>>,
    connectors: Mutex<ConnectorsView>,
    channels: Mutex<ChannelsView>,
    routines: Mutex<Vec<Routine>>,
    routine_runs: Mutex<HashMap<Id, Vec<RoutineRun>>>,
    local: Mutex<LocalView>,
    machine: Mutex<Machine>,
}

pub struct FixtureSource;

impl FixtureSource {
    pub fn new() -> Self {
        Self
    }
}

impl SyncSource for FixtureSource {
    fn name(&self) -> &'static str {
        "fixture"
    }

    fn run(
        self: Box<Self>,
        events: mpsc::UnboundedSender<SyncEvent>,
        mut commands: mpsc::UnboundedReceiver<ClientCommand>,
    ) -> BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            let (doc_tx, mut doc_rx) = mpsc::unbounded_channel();
            let shared = Arc::new(Shared {
                docs: Arc::new(Mutex::new(HashMap::new())),
                projects: Mutex::new(seed_projects()),
                chats: Mutex::new(seed_chats()),
                open_chat: Mutex::new(None),
                running: Mutex::new(HashMap::new()),
                skills: Mutex::new(seed_skills()),
                events: events.clone(),
                doc_tx,
                subscriptions: Mutex::new(Vec::new()),
                connectors: Mutex::new(seed_connectors()),
                channels: Mutex::new(seed_channels()),
                routines: Mutex::new(seed_routines()),
                routine_runs: Mutex::new(seed_routine_runs()),
                local: Mutex::new(seed_local()),
                machine: Mutex::new(Machine {
                    state: MachineState::Awake,
                    since: Some(now_ms().saturating_sub(42 * 60_000)),
                    spec: Some("4 vCPU · 8 GiB · 10 GiB".into()),
                    class: Some("arbos-agent-0.1.7".into()),
                    spend_today_usd: 5.39,
                    spend_cap_usd: 25.0,
                    detail: None,
                    progress: None,
                }),
            });
            seed_docs(&shared);

            let _ = events.send(SyncEvent::Link(Link::Connecting));
            emit_projects(&shared);
            emit_machine(&shared);
            emit_chats(&shared);
            {
                let docs = shared.docs.lock().unwrap();
                for (chat_id, doc) in docs.iter() {
                    let _ = events.send(SyncEvent::ChatSnapshot {
                        chat_id: chat_id.clone(),
                        messages: doc.read_all(),
                    });
                }
            }
            emit_project_docs(&shared);
            emit_connectors(&shared);
            emit_skills(&shared);
            emit_channels(&shared);
            emit_local(&shared);
            let _ = events.send(SyncEvent::Link(Link::Live));

            loop {
                tokio::select! {
                    changed = doc_rx.recv() => {
                        let Some(changed) = changed else { break };
                        forward_doc_change(&shared, changed);
                    }
                    cmd = commands.recv() => {
                        let Some(cmd) = cmd else { break };
                        handle_command(&shared, cmd);
                    }
                }
            }
            Ok(())
        })
    }
}

fn forward_doc_change(shared: &Arc<Shared>, changed: DocChanged) {
    let docs = shared.docs.lock().unwrap();
    let Some(doc) = docs.get(&changed.chat_id) else {
        return;
    };
    if changed.touched.list_changed {
        let _ = shared.events.send(SyncEvent::ChatSnapshot {
            chat_id: changed.chat_id.clone(),
            messages: doc.read_all(),
        });
        return;
    }
    for ix in changed.touched.messages {
        if let Some(message) = doc.read_message(ix) {
            let _ = shared.events.send(SyncEvent::MessageUpsert {
                chat_id: changed.chat_id.clone(),
                message: Box::new(message),
            });
        }
    }
}

fn handle_command(shared: &Arc<Shared>, cmd: ClientCommand) {
    match cmd {
        ClientCommand::OpenChat { chat_id } => {
            *shared.open_chat.lock().unwrap() = Some(chat_id.clone());
            mark_read(shared, &chat_id);
        }
        ClientCommand::Send {
            chat_id,
            message_id,
            text,
            attachments,
        } => {
            let project_id = project_of(shared, &chat_id);
            {
                let docs = shared.docs.lock().unwrap();
                if let Some(doc) = docs.get(&chat_id) {
                    let asleep = machine_down(shared);
                    let mut parts: Vec<Part> = attachments
                        .iter()
                        .map(|path| Part::Attachment {
                            id: new_id(),
                            attachment: Attachment {
                                name: path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("attachment")
                                    .to_string(),
                                mime: "application/octet-stream".into(),
                                blob_ref: None,
                            },
                        })
                        .collect();
                    parts.push(Part::Text {
                        id: new_id(),
                        text: text.clone(),
                    });
                    let msg = Message {
                        id: message_id.clone(),
                        chat_id: chat_id.clone(),
                        role: Role::User,
                        sender: "user".into(),
                        status: if asleep {
                            MessageStatus::Queued
                        } else {
                            MessageStatus::Done
                        },
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
                        pending: true,
                        parts,
                        hidden_parts: Vec::new(),
                    };
                    let _ = doc.insert_message(&msg);
                    let _ = doc.push_command(
                        "send",
                        json!({ "messageId": message_id, "text": text }),
                        DEVICE,
                    );
                    doc.commit();
                }
            }
            maybe_retitle(shared, &chat_id, &text);
            spawn_turn(shared, project_id, chat_id, message_id, text, false);
        }
        ClientCommand::Answer {
            chat_id,
            message_id,
            text,
            question_id,
            value,
        } => {
            let project_id = project_of(shared, &chat_id);
            {
                let docs = shared.docs.lock().unwrap();
                if let Some(doc) = docs.get(&chat_id) {
                    if let Some(qid) = &question_id {
                        let _ = doc.answer_question(qid, value.as_ref().unwrap_or(&json!(null)));
                    }
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
                        pending: true,
                        parts: vec![Part::Text {
                            id: new_id(),
                            text: text.clone(),
                        }],
                        hidden_parts: Vec::new(),
                    };
                    let _ = doc.insert_message(&msg);
                    let _ = doc.push_command(
                        "answer",
                        json!({ "messageId": message_id, "text": text, "questionId": question_id, "value": value }),
                        DEVICE,
                    );
                    doc.commit();
                }
            }
            spawn_turn(shared, project_id, chat_id, message_id, text, true);
        }
        ClientCommand::Stop { chat_id } => {
            if let Some(flag) = shared.running.lock().unwrap().get(&chat_id) {
                flag.store(true, Ordering::SeqCst);
            }
            let docs = shared.docs.lock().unwrap();
            if let Some(doc) = docs.get(&chat_id) {
                let _ = doc.push_command("stop", json!({}), DEVICE);
                doc.commit();
            }
        }
        ClientCommand::Approve {
            chat_id,
            approval_id,
        } => decide_approval(shared, chat_id, approval_id, ApprovalState::Approved, None),
        ClientCommand::Reject {
            chat_id,
            approval_id,
            reason,
        } => decide_approval(
            shared,
            chat_id,
            approval_id,
            ApprovalState::Rejected,
            reason,
        ),
        ClientCommand::Retry {
            chat_id,
            message_id,
        } => {
            let project_id = project_of(shared, &chat_id);
            let prompt = {
                let docs = shared.docs.lock().unwrap();
                docs.get(&chat_id).and_then(|doc| {
                    let ix = doc.index_of(&message_id)?;
                    let text = (0..ix)
                        .rev()
                        .filter_map(|i| doc.read_message(i))
                        .find(|m| m.role == Role::User)
                        .map(|m| m.plain_text())?;
                    let _ = doc.set_status(ix, MessageStatus::Cancelled);
                    let _ = doc.push_command("retry", json!({ "messageId": message_id }), DEVICE);
                    doc.commit();
                    Some(text)
                })
            };
            if let Some(text) = prompt {
                spawn_turn(shared, project_id, chat_id, new_id(), text, true);
            }
        }
        ClientCommand::NewChat {
            project_id,
            chat_id,
        } => {
            add_doc(shared, ChatDoc::new(&chat_id, &project_id));
            shared.chats.lock().unwrap().push(Chat {
                id: chat_id.clone(),
                project_id,
                title: "New chat".into(),
                is_pinned: false,
                is_system: false,
                unread: false,
                last_message_at: now_ms(),
            });
            emit_chats(shared);
            let _ = shared.events.send(SyncEvent::ChatSnapshot {
                chat_id,
                messages: Vec::new(),
            });
        }
        ClientCommand::CloseChat { chat_id } => {
            shared
                .chats
                .lock()
                .unwrap()
                .retain(|c| c.id != chat_id || c.is_pinned);
            emit_chats(shared);
        }
        ClientCommand::MarkRead { chat_id } => mark_read(shared, &chat_id),
        ClientCommand::SetModel {
            project_id,
            model,
            effort,
        } => {
            {
                let mut projects = shared.projects.lock().unwrap();
                if let Some(p) = projects.iter_mut().find(|p| p.id == project_id) {
                    p.model = model;
                    p.effort = effort;
                }
            }
            emit_projects(shared);
        }
        ClientCommand::MachineStart => {
            let shared = shared.clone();
            tokio::spawn(async move {
                wake_machine(&shared).await;
            });
        }
        ClientCommand::MachineStop => {
            set_machine(shared, MachineState::Stopped, Some("Stopped by you"), None);
        }
        ClientCommand::Pair { .. } => {
            let _ = shared.events.send(SyncEvent::Paired {
                device_id: DEVICE.into(),
            });
        }
        ClientCommand::CreateProject { name } => {
            let id = new_id();
            let icon = name
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_else(|| "Ω".into());
            shared.projects.lock().unwrap().push(Project {
                id: id.clone(),
                name: name.clone(),
                icon,
                color: "#8b7cf6".into(),
                state: BoxState::Awake,
                state_detail: None,
                wake_progress: None,
                model: MODELS[0].into(),
                effort: Effort::High,
                spend_today_usd: 0.0,
                spend_cap_usd: 25.0,
            });
            let chat_id = new_id();
            shared.chats.lock().unwrap().push(Chat {
                id: chat_id.clone(),
                project_id: id.clone(),
                title: name,
                is_pinned: true,
                is_system: false,
                unread: false,
                last_message_at: now_ms(),
            });
            add_doc(shared, ChatDoc::new(&chat_id, &id));
            emit_projects(shared);
            emit_chats(shared);
            let _ = shared.events.send(SyncEvent::ChatSnapshot {
                chat_id,
                messages: Vec::new(),
            });
        }
        ClientCommand::FetchBlob {
            project_id,
            key,
            save,
        } => {
            let body = format!(
                "# {key}\n\nFixture blob for project {project_id}.\n\nThe relay source downloads the real bytes from GET /blob/{{project}}/{{key}}.\n"
            );
            let saved_to = if save {
                let path = super::credentials::downloads_dir().join(key.replace('/', "_"));
                std::fs::write(&path, &body).ok().map(|_| path)
            } else {
                None
            };
            let _ = shared.events.send(SyncEvent::Blob {
                project_id,
                key,
                mime: "text/markdown".into(),
                bytes: Arc::new(body.into_bytes()),
                saved_to,
            });
        }
        ClientCommand::WatchBrowser { on: true, .. } => {
            let _ = shared.events.send(SyncEvent::Notice(
                "Fixture has no browser stream; the relay source shows the box's JPEG frames here."
                    .into(),
            ));
        }
        ClientCommand::WatchBrowser { on: false, .. } => {}
        ClientCommand::RefreshConnectors => emit_connectors(shared),
        ClientCommand::ConnectorConnect { provider, token } => {
            // Mock relay: the "OAuth dance" completes by itself after a moment.
            let shared = shared.clone();
            tokio::spawn(async move {
                sleep(Duration::from_millis(700)).await;
                let label = {
                    let mut view = shared.connectors.lock().unwrap();
                    let label = view
                        .providers
                        .iter()
                        .find(|p| p.provider == provider)
                        .map(|p| p.label.clone())
                        .unwrap_or_else(|| provider.clone());
                    let label = format!(
                        "{label} · {}",
                        if token.is_some() { "token" } else { "mock" }
                    );
                    view.items.retain(|c| c.provider != provider);
                    view.items.push(Connector {
                        id: new_id(),
                        provider: provider.clone(),
                        label: label.clone(),
                        scopes: Vec::new(),
                        status: ConnectorStatus::Connected,
                        last_error: None,
                        mock: true,
                    });
                    label
                };
                emit_connectors(&shared);
                let _ = shared
                    .events
                    .send(SyncEvent::Notice(format!("{label} connected (fixture).")));
            });
        }
        ClientCommand::ConnectorDisconnect { id } => {
            shared
                .connectors
                .lock()
                .unwrap()
                .items
                .retain(|c| c.id != id);
            emit_connectors(shared);
        }
        ClientCommand::RoutinesRefresh { project_id } => emit_routines(shared, &project_id),
        ClientCommand::RoutineCreate {
            project_id,
            name,
            prompt,
            chat_id,
            trigger,
        } => {
            let next_run_at = match &trigger {
                RoutineTrigger::Cron { .. } => Some(now_ms() as i64 + 3_600_000),
                RoutineTrigger::Webhook { .. } | RoutineTrigger::Event { .. } => None,
            };
            let fire_url = match &trigger {
                RoutineTrigger::Webhook { .. } => {
                    Some("http://127.0.0.1:7700/routines/<id>/fire?secret=…".into())
                }
                RoutineTrigger::Cron { .. } | RoutineTrigger::Event { .. } => None,
            };
            shared.routines.lock().unwrap().push(Routine {
                id: new_id(),
                project_id: project_id.clone(),
                chat_id,
                name,
                prompt,
                trigger,
                enabled: true,
                created_at: now_ms() as i64,
                last_run_at: None,
                next_run_at,
                fire_url,
            });
            emit_routines(shared, &project_id);
        }
        ClientCommand::RoutineSetEnabled {
            project_id,
            routine_id,
            enabled,
        } => {
            {
                let mut routines = shared.routines.lock().unwrap();
                if let Some(r) = routines.iter_mut().find(|r| r.id == routine_id) {
                    r.enabled = enabled;
                    r.next_run_at = match (&r.trigger, enabled) {
                        (RoutineTrigger::Cron { .. }, true) => Some(now_ms() as i64 + 3_600_000),
                        (RoutineTrigger::Cron { .. }, false)
                        | (RoutineTrigger::Webhook { .. }, _)
                        | (RoutineTrigger::Event { .. }, _) => None,
                    };
                }
            }
            emit_routines(shared, &project_id);
        }
        ClientCommand::RoutineRunNow {
            project_id,
            routine_id,
        } => run_routine_now(shared, &project_id, &routine_id),
        ClientCommand::RoutineRuns { routine_id, .. } => emit_routine_runs(shared, &routine_id),
        ClientCommand::RoutineDelete {
            project_id,
            routine_id,
        } => {
            shared
                .routines
                .lock()
                .unwrap()
                .retain(|r| r.id != routine_id);
            shared.routine_runs.lock().unwrap().remove(&routine_id);
            emit_routines(shared, &project_id);
        }
        ClientCommand::SkillsRefresh => emit_skills(shared),
        ClientCommand::SkillSave { mut skill } => {
            {
                let mut skills = shared.skills.lock().unwrap();
                let now = now_ms() as i64;
                skill.updated_at = now;
                if skill.slug.is_empty() {
                    skill.slug = arbos_core::skill::slugify(&skill.name);
                }
                match skills
                    .iter_mut()
                    .find(|s| s.id == skill.id && !skill.id.is_empty())
                {
                    Some(existing) => {
                        skill.version = existing.version + 1;
                        skill.created_at = existing.created_at;
                        *existing = skill;
                    }
                    None => {
                        skill.id = new_id();
                        skill.version = 1;
                        skill.created_at = now;
                        skills.push(skill);
                    }
                }
            }
            emit_skills(shared);
        }
        ClientCommand::SkillDelete { id } => {
            shared.skills.lock().unwrap().retain(|s| s.id != id);
            emit_skills(shared);
        }
        ClientCommand::RefreshChannels => emit_channels(shared),
        ClientCommand::ChannelPairNew {
            project_id,
            chat_id,
        } => {
            // Mock relay: mint a code now; "the user" pairs a Telegram chat with it after a moment.
            let code = format!("{:06}", now_ms() % 1_000_000);
            {
                let mut view = shared.channels.lock().unwrap();
                view.pairing = Some(ChannelPairing {
                    code: code.clone(),
                    project_id: project_id.clone(),
                    chat_id: chat_id.clone().unwrap_or_else(|| CHAT_COMMS_MAIN.into()),
                    expires_at: now_ms() + 10 * 60 * 1000,
                });
            }
            emit_channels(shared);
            let shared = shared.clone();
            tokio::spawn(async move {
                sleep(Duration::from_secs(6)).await;
                {
                    let mut view = shared.channels.lock().unwrap();
                    if view.pairing.as_ref().map(|p| p.code.as_str()) != Some(code.as_str()) {
                        return;
                    }
                    view.pairing = None;
                    view.bindings.insert(
                        0,
                        ChannelBinding {
                            id: new_id(),
                            platform: "telegram".into(),
                            external_chat_id: "8812".into(),
                            external_user_id: "8812".into(),
                            project_id: project_id.clone(),
                            chat_id: chat_id.unwrap_or_else(|| CHAT_COMMS_MAIN.into()),
                            label: "Telegram · DM · Jacob".into(),
                            bound_at: now_ms(),
                        },
                    );
                }
                emit_channels(&shared);
                let _ = shared.events.send(SyncEvent::Notice(
                    "Telegram · DM · Jacob paired with /pair (fixture).".into(),
                ));
            });
        }
        ClientCommand::ChannelUnbind { id } => {
            shared
                .channels
                .lock()
                .unwrap()
                .bindings
                .retain(|b| b.id != id);
            emit_channels(shared);
        }
        ClientCommand::LocalRefresh => emit_local(shared),
        ClientCommand::LocalSetAllowReads { device_id, allow } => {
            if let Some(d) = shared
                .local
                .lock()
                .unwrap()
                .devices
                .iter_mut()
                .find(|d| d.device_id == device_id)
            {
                d.allow_reads = allow;
            }
            emit_local(shared);
        }
        ClientCommand::LocalForget { device_id } => {
            shared
                .local
                .lock()
                .unwrap()
                .devices
                .retain(|d| d.device_id != device_id);
            emit_local(shared);
        }
    }
}

fn emit_channels(shared: &Shared) {
    let view = shared.channels.lock().unwrap().clone();
    let _ = shared.events.send(SyncEvent::Channels(view));
}

/// Settings → Channels demo: Beeper configured through Harness Comms, Slack in mock mode, the
/// others missing credentials; one WhatsApp chat already bound to Comms.
fn seed_channels() -> ChannelsView {
    let platform =
        |platform: &str, label: &str, state: ChannelPlatformState, detail: &str| ChannelPlatform {
            platform: platform.into(),
            label: label.into(),
            state,
            detail: detail.into(),
            inbound: String::new(),
        };
    ChannelsView {
        mock: false,
        platforms: vec![
            platform(
                "slack",
                "Slack",
                ChannelPlatformState::Mock,
                "ARBOS_CHANNELS_MOCK=1 · inbound via POST /channels/mock/inbound",
            ),
            platform(
                "telegram",
                "Telegram",
                ChannelPlatformState::Missing,
                "missing ARBOS_TELEGRAM_BOT_TOKEN",
            ),
            platform(
                "whatsapp",
                "WhatsApp (Cloud API)",
                ChannelPlatformState::Configured,
                "Cloud API webhook + messages",
            ),
            platform(
                "beeper",
                "Beeper (Harness Comms)",
                ChannelPlatformState::Configured,
                "via Harness Comms · https://comms.example.trycloudflare.com",
            ),
        ],
        bindings: vec![ChannelBinding {
            id: new_id(),
            platform: "whatsapp".into(),
            external_chat_id: "15555550123".into(),
            external_user_id: "15555550123".into(),
            project_id: PROJECT_COMMS.into(),
            chat_id: CHAT_COMMS_MAIN.into(),
            label: "WhatsApp · Jacob".into(),
            bound_at: now_ms().saturating_sub(3 * 86_400_000),
        }],
        pairing: None,
        loaded: true,
    }
}

fn emit_local(shared: &Shared) {
    let view = shared.local.lock().unwrap().clone();
    let _ = shared.events.send(SyncEvent::LocalDevices(view));
}

/// Settings → This Mac demo: one helper online, reads still ask.
fn seed_local() -> LocalView {
    LocalView {
        devices: vec![LocalDevice {
            device_id: "01JFIXTURELOCALMACBOOK0001".into(),
            name: "Jacob's MacBook Pro".into(),
            hostname: "jacobs-mbp".into(),
            os: "macos".into(),
            home: "/Users/jacob".into(),
            connected: true,
            last_seen: Some(now_ms() as i64 - 12_000),
            allow_reads: false,
        }],
        relay_url: "http://127.0.0.1:7700".into(),
        loaded: true,
    }
}

fn emit_connectors(shared: &Shared) {
    let view = shared.connectors.lock().unwrap().clone();
    let _ = shared.events.send(SyncEvent::Connectors(view));
}

/// Settings → Connectors demo: GitHub and Harness Comms connected, Google waiting for sign-in,
/// Slack not connected; everything flagged mock.
fn seed_connectors() -> ConnectorsView {
    let act = |name: &str, kind: &str| ConnectorAction {
        name: name.into(),
        kind: kind.into(),
    };
    let providers = vec![
        ConnectorProvider {
            provider: "google".into(),
            label: "Google (Gmail, Calendar)".into(),
            connect: "oauth".into(),
            oauth_configured: false,
            actions: vec![
                act("list_threads", "read"),
                act("search", "read"),
                act("read_message", "read"),
                act("create_draft", "write"),
                act("send", "send"),
                act("list_events", "read"),
                act("create_event", "send"),
            ],
        },
        ConnectorProvider {
            provider: "slack".into(),
            label: "Slack".into(),
            connect: "oauth_or_token".into(),
            oauth_configured: false,
            actions: vec![
                act("list_channels", "read"),
                act("read_channel", "read"),
                act("search", "read"),
                act("post_message", "send"),
            ],
        },
        ConnectorProvider {
            provider: "github".into(),
            label: "GitHub".into(),
            connect: "oauth_or_token".into(),
            oauth_configured: false,
            actions: vec![
                act("list_repos", "read"),
                act("list_issues", "read"),
                act("read_issue", "read"),
                act("list_prs", "read"),
                act("create_issue", "write"),
                act("comment", "send"),
            ],
        },
        ConnectorProvider {
            provider: "harness_comms".into(),
            label: "Harness Comms (Beeper + Gmail)".into(),
            connect: "token".into(),
            oauth_configured: true,
            actions: vec![
                act("status", "read"),
                act("search", "read"),
                act("read", "read"),
                act("search_conversations", "read"),
                act("read_chat", "read"),
                act("draft", "write"),
                act("send", "send"),
            ],
        },
    ];
    let items = vec![
        Connector {
            id: "01J9CONNGITHUB00000000000".into(),
            provider: "github".into(),
            label: "GitHub · unarbos".into(),
            scopes: vec!["repo".into(), "read:user".into()],
            status: ConnectorStatus::Connected,
            last_error: None,
            mock: true,
        },
        Connector {
            id: "01J9CONNGOOGLE00000000000".into(),
            provider: "google".into(),
            label: "Google (Gmail, Calendar)".into(),
            scopes: Vec::new(),
            status: ConnectorStatus::NeedsAuth,
            last_error: Some("consent not finished".into()),
            mock: true,
        },
        Connector {
            id: "01J9CONNHARNESS0000000000".into(),
            provider: "harness_comms".into(),
            label: "Harness Comms · arbos-comms".into(),
            scopes: vec![
                "read".into(),
                "search".into(),
                "draft".into(),
                "request_send".into(),
            ],
            status: ConnectorStatus::Connected,
            last_error: None,
            mock: true,
        },
    ];
    ConnectorsView {
        mock: true,
        providers,
        items,
        loaded: true,
    }
}

fn emit_routines(shared: &Shared, project_id: &str) {
    let routines: Vec<Routine> = shared
        .routines
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.project_id == project_id)
        .cloned()
        .collect();
    let _ = shared.events.send(SyncEvent::Routines {
        project_id: project_id.to_string(),
        routines,
    });
}

fn emit_routine_runs(shared: &Shared, routine_id: &str) {
    let runs = shared
        .routine_runs
        .lock()
        .unwrap()
        .get(routine_id)
        .cloned()
        .unwrap_or_default();
    let _ = shared.events.send(SyncEvent::RoutineRuns {
        routine_id: routine_id.to_string(),
        runs,
    });
}

/// What the box does on a `routine_run` push, scripted: the prompt lands in the chat as a user
/// message from `routine:<id>`, a reply streams, and the run row flips to `done`.
fn run_routine_now(shared: &Arc<Shared>, project_id: &str, routine_id: &str) {
    let Some(routine) = shared
        .routines
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.id == routine_id)
        .cloned()
    else {
        return;
    };
    let run_id = new_id();
    let started = now_ms() as i64;
    {
        let mut runs = shared.routine_runs.lock().unwrap();
        runs.entry(routine_id.to_string()).or_default().insert(
            0,
            RoutineRun {
                id: run_id.clone(),
                routine_id: routine_id.to_string(),
                started_at: started,
                finished_at: None,
                status: RoutineRunStatus::Running,
                message_id: None,
                note: Some("cause: manual".into()),
            },
        );
        let mut routines = shared.routines.lock().unwrap();
        if let Some(r) = routines.iter_mut().find(|r| r.id == routine_id) {
            r.last_run_at = Some(started);
        }
    }
    emit_routine_runs(shared, routine_id);
    emit_routines(shared, project_id);

    let message_id = new_id();
    let chat_id = routine.chat_id.clone();
    if !shared.docs.lock().unwrap().contains_key(&chat_id) {
        return;
    }
    {
        let docs = shared.docs.lock().unwrap();
        if let Some(doc) = docs.get(&chat_id) {
            let _ = doc.insert_message(&Message {
                id: message_id.clone(),
                chat_id: chat_id.clone(),
                role: Role::User,
                sender: format!("routine:{routine_id}"),
                status: MessageStatus::Done,
                turn: None,
                decision: None,
                subagent_id: None,
                subagent_name: None,
                in_reply_to: None,
                routine_name: Some(routine.name.clone()),
                usage: None,
                return_contract: None,
                waiting_user: false,
                waiting_approval: false,
                model: None,
                effort: None,
                created_at: now_ms(),
                completed_at: None,
                pending: false,
                parts: vec![Part::Text {
                    id: new_id(),
                    text: routine.prompt.clone(),
                }],
                hidden_parts: Vec::new(),
            });
            doc.commit();
        }
    }
    spawn_turn(
        shared,
        project_id.to_string(),
        chat_id.clone(),
        message_id.clone(),
        routine.prompt.clone(),
        true,
    );
    let shared = shared.clone();
    let routine_id = routine_id.to_string();
    tokio::spawn(async move {
        // Wait for the scripted reply to settle, then close the run with its message id.
        for _ in 0..40 {
            sleep(Duration::from_millis(250)).await;
            let settled = shared.running.lock().unwrap().get(&chat_id).is_none();
            if settled {
                break;
            }
        }
        let reply_id = with_doc(&shared, &chat_id, |doc| {
            doc.read_all()
                .into_iter()
                .rev()
                .find(|m| m.role == Role::Assistant)
                .map(|m| m.id)
        })
        .flatten();
        {
            let mut runs = shared.routine_runs.lock().unwrap();
            if let Some(run) = runs
                .get_mut(&routine_id)
                .and_then(|v| v.iter_mut().find(|r| r.id == run_id))
            {
                run.status = RoutineRunStatus::Done;
                run.finished_at = Some(now_ms() as i64);
                run.message_id = reply_id;
                run.note = Some("Here is how I would approach it.".into());
            }
        }
        emit_routine_runs(&shared, &routine_id);
    });
}

fn emit_skills(shared: &Shared) {
    let skills = shared.skills.lock().unwrap().clone();
    let _ = shared.events.send(SyncEvent::Skills(skills));
}

fn add_doc(shared: &Arc<Shared>, doc: ChatDoc) {
    let chat_id = doc.chat_id.clone();
    let tx = shared.doc_tx.clone();
    let id_for_cb = chat_id.clone();
    let sub = doc.doc.subscribe_root(Arc::new(move |event| {
        let touched = ChatDoc::touched(&event);
        if touched.list_changed || !touched.messages.is_empty() {
            let _ = tx.send(DocChanged {
                chat_id: id_for_cb.clone(),
                touched,
            });
        }
    }));
    shared.subscriptions.lock().unwrap().push(sub);
    shared.docs.lock().unwrap().insert(chat_id, doc);
}

fn project_of(shared: &Shared, chat_id: &str) -> Id {
    shared
        .chats
        .lock()
        .unwrap()
        .iter()
        .find(|c| c.id == chat_id)
        .map(|c| c.project_id.clone())
        .unwrap_or_else(|| PROJECT_ARBOS.to_string())
}

fn project_state(shared: &Shared, project_id: &str) -> BoxState {
    shared
        .projects
        .lock()
        .unwrap()
        .iter()
        .find(|p| p.id == project_id)
        .map(|p| p.state)
        .unwrap_or(BoxState::Awake)
}

fn project_model(shared: &Shared, project_id: &str) -> (String, Effort) {
    shared
        .projects
        .lock()
        .unwrap()
        .iter()
        .find(|p| p.id == project_id)
        .map(|p| (p.model.clone(), p.effort))
        .unwrap_or((MODELS[0].to_string(), Effort::High))
}

fn set_project_state(
    shared: &Shared,
    project_id: &str,
    state: BoxState,
    detail: Option<&str>,
    progress: Option<f32>,
) {
    {
        let mut projects = shared.projects.lock().unwrap();
        if let Some(p) = projects.iter_mut().find(|p| p.id == project_id) {
            p.state = state;
            p.state_detail = detail.map(str::to_string);
            p.wake_progress = progress;
        }
    }
    let _ = shared.events.send(SyncEvent::ProjectStatus {
        project_id: project_id.to_string(),
        state,
        detail: detail.map(str::to_string),
        progress,
    });
}

fn emit_machine(shared: &Shared) {
    let m = shared.machine.lock().unwrap().clone();
    let _ = shared.events.send(SyncEvent::Machine(m));
}

fn machine_down(shared: &Shared) -> bool {
    shared.machine.lock().unwrap().state.is_down()
}

fn set_machine(shared: &Shared, state: MachineState, detail: Option<&str>, progress: Option<f32>) {
    {
        let mut m = shared.machine.lock().unwrap();
        if m.state != state {
            m.since = Some(now_ms());
        }
        m.state = state;
        m.detail = detail.map(str::to_string);
        m.progress = progress;
    }
    emit_machine(shared);
}

/// Start the user's machine: a short progress ramp, then every queued
/// message in every workspace is released.
async fn wake_machine(shared: &Arc<Shared>) {
    if !machine_down(shared) {
        return;
    }
    let was_stopped = shared.machine.lock().unwrap().state == MachineState::Stopped;
    let steps = if was_stopped { 18 } else { 14 };
    for i in 0..=steps {
        set_machine(
            shared,
            MachineState::Waking,
            Some("Starting your machine…"),
            Some(i as f32 / steps as f32),
        );
        sleep(Duration::from_millis(180)).await;
    }
    set_machine(shared, MachineState::Awake, None, None);
    let docs = shared.docs.lock().unwrap();
    for doc in docs.values() {
        let mut changed = false;
        for ix in 0..doc.len() {
            if doc
                .read_message(ix)
                .map(|m| m.status == MessageStatus::Queued)
                .unwrap_or(false)
            {
                let _ = doc.set_status(ix, MessageStatus::Done);
                changed = true;
            }
        }
        if changed {
            doc.commit();
        }
    }
}

fn emit_projects(shared: &Shared) {
    let projects = shared.projects.lock().unwrap().clone();
    let _ = shared.events.send(SyncEvent::Projects(projects));
}

fn emit_project_docs(shared: &Shared) {
    let t = now_ms() as i64;
    let thread =
        |id: &str, name: &str, purpose: &str, summary: &str, status: &str, ago_min: i64| {
            ThreadInfo {
                id: id.into(),
                name: name.into(),
                purpose: purpose.into(),
                summary: summary.into(),
                status: status.into(),
                harness: "prime".into(),
                last_turn_at: Some(t - ago_min * 60_000),
            }
        };
    let arbos = ProjectDocView {
        threads: vec![
            thread("01J9THREADRAIL", "rail-layout", "Match the rail tiles to the mock", "Tiles 72 px, gap 10 px; cargo check clean.", "idle", 40),
            thread("01J9THREADDOCS", "docs-lookup", "Check GPUI list() alignment", "ListAlignment::Bottom confirmed; notes in docs/research/gpui.md.", "retired", 38),
        ],
        status: json!({ "box": "awake", "runningTurns": 0, "spendTodayUsd": 3.87 }),
        settings: json!({ "agentModel": MODELS[0], "agentEffort": "high" }),
        memory_t1: "- The UI is a messenger: rail left, chat center, nothing else at rest.\n- Jacob prefers conservative defaults chosen without asking.".into(),
        model_note: String::new(),
    };
    let _ = shared.events.send(SyncEvent::ProjectDoc {
        project_id: PROJECT_ARBOS.into(),
        view: arbos,
    });
}

fn emit_chats(shared: &Shared) {
    let chats = shared.chats.lock().unwrap().clone();
    let mut by_project: HashMap<Id, Vec<Chat>> = HashMap::new();
    for chat in chats {
        by_project
            .entry(chat.project_id.clone())
            .or_default()
            .push(chat);
    }
    for (project_id, chats) in by_project {
        let _ = shared.events.send(SyncEvent::Chats { project_id, chats });
    }
}

fn mark_read(shared: &Shared, chat_id: &str) {
    let changed = {
        let mut chats = shared.chats.lock().unwrap();
        match chats.iter_mut().find(|c| c.id == chat_id && c.unread) {
            Some(c) => {
                c.unread = false;
                true
            }
            None => false,
        }
    };
    if changed {
        emit_chats(shared);
    }
}

fn maybe_retitle(shared: &Shared, chat_id: &str, text: &str) {
    let changed = {
        let mut chats = shared.chats.lock().unwrap();
        match chats
            .iter_mut()
            .find(|c| c.id == chat_id && !c.is_pinned && c.title == "New chat")
        {
            Some(c) => {
                c.title = title_from(text);
                c.last_message_at = now_ms();
                true
            }
            None => false,
        }
    };
    if changed {
        emit_chats(shared);
    }
}

fn title_from(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(34).collect();
    if line.chars().count() > 34 {
        out.push('…');
    }
    if out.is_empty() {
        "New chat".into()
    } else {
        out
    }
}

async fn wake_project(shared: &Arc<Shared>, project_id: &str) {
    let state = project_state(shared, project_id);
    if !state.is_asleep() && state != BoxState::Error {
        return;
    }
    // Workspaces open fast once the machine is up: a brief "waking" dot, then awake.
    set_project_state(shared, project_id, BoxState::Waking, None, None);
    sleep(Duration::from_millis(600)).await;
    set_project_state(shared, project_id, BoxState::Awake, None, None);
    let chat_ids: Vec<Id> = shared
        .chats
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.project_id == project_id)
        .map(|c| c.id.clone())
        .collect();
    let docs = shared.docs.lock().unwrap();
    for chat_id in chat_ids {
        if let Some(doc) = docs.get(&chat_id) {
            for ix in 0..doc.len() {
                if doc
                    .read_message(ix)
                    .map(|m| m.status == MessageStatus::Queued)
                    .unwrap_or(false)
                {
                    let _ = doc.set_status(ix, MessageStatus::Done);
                }
            }
            doc.commit();
        }
    }
}

fn spawn_turn(
    shared: &Arc<Shared>,
    project_id: Id,
    chat_id: Id,
    user_message_id: Id,
    text: String,
    force_reply: bool,
) {
    let stop = Arc::new(AtomicBool::new(false));
    shared
        .running
        .lock()
        .unwrap()
        .insert(chat_id.clone(), stop.clone());
    let shared = shared.clone();
    tokio::spawn(async move {
        run_turn(
            &shared,
            &project_id,
            &chat_id,
            &user_message_id,
            &text,
            force_reply,
            stop,
        )
        .await;
        shared.running.lock().unwrap().remove(&chat_id);
    });
}

enum Script {
    Reply(&'static [&'static str]),
    Act {
        tools: &'static [(&'static str, &'static str, &'static str, u64)],
        diff: Option<(&'static str, u32, u32)>,
        summary: &'static [&'static str],
    },
    Browse {
        title: &'static str,
        url: &'static str,
        summary: &'static [&'static str],
    },
    /// `ask_user`: lead sentence, question, optional typed widget (design §10).
    Ask(&'static str, &'static str, Option<Widget>),
    Fail(&'static str, &'static str),
    /// S18: a draft that waits for approval before anything is sent.
    Draft {
        lead: &'static str,
        channel: &'static str,
        to: &'static str,
        subject: &'static str,
        body: &'static str,
    },
}

const REPLY_DEFAULT: &[&str] = &[
    "Here is how I would approach it.",
    "- Keep the rail narrow and the tabs flat, so the chat column gets the width.\n- Put the composer at the bottom once a chat has a message; before that, center it so a new tab feels like a blank page.\n- Treat every tool call as a card that folds, so long runs stay readable.",
    "If you want, I can turn this into a short checklist and start on the first item.",
];

const REPLY_SIMPLE: &[&str] = &[
    "Agreed. Two sections only: the rail on the left and the chat in the center.",
    "Nothing else at rest. Details open only when you ask for them.",
];

const REPLY_CODE: &[&str] = &[
    "The transcript is a bottom-aligned list with one row per markdown block:",
    "```rust\nlet list = ListState::new(0, ListAlignment::Bottom, px(320.0));\nlist(list.clone(), cx.processor(Self::render_row))\n```",
    "Rows are keyed by `{msg}#{part}.{block}` and re-spliced only when their bytes change, so a streaming reply never re-renders the settled prefix.",
];

const ACT_TOOLS: &[(&str, &str, &str, u64)] = &[
    (
        "read_file",
        "crates/arbos-mac/src/ui/rail.rs",
        "90 px rail, 72 px tiles, 11 px labels",
        420,
    ),
    (
        "grep",
        "TILE_W crates/",
        "3 matches in rail.rs, theme.rs",
        180,
    ),
    (
        "edit_file",
        "crates/arbos-mac/src/ui/rail.rs",
        "Set tile gap to 10 px",
        640,
    ),
    (
        "bash",
        "cargo check -p arbos-mac",
        "Finished `dev` profile in 4.1 s",
        1900,
    ),
];

const ACT_SUMMARY: &[&str] = &[
    "Done. The tiles were 68 px wide with an 8 px gap; the mock uses 72 px and 10 px.",
    "I changed the two constants in `rail.rs`, re-ran `cargo check`, and it is clean. The diff is above if you want to look.",
];

const BROWSE_SUMMARY: &[&str] = &[
    "Yes. `ListAlignment::Bottom` scrolls from the bottom up, like a chat log, and `ListSizingBehavior::Auto` lets the list fill the column.",
    "I saved the relevant page as `docs/research/gpui.md` in the sandbox.",
];

const ASK_LEAD: &str = "I can do that. One thing first:";
const ASK_QUESTION: &str =
    "Which date should I plan toward: the 1 December private beta, or the January public launch?";

const REPLY_ANSWERED: &[&str] =
    &["Noted. I will go with that and come back when there is something to look at."];
const DRAFT_LEAD: &str = "Here is the draft. Nothing goes out until you approve the card below.";
const DRAFT_TO: &str = "priya@northlight.vc";
const DRAFT_SUBJECT: &str = "Arbos — October investor update";
const DRAFT_BODY: &str = "Hi Priya,\n\nQuick October update from Arbos:\n\n- The Mac beta is feature-complete; 14 external testers are on it this week.\n- Box boot is down to 2.1 s warm and 28 s cold.\n- We closed the design partner agreement with Affine.\n\nHappy to walk you through any of it on Thursday.\n\nBest,\nJacob";

fn pick_script(text: &str, force_reply: bool) -> Script {
    let t = text.to_lowercase();
    if force_reply {
        return Script::Reply(REPLY_ANSWERED);
    }
    if t.contains("draft") || t.contains("email") || t.contains("send ") {
        return Script::Draft {
            lead: DRAFT_LEAD,
            channel: "email",
            to: DRAFT_TO,
            subject: DRAFT_SUBJECT,
            body: DRAFT_BODY,
        };
    }
    if t.contains("fail") || t.contains("error") || t.contains("timeout") {
        return Script::Fail("sandbox_timeout", "Sandbox did not answer in 60 s");
    }
    if t.contains("which") || t.contains("choose") || t.contains("pick one") {
        return Script::Ask(
            ASK_LEAD,
            "Which build should the beta ship from?",
            Some(widget_choice()),
        );
    }
    if t.contains("select") || t.contains("several") || t.contains("platforms") {
        return Script::Ask(
            ASK_LEAD,
            "Which platforms are in the first wave?",
            Some(widget_multi()),
        );
    }
    if t.contains("when") || t.contains("date") {
        return Script::Ask(ASK_LEAD, "When does the beta open?", Some(widget_date()));
    }
    if t.contains("form") || t.contains("details") {
        return Script::Ask(ASK_LEAD, "Release details", Some(widget_form()));
    }
    if t.contains("name") || t.contains("call it") {
        return Script::Ask(
            ASK_LEAD,
            "What should this release be called?",
            Some(widget_text()),
        );
    }
    if t.contains("ask") || t.contains("launch") || t.contains("deadline") {
        return Script::Ask(ASK_LEAD, ASK_QUESTION, None);
    }
    if t.contains("browse") || t.contains("docs") || t.contains("open ") || t.contains("website") {
        return Script::Browse {
            title: "GPUI docs — ListAlignment",
            url: "https://docs.rs/gpui/latest/gpui/enum.ListAlignment.html",
            summary: BROWSE_SUMMARY,
        };
    }
    if t.contains("fix")
        || t.contains("check")
        || t.contains("run ")
        || t.contains("build")
        || t.contains("test")
        || t.contains("padding")
    {
        return Script::Act {
            tools: ACT_TOOLS,
            diff: Some(("crates/arbos-mac/src/ui/rail.rs", 6, 2)),
            summary: ACT_SUMMARY,
        };
    }
    if t.contains("simple") {
        return Script::Reply(REPLY_SIMPLE);
    }
    if t.contains("list") || t.contains("code") || t.contains("transcript") {
        return Script::Reply(REPLY_CODE);
    }
    Script::Reply(REPLY_DEFAULT)
}

async fn run_turn(
    shared: &Arc<Shared>,
    project_id: &str,
    chat_id: &str,
    user_message_id: &str,
    text: &str,
    force_reply: bool,
    stop: Arc<AtomicBool>,
) {
    sleep(Duration::from_millis(220)).await;
    with_doc(shared, chat_id, |doc| {
        if let Some(ix) = doc.index_of(user_message_id) {
            let _ = doc.set_field(ix, "pending", json!(false));
            doc.commit();
        }
    });

    if machine_down(shared) {
        wake_machine(shared).await;
    }
    if project_state(shared, project_id).is_asleep() {
        wake_project(shared, project_id).await;
    }

    let (model, effort) = project_model(shared, project_id);
    let assistant_id = new_id();
    let ix = with_doc(shared, chat_id, |doc| {
        let msg = Message {
            id: assistant_id.clone(),
            chat_id: chat_id.to_string(),
            role: Role::Assistant,
            sender: "agent".into(),
            status: MessageStatus::Streaming,
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
            model: Some(model.clone()),
            effort: Some(effort),
            created_at: now_ms(),
            completed_at: None,
            pending: false,
            parts: vec![Part::Status {
                id: new_id(),
                code: StatusKind::Deciding,
                message: "Arbos is deciding…".into(),
            }],
            hidden_parts: Vec::new(),
        };
        let ix = doc.insert_message(&msg).unwrap_or(0);
        doc.commit();
        ix
    })
    .unwrap_or(0);

    sleep(Duration::from_millis(700)).await;
    if stop.load(Ordering::SeqCst) {
        finish_cancelled(shared, chat_id, ix);
        return;
    }

    let script = pick_script(text, force_reply);
    let turn = match &script {
        Script::Reply(_) | Script::Ask(_, _, _) | Script::Fail(_, _) => TurnKind::Reply,
        Script::Act { .. } | Script::Draft { .. } => TurnKind::Act,
        Script::Browse { .. } => TurnKind::Spawn,
    };
    with_doc(shared, chat_id, |doc| {
        let _ = doc.set_field(ix, "turn", serde_json::to_value(turn).unwrap_or_default());
        doc.commit();
    });

    let finished = match script {
        Script::Reply(blocks) => {
            with_doc(shared, chat_id, |doc| {
                let _ = doc.replace_part(ix, 0, &empty_text());
                doc.commit();
            });
            stream_blocks(shared, chat_id, ix, 0, blocks, &stop).await
        }
        Script::Act {
            tools,
            diff,
            summary,
        } => {
            let mut first = true;
            let mut ok = true;
            for (name, input, out, ms) in tools {
                if stop.load(Ordering::SeqCst) {
                    ok = false;
                    break;
                }
                let call = ToolCall {
                    id: new_id(),
                    name: name.to_string(),
                    input: json!(input),
                };
                let pix = with_doc(shared, chat_id, |doc| {
                    let part = Part::Tool {
                        id: new_id(),
                        call: call.clone(),
                        output_summary: String::new(),
                        output_ref: None,
                        is_error: false,
                        status: Some(ToolStatus::Running),
                        duration_ms: None,
                        subagent_name: None,
                    };
                    let pix = if first {
                        let _ = doc.replace_part(ix, 0, &part);
                        0
                    } else {
                        doc.push_part(ix, &part).unwrap_or(0)
                    };
                    doc.commit();
                    pix
                })
                .unwrap_or(0);
                first = false;
                sleep(Duration::from_millis(*ms)).await;
                with_doc(shared, chat_id, |doc| {
                    let _ = doc.set_part_field(ix, pix, "outputSummary", json!(out));
                    let _ = doc.set_part_field(
                        ix,
                        pix,
                        "outputRef",
                        json!(format!("blob/{}/{}", project_id, call.id)),
                    );
                    let _ = doc.set_part_field(ix, pix, "status", json!("ok"));
                    let _ = doc.set_part_field(ix, pix, "durationMs", json!(ms));
                    doc.commit();
                });
                if *name == "edit_file" {
                    if let Some((path, added, removed)) = diff {
                        with_doc(shared, chat_id, |doc| {
                            let _ = doc.push_part(
                                ix,
                                &Part::Diff {
                                    id: new_id(),
                                    path: path.to_string(),
                                    added,
                                    removed,
                                    blob_ref: Some(format!("blob/{project_id}/diff-rail")),
                                },
                            );
                            doc.commit();
                        });
                    }
                }
            }
            if ok {
                let pix = with_doc(shared, chat_id, |doc| {
                    let pix = doc.push_part(ix, &empty_text()).unwrap_or(0);
                    doc.commit();
                    pix
                })
                .unwrap_or(0);
                stream_blocks(shared, chat_id, ix, pix, summary, &stop).await
            } else {
                false
            }
        }
        Script::Browse {
            title,
            url,
            summary,
        } => {
            let status_pix = with_doc(shared, chat_id, |doc| {
                let _ = doc.replace_part(
                    ix,
                    0,
                    &Part::Status {
                        id: new_id(),
                        code: StatusKind::Working,
                        message: "Working in sandbox · starting browser · 00:00".into(),
                    },
                );
                let _ = doc.push_part(
                    ix,
                    &Part::Tool {
                        id: new_id(),
                        call: ToolCall {
                            id: new_id(),
                            name: "spawn_subagent".into(),
                            input: json!("docs-lookup · prime"),
                        },
                        output_summary: "Subagent started".into(),
                        output_ref: None,
                        is_error: false,
                        status: Some(ToolStatus::Ok),
                        duration_ms: Some(300),
                        subagent_name: Some("docs-lookup".into()),
                    },
                );
                doc.commit();
                0
            })
            .unwrap_or(0);
            sleep(Duration::from_millis(900)).await;
            let browser_pix = with_doc(shared, chat_id, |doc| {
                let pix = doc
                    .push_part(
                        ix,
                        &Part::Browser {
                            id: new_id(),
                            title: title.to_string(),
                            url: url.to_string(),
                            last_action: "navigate".into(),
                            live: true,
                        },
                    )
                    .unwrap_or(0);
                doc.commit();
                pix
            })
            .unwrap_or(0);
            let actions = ["scroll", "click \"Variants\"", "read page", "save notes"];
            let mut cancelled = false;
            for (i, action) in actions.iter().enumerate() {
                if stop.load(Ordering::SeqCst) {
                    cancelled = true;
                    break;
                }
                sleep(Duration::from_millis(1000)).await;
                let secs = i + 2;
                with_doc(shared, chat_id, |doc| {
                    let _ = doc.set_part_field(
                        ix,
                        status_pix,
                        "message",
                        json!(format!("Working in sandbox · {} · 00:{:02}", action, secs)),
                    );
                    let _ = doc.set_part_field(ix, browser_pix, "lastAction", json!(action));
                    doc.commit();
                });
            }
            if cancelled {
                false
            } else {
                with_doc(shared, chat_id, |doc| {
                    let _ = doc.set_part_field(ix, browser_pix, "live", json!(false));
                    let _ = doc.set_part_field(
                        ix,
                        status_pix,
                        "message",
                        json!("Worked in sandbox · 4 actions · 00:06"),
                    );
                    let _ = doc.push_part(
                        ix,
                        &Part::File {
                            id: new_id(),
                            path: "docs/research/gpui.md".into(),
                            lines: 212,
                            blob_ref: Some(format!("blob/{project_id}/gpui-notes")),
                        },
                    );
                    doc.commit();
                });
                let pix = with_doc(shared, chat_id, |doc| {
                    let pix = doc.push_part(ix, &empty_text()).unwrap_or(0);
                    doc.commit();
                    pix
                })
                .unwrap_or(0);
                stream_blocks(shared, chat_id, ix, pix, summary, &stop).await
            }
        }
        Script::Ask(lead, question, widget) => {
            with_doc(shared, chat_id, |doc| {
                let _ = doc.replace_part(ix, 0, &empty_text());
                doc.commit();
            });
            let done = stream_blocks(shared, chat_id, ix, 0, &[lead], &stop).await;
            if done {
                let widget = widget.clone();
                with_doc(shared, chat_id, |doc| {
                    let _ = doc.push_part(ix, &question_part(question, widget, None));
                    // Legacy status row, as agentd still writes it next to the question part.
                    let _ = doc.push_part(
                        ix,
                        &Part::Status {
                            id: new_id(),
                            code: StatusKind::AskUser,
                            message: question.to_string(),
                        },
                    );
                    doc.commit();
                });
            }
            done
        }
        Script::Draft {
            lead,
            channel,
            to,
            subject,
            body,
        } => {
            with_doc(shared, chat_id, |doc| {
                let _ = doc.replace_part(ix, 0, &empty_text());
                doc.commit();
            });
            let done = stream_blocks(shared, chat_id, ix, 0, &[lead], &stop).await;
            if done {
                let approval_id = new_id();
                with_doc(shared, chat_id, |doc| {
                    let _ = doc.push_part(
                        ix,
                        &approval_card(
                            &approval_id,
                            channel,
                            to,
                            subject,
                            body,
                            ApprovalState::Pending,
                            None,
                        ),
                    );
                    let _ = doc.push_part(
                        ix,
                        &Part::Status {
                            id: new_id(),
                            code: StatusKind::WaitingApproval,
                            message: format!("Send {channel} to {to}: {subject}"),
                        },
                    );
                    let _ = doc.set_field(ix, "waitingApproval", json!(true));
                    doc.commit();
                });
            }
            done
        }
        Script::Fail(code, message) => {
            sleep(Duration::from_millis(900)).await;
            with_doc(shared, chat_id, |doc| {
                let _ = doc.replace_part(
                    ix,
                    0,
                    &Part::Error {
                        id: new_id(),
                        code: code.to_string(),
                        message: message.to_string(),
                    },
                );
                let _ = doc.set_status(ix, MessageStatus::Error);
                doc.commit();
            });
            mark_unread_if_hidden(shared, chat_id);
            return;
        }
    };

    if finished {
        with_doc(shared, chat_id, |doc| {
            let _ = doc.set_status(ix, MessageStatus::Done);
            doc.commit();
        });
        mark_unread_if_hidden(shared, chat_id);
    } else {
        finish_cancelled(shared, chat_id, ix);
    }
}

fn opt(id: &str, label: &str) -> ChoiceOption {
    ChoiceOption {
        id: id.into(),
        label: label.into(),
    }
}

fn widget_choice() -> Widget {
    Widget::Choice {
        options: vec![
            opt("main", "main"),
            opt("release-1.2", "release-1.2"),
            opt("nightly", "nightly"),
        ],
    }
}

fn widget_multi() -> Widget {
    Widget::MultiSelect {
        options: vec![
            opt("macos", "macOS"),
            opt("ios", "iOS"),
            opt("web", "Web"),
            opt("windows", "Windows"),
        ],
    }
}

fn widget_text() -> Widget {
    Widget::Text {
        placeholder: Some("e.g. Juniper".into()),
        multiline: false,
    }
}

fn widget_date() -> Widget {
    Widget::Date {
        min: Some("2026-10-15".into()),
        max: Some("2027-03-31".into()),
    }
}

fn widget_form() -> Widget {
    let field = |id: &str, label: &str, kind: FieldKind, required: bool| FormField {
        id: id.into(),
        label: label.into(),
        kind,
        options: Vec::new(),
        required,
    };
    Widget::Form {
        fields: vec![
            field("version", "Version", FieldKind::Text, true),
            FormField {
                options: vec![opt("beta", "Beta"), opt("stable", "Stable")],
                ..field("channel", "Channel", FieldKind::Choice, true)
            },
            field("seats", "Seats", FieldKind::Number, false),
            field("freeze", "Code freeze", FieldKind::Date, false),
            field("notify", "Email testers", FieldKind::Bool, false),
        ],
    }
}

/// A `question` part as agentd writes it; `answer` makes it an answered card.
fn question_part(
    question: &str,
    widget: Option<Widget>,
    answer: Option<serde_json::Value>,
) -> Part {
    let answered = answer.is_some();
    Part::Question {
        id: new_id(),
        question_id: new_id(),
        question: question.into(),
        widget,
        answer,
        state: if answered {
            QuestionState::Answered
        } else {
            QuestionState::Open
        },
        answered_at: answered.then(now_ms),
    }
}

fn empty_text() -> Part {
    Part::Text {
        id: new_id(),
        text: String::new(),
    }
}

/// One `approval` part in the shape `arbos_core::approval` owns.
fn approval_card(
    approval_id: &str,
    channel: &str,
    to: &str,
    subject: &str,
    body: &str,
    state: ApprovalState,
    decided_at: Option<u64>,
) -> Part {
    let now = now_ms();
    Part::Approval {
        id: new_id(),
        approval_id: approval_id.to_string(),
        title: format!("Send {channel} to {to}: {subject}"),
        summary: body
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(160)
            .collect(),
        risk: ApprovalRisk::Send,
        payload: json!({ "kind": "draft_message", "channel": channel, "to": to, "subject": subject, "body": body }),
        state,
        expires_at: Some(decided_at.unwrap_or(now) + 24 * 60 * 60_000),
        decided_at,
        decided_by: decided_at.map(|_| DEVICE.to_string()),
        reason: None,
    }
}

/// S18 in the fixture: flip the card, mirror the command, then let the "agent" report
/// the outcome as a follow-up turn (what the box's `[Arbos]` notice turn does).
fn decide_approval(
    shared: &Arc<Shared>,
    chat_id: Id,
    approval_id: String,
    state: ApprovalState,
    reason: Option<String>,
) {
    let kind = match state {
        ApprovalState::Approved => "approve",
        ApprovalState::Rejected => "reject",
        ApprovalState::Pending | ApprovalState::Expired => return,
    };
    let found = with_doc(shared, &chat_id, |doc| {
        let mut payload = json!({ "approvalId": approval_id });
        if let Some(r) = reason.as_deref().filter(|r| !r.trim().is_empty()) {
            payload["reason"] = json!(r);
        }
        let _ = doc.push_command(kind, payload, DEVICE);
        let hit = (0..doc.len()).rev().find_map(|ix| {
            let m = doc.read_message(ix)?;
            let pix = m.parts.iter().position(|p| {
                matches!(p, Part::Approval { approval_id: a, state: ApprovalState::Pending, .. } if *a == approval_id)
            })?;
            let title = match &m.parts[pix] {
                Part::Approval { title, payload, .. } => (title.clone(), payload.clone()),
                _ => return None,
            };
            Some((ix, pix, title))
        });
        if let Some((ix, pix, _)) = &hit {
            let _ = doc.set_part_field(*ix, *pix, "state", json!(state));
            let _ = doc.set_part_field(*ix, *pix, "decidedAt", json!(now_ms()));
            let _ = doc.set_part_field(*ix, *pix, "decidedBy", json!(DEVICE));
            if let Some(r) = reason.as_deref().filter(|r| !r.trim().is_empty()) {
                let _ = doc.set_part_field(*ix, *pix, "reason", json!(r));
            }
            let _ = doc.set_field(*ix, "waitingApproval", json!(false));
        }
        doc.commit();
        hit
    })
    .flatten();
    let Some((_, _, (title, payload))) = found else {
        return;
    };
    let to = payload
        .get("to")
        .and_then(|v| v.as_str())
        .unwrap_or("them")
        .to_string();
    let channel = payload
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("message")
        .to_string();
    let blocks: Vec<String> = match state {
        ApprovalState::Approved => vec![format!(
            "Approved. I recorded the {channel} to {to} in this box's outbox; no {channel} connector is configured yet, so it was not delivered externally."
        )],
        ApprovalState::Rejected => vec![format!(
            "Understood, nothing was sent{}. Tell me what to change in \"{title}\" and I will redraft it.",
            reason
                .as_deref()
                .filter(|r| !r.trim().is_empty())
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        )],
        ApprovalState::Pending | ApprovalState::Expired => return,
    };
    let project_id = project_of(shared, &chat_id);
    let shared = shared.clone();
    tokio::spawn(async move {
        run_outcome_turn(&shared, &project_id, &chat_id, &approval_id, blocks).await;
    });
}

async fn run_outcome_turn(
    shared: &Arc<Shared>,
    project_id: &str,
    chat_id: &str,
    approval_id: &str,
    blocks: Vec<String>,
) {
    sleep(Duration::from_millis(400)).await;
    let (model, effort) = project_model(shared, project_id);
    let ix = with_doc(shared, chat_id, |doc| {
        let mut msg = Message {
            id: new_id(),
            chat_id: chat_id.to_string(),
            role: Role::Assistant,
            sender: "agent".into(),
            status: MessageStatus::Streaming,
            turn: Some(TurnKind::Reply),
            decision: Some("reply".into()),
            subagent_id: None,
            subagent_name: None,
            in_reply_to: None,
            usage: None,
            return_contract: None,
            waiting_user: false,
            waiting_approval: false,
            routine_name: None,
            model: Some(model),
            effort: Some(effort),
            created_at: now_ms(),
            completed_at: None,
            pending: false,
            parts: vec![empty_text()],
            hidden_parts: Vec::new(),
        };
        msg.parts.push(Part::Status {
            id: new_id(),
            code: StatusKind::Working,
            message: format!("Following up on approval {approval_id}"),
        });
        let ix = doc.insert_message(&msg).unwrap_or(0);
        doc.commit();
        ix
    })
    .unwrap_or(0);
    let refs: Vec<&str> = blocks.iter().map(String::as_str).collect();
    let stop = AtomicBool::new(false);
    stream_blocks(shared, chat_id, ix, 0, &refs, &stop).await;
    with_doc(shared, chat_id, |doc| {
        let _ = doc.set_status(ix, MessageStatus::Done);
        doc.commit();
    });
    mark_unread_if_hidden(shared, chat_id);
}

fn finish_cancelled(shared: &Arc<Shared>, chat_id: &str, ix: usize) {
    with_doc(shared, chat_id, |doc| {
        let _ = doc.push_part(
            ix,
            &Part::Status {
                id: new_id(),
                code: StatusKind::Note,
                message: "Stopped by you".into(),
            },
        );
        let _ = doc.set_status(ix, MessageStatus::Cancelled);
        doc.commit();
    });
}

fn mark_unread_if_hidden(shared: &Arc<Shared>, chat_id: &str) {
    let open = shared.open_chat.lock().unwrap().clone();
    if open.as_deref() == Some(chat_id) {
        return;
    }
    {
        let mut chats = shared.chats.lock().unwrap();
        if let Some(c) = chats.iter_mut().find(|c| c.id == chat_id) {
            c.unread = true;
            c.last_message_at = now_ms();
        }
    }
    emit_chats(shared);
}

/// Stream markdown blocks into one text part in 120 ms frames of a few words.
/// Returns false when stopped.
async fn stream_blocks(
    shared: &Arc<Shared>,
    chat_id: &str,
    ix: usize,
    pix: usize,
    blocks: &[&str],
    stop: &AtomicBool,
) -> bool {
    let mut full = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            full.push_str("\n\n");
        }
        full.push_str(block);
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut words = 0;
    for piece in full.split_inclusive(' ') {
        current.push_str(piece);
        words += 1;
        if words >= 3 {
            chunks.push(std::mem::take(&mut current));
            words = 0;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    for chunk in chunks {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        with_doc(shared, chat_id, |doc| {
            let _ = doc.append_text(ix, pix, &chunk);
            doc.commit();
        });
        sleep(Duration::from_millis(STREAM_FRAME_MS)).await;
    }
    true
}

fn with_doc<R>(shared: &Arc<Shared>, chat_id: &str, f: impl FnOnce(&ChatDoc) -> R) -> Option<R> {
    let docs = shared.docs.lock().unwrap();
    docs.get(chat_id).map(f)
}

// ---------------------------------------------------------------------------
// Seed data: the mock conversation plus one chat per rendering state.
// ---------------------------------------------------------------------------

fn seed_projects() -> Vec<Project> {
    vec![
        Project {
            id: PROJECT_AFFINE.into(),
            name: "Affine".into(),
            icon: "△".into(),
            color: "#8b7cf6".into(),
            state: BoxState::HibernatedLight,
            state_detail: None,
            wake_progress: None,
            model: MODELS[1].into(),
            effort: Effort::Medium,
            spend_today_usd: 0.42,
            spend_cap_usd: 25.0,
        },
        Project {
            id: PROJECT_COMMS.into(),
            name: "Comms".into(),
            icon: "Ψ".into(),
            color: "#8b7cf6".into(),
            state: BoxState::StoppedByUser,
            state_detail: None,
            wake_progress: None,
            model: MODELS[0].into(),
            effort: Effort::Low,
            spend_today_usd: 1.10,
            spend_cap_usd: 25.0,
        },
        Project {
            id: PROJECT_ARBOS.into(),
            name: "Arbos".into(),
            icon: "Ω".into(),
            color: "#8b7cf6".into(),
            state: BoxState::Awake,
            state_detail: None,
            wake_progress: None,
            model: MODELS[0].into(),
            effort: Effort::High,
            spend_today_usd: 3.87,
            spend_cap_usd: 25.0,
        },
    ]
}

fn seed_skills() -> Vec<Skill> {
    let t = now_ms() as i64;
    let skill = |slug: &str,
                 name: &str,
                 description: &str,
                 tags: &[&str],
                 when_to_use: &str,
                 body: &str,
                 origin: Option<&str>,
                 shared: bool,
                 ago_h: i64| Skill {
        id: new_id(),
        slug: slug.into(),
        name: name.into(),
        description: description.into(),
        tags: tags.iter().map(|s| s.to_string()).collect(),
        when_to_use: when_to_use.into(),
        body: body.into(),
        origin_project_id: origin.map(str::to_string),
        shared,
        version: 1,
        created_at: t - ago_h * 3_600_000,
        updated_at: t - ago_h * 3_600_000,
    };
    vec![
        skill(
            "gpui-list-alignment",
            "GPUI bottom-aligned lists",
            "How to build a chat-style list that scrolls from the bottom in GPUI.",
            &["gpui", "rust", "ui"],
            "Any transcript or log view that must stick to its newest row.",
            "## Steps\n\n1. `ListState::new(0, ListAlignment::Bottom, px(320.0))`.\n2. Key rows by `{msg}#{part}.{block}` so streaming appends re-splice only the changed tail.\n3. Use `ListSizingBehavior::Auto` so the list fills the column.\n\n## Pitfalls\n\n- Never rebuild the whole list on a text append; use `splice`.\n",
            Some(PROJECT_ARBOS),
            true,
            30,
        ),
        skill(
            "weekly-update",
            "Weekly team update",
            "Template and checklist for the Monday update mail.",
            &["comms", "writing"],
            "When asked to draft, send, or summarise the weekly update.",
            "## Shape\n\n- Three bullets: shipped, in flight, blocked.\n- One line per project, link to the thread.\n- Under 200 words; no adjectives.\n\n## Send\n\nRead `updates/<date>.md`, fill the template, ask before sending to more than ten people.\n",
            Some(PROJECT_COMMS),
            false,
            52,
        ),
        skill(
            "release-checklist",
            "Mac beta release checklist",
            "Everything to check before tagging a Mac beta build.",
            &["release", "mac", "process"],
            "Before any tag, TestFlight upload, or launch-plan question.",
            "## Before tagging\n\n1. `cargo fmt --all --check` and `cargo clippy --workspace -- -D warnings` are clean.\n2. Fixture and relay sources both start and pair.\n3. Changelog entry written.\n\n## After\n\n- Notarise, upload, post the build link in the launch chat.\n",
            None,
            true,
            6,
        ),
    ]
}

fn seed_chats() -> Vec<Chat> {
    let t = now_ms();
    let chat = |id: &str, project: &str, title: &str, pinned: bool, ago_min: u64| Chat {
        id: id.into(),
        project_id: project.into(),
        title: title.into(),
        is_pinned: pinned,
        is_system: false,
        unread: false,
        last_message_at: t.saturating_sub(ago_min * 60_000),
    };
    vec![
        chat(CHAT_ARBOS_MAIN, PROJECT_ARBOS, "Arbos", true, 3),
        chat(
            CHAT_ARBOS_RESEARCH,
            PROJECT_ARBOS,
            "Interface research",
            false,
            40,
        ),
        chat(
            CHAT_ARBOS_LAUNCH,
            PROJECT_ARBOS,
            "Launch planning",
            false,
            95,
        ),
        chat(CHAT_ARBOS_WIDGETS, PROJECT_ARBOS, "Beta release", false, 12),
        chat(
            CHAT_ARBOS_DRAFTS,
            PROJECT_ARBOS,
            "Investor update",
            false,
            12,
        ),
        chat(CHAT_AFFINE_MAIN, PROJECT_AFFINE, "Affine", true, 60 * 26),
        chat(CHAT_COMMS_MAIN, PROJECT_COMMS, "Comms", true, 60 * 5),
    ]
}

pub const ROUTINE_STANDUP: &str = "01J9ROUTINE0STANDUP0000000";
pub const ROUTINE_PRS: &str = "01J9ROUTINE0PRS00000000000";
pub const ROUTINE_DEPLOY: &str = "01J9ROUTINE0DEPLOY00000000";

fn seed_routines() -> Vec<Routine> {
    let t = now_ms() as i64;
    let hour = 3_600_000;
    vec![
        Routine {
            id: ROUTINE_STANDUP.into(),
            project_id: PROJECT_ARBOS.into(),
            chat_id: CHAT_ARBOS_LAUNCH.into(),
            name: "Morning stand-up".into(),
            prompt: "Summarise what changed in the launch plan since yesterday and list today's three most important items.".into(),
            trigger: RoutineTrigger::Cron {
                expr: "0 9 * * 1-5".into(),
                tz: "Europe/Berlin".into(),
            },
            enabled: true,
            created_at: t - 9 * 24 * hour,
            last_run_at: Some(t - 5 * hour),
            next_run_at: Some(t + 19 * hour),
            fire_url: None,
        },
        Routine {
            id: ROUTINE_PRS.into(),
            project_id: PROJECT_ARBOS.into(),
            chat_id: CHAT_ARBOS_RESEARCH.into(),
            name: "Open PR digest".into(),
            prompt: "Check the open pull requests and tell me which ones are waiting on me.".into(),
            trigger: RoutineTrigger::Cron {
                expr: "0 * * * *".into(),
                tz: "UTC".into(),
            },
            enabled: false,
            created_at: t - 3 * 24 * hour,
            last_run_at: Some(t - 26 * hour),
            next_run_at: None,
            fire_url: None,
        },
        Routine {
            id: ROUTINE_DEPLOY.into(),
            project_id: PROJECT_ARBOS.into(),
            chat_id: CHAT_ARBOS_MAIN.into(),
            name: "Deploy finished".into(),
            prompt: "A deploy webhook fired. Read the trigger context and tell me whether anything needs attention.".into(),
            trigger: RoutineTrigger::Webhook {
                secret: "fixture".into(),
            },
            enabled: true,
            created_at: t - 24 * hour,
            last_run_at: Some(t - 40 * 60_000),
            next_run_at: None,
            fire_url: Some(format!(
                "http://127.0.0.1:7700/routines/{ROUTINE_DEPLOY}/fire?secret=…"
            )),
        },
    ]
}

fn seed_routine_runs() -> HashMap<Id, Vec<RoutineRun>> {
    let t = now_ms() as i64;
    let hour = 3_600_000;
    let run = |routine: &str, ago_ms: i64, dur_ms: i64, status: RoutineRunStatus, note: &str| {
        RoutineRun {
            id: new_id(),
            routine_id: routine.into(),
            started_at: t - ago_ms,
            finished_at: matches!(status, RoutineRunStatus::Done | RoutineRunStatus::Failed)
                .then_some(t - ago_ms + dur_ms),
            status,
            message_id: Some(new_id()),
            note: Some(note.into()),
        }
    };
    let mut out = HashMap::new();
    out.insert(
        ROUTINE_STANDUP.to_string(),
        vec![
            run(ROUTINE_STANDUP, 5 * hour, 41_000, RoutineRunStatus::Done, "Two items moved: the beta invite copy is done, the pricing page slipped to Thursday."),
            run(ROUTINE_STANDUP, 29 * hour, 38_000, RoutineRunStatus::Done, "No changes since yesterday; today: invite copy, pricing page, press list."),
            run(ROUTINE_STANDUP, 53 * hour, 7_000, RoutineRunStatus::Failed, "the box did not finish this run within 30 min"),
            run(ROUTINE_STANDUP, 77 * hour, 44_000, RoutineRunStatus::Done, "Launch date confirmed for 1 December; three items listed."),
        ],
    );
    out.insert(
        ROUTINE_PRS.to_string(),
        vec![run(
            ROUTINE_PRS,
            26 * hour,
            52_000,
            RoutineRunStatus::Done,
            "Three PRs open; #412 and #415 wait on your review.",
        )],
    );
    out.insert(
        ROUTINE_DEPLOY.to_string(),
        vec![
            run(
                ROUTINE_DEPLOY,
                40 * 60_000,
                12_000,
                RoutineRunStatus::Done,
                "api 1.4.2 deployed cleanly; error rate unchanged.",
            ),
            run(
                ROUTINE_DEPLOY,
                6 * hour,
                15_000,
                RoutineRunStatus::Done,
                "web 2.0.9 deployed; one new warning in the logs, not urgent.",
            ),
        ],
    );
    out
}

struct Seeder<'a> {
    doc: &'a ChatDoc,
    t: u64,
}

impl Seeder<'_> {
    fn user(&mut self, text: &str) {
        self.t += 45_000;
        let _ = self.doc.insert_message(&Message {
            id: new_id(),
            chat_id: self.doc.chat_id.clone(),
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
            created_at: self.t,
            completed_at: Some(self.t),
            pending: false,
            parts: vec![Part::Text {
                id: new_id(),
                text: text.into(),
            }],
            hidden_parts: Vec::new(),
        });
    }

    fn assistant(&mut self, turn: TurnKind, status: MessageStatus, parts: Vec<Part>) {
        self.t += 20_000;
        let _ = self.doc.insert_message(&Message {
            id: new_id(),
            chat_id: self.doc.chat_id.clone(),
            role: Role::Assistant,
            sender: "agent".into(),
            status,
            turn: Some(turn),
            decision: None,
            subagent_id: None,
            subagent_name: None,
            in_reply_to: None,
            routine_name: None,
            usage: None,
            return_contract: None,
            waiting_user: false,
            waiting_approval: false,
            model: Some(MODELS[0].into()),
            effort: Some(Effort::High),
            created_at: self.t,
            completed_at: Some(self.t + 9_000),
            pending: false,
            parts,
            hidden_parts: Vec::new(),
        });
    }
}

fn text(s: &str) -> Part {
    Part::Text {
        id: new_id(),
        text: s.into(),
    }
}

fn tool(name: &str, input: &str, out: &str, ms: u64) -> Part {
    Part::Tool {
        id: new_id(),
        call: ToolCall {
            id: new_id(),
            name: name.into(),
            input: json!(input),
        },
        output_summary: out.into(),
        output_ref: Some(format!("blob/seed/{}", new_id())),
        is_error: false,
        status: Some(ToolStatus::Ok),
        duration_ms: Some(ms),
        subagent_name: None,
    }
}

fn seed_docs(shared: &Arc<Shared>) {
    let base = now_ms().saturating_sub(3 * 60 * 60_000);

    let main = ChatDoc::new(CHAT_ARBOS_MAIN, PROJECT_ARBOS);
    {
        let mut s = Seeder {
            doc: &main,
            t: base,
        };
        s.user("Let's refine the agent workspace.");
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![text(
                "Each agent has its own space, with separate tabs for the conversations happening in parallel.",
            )],
        );
        s.user("Keep it simple.");
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![text(
                "A compact agent rail, clear chat tabs, and everything else within reach.",
            )],
        );
        main.commit();
    }
    add_doc(shared, main);

    let research = ChatDoc::new(CHAT_ARBOS_RESEARCH, PROJECT_ARBOS);
    {
        let mut s = Seeder {
            doc: &research,
            t: base + 600_000,
        };
        s.user("Check the rail tile sizes against the mock and fix the padding.");
        s.assistant(
            TurnKind::Act,
            MessageStatus::Done,
            vec![
                tool("read_file", "crates/arbos-mac/src/ui/rail.rs", "90 px rail, 68 px tiles, 8 px gap", 410),
                tool("grep", "TILE_W crates/", "3 matches in rail.rs, theme.rs", 160),
                tool("edit_file", "crates/arbos-mac/src/ui/rail.rs", "Tiles 72 px, gap 10 px", 620),
                Part::Diff {
                    id: new_id(),
                    path: "crates/arbos-mac/src/ui/rail.rs".into(),
                    added: 6,
                    removed: 2,
                    blob_ref: Some("blob/seed/diff-rail".into()),
                },
                tool("bash", "cargo check -p arbos-mac", "Finished `dev` profile in 4.1 s", 4100),
                text("Tiles were 68 px with an 8 px gap. I set them to 72 px and 10 px to match the mock, and `cargo check` is clean."),
            ],
        );
        s.user("Open the GPUI docs and see if list() supports bottom alignment.");
        s.assistant(
            TurnKind::Spawn,
            MessageStatus::Done,
            vec![
                Part::Status {
                    id: new_id(),
                    code: StatusKind::Working,
                    message: "Worked in sandbox · 4 actions · 00:41".into(),
                },
                Part::Tool {
                    id: new_id(),
                    call: ToolCall {
                        id: new_id(),
                        name: "spawn_subagent".into(),
                        input: json!("docs-lookup · prime"),
                    },
                    output_summary: "Subagent finished".into(),
                    output_ref: None,
                    is_error: false,
                    status: Some(ToolStatus::Ok),
                    duration_ms: Some(41_000),
                    subagent_name: Some("docs-lookup".into()),
                },
                Part::Browser {
                    id: new_id(),
                    title: "GPUI docs — ListAlignment".into(),
                    url: "https://docs.rs/gpui/latest/gpui/enum.ListAlignment.html".into(),
                    last_action: "read page".into(),
                    live: false,
                },
                Part::File {
                    id: new_id(),
                    path: "docs/research/gpui.md".into(),
                    lines: 212,
                    blob_ref: Some("blob/seed/gpui-notes".into()),
                },
                text("Yes. `ListAlignment::Bottom` scrolls from the bottom up, like a chat log. Notes are in `docs/research/gpui.md`."),
            ],
        );
        research.commit();
    }
    add_doc(shared, research);

    let launch = ChatDoc::new(CHAT_ARBOS_LAUNCH, PROJECT_ARBOS);
    {
        let mut s = Seeder {
            doc: &launch,
            t: base + 1_200_000,
        };
        s.user("Draft a launch plan for the Mac beta.");
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![
                text(ASK_LEAD),
                Part::Status {
                    id: new_id(),
                    code: StatusKind::AskUser,
                    message: ASK_QUESTION.into(),
                },
            ],
        );
        launch.commit();
    }
    add_doc(shared, launch);

    // Every widget type once (design §10): one answered choice, then five open cards.
    let widgets = ChatDoc::new(CHAT_ARBOS_WIDGETS, PROJECT_ARBOS);
    {
        let mut s = Seeder {
            doc: &widgets,
            t: base + 2_400_000,
        };
        s.user("Let's get the beta out this month.");
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![
                text("Happy to. One thing first:"),
                question_part(
                    "Dark or light theme for the first screenshots?",
                    Some(Widget::Choice {
                        options: vec![opt("dark", "Dark"), opt("light", "Light")],
                    }),
                    Some(json!("dark")),
                ),
            ],
        );
        s.user("Answer: Dark");
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![
                text("Dark it is. A few more things so I can set everything up:"),
                question_part(
                    "Which build should the beta ship from?",
                    Some(widget_choice()),
                    None,
                ),
            ],
        );
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![question_part(
                "Which platforms are in the first wave?",
                Some(widget_multi()),
                None,
            )],
        );
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![question_part(
                "What should this release be called?",
                Some(widget_text()),
                None,
            )],
        );
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![question_part(
                "When does the beta open?",
                Some(widget_date()),
                None,
            )],
        );
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![question_part("Release details", Some(widget_form()), None)],
        );
        widgets.commit();
    }
    add_doc(shared, widgets);
    // S18: one approved card (already sent) and one pending card waiting on the user.
    let drafts = ChatDoc::new(CHAT_ARBOS_DRAFTS, PROJECT_ARBOS);
    {
        let mut s = Seeder {
            doc: &drafts,
            t: base + 2_000_000,
        };
        s.user("Email Sam to confirm Tuesday's demo at 10:00.");
        let sam_body = "Hi Sam,\n\nConfirming the Arbos demo on Tuesday at 10:00 (your office, 45 min). I'll bring the Mac build and a sandbox you can poke at.\n\nSee you then,\nJacob";
        let decided = s.t + 60_000;
        s.assistant(
            TurnKind::Act,
            MessageStatus::Done,
            vec![
                text("Here is the confirmation. Approve the card to send it."),
                approval_card(
                    "01J9APPROVAL0SAM0000000000",
                    "email",
                    "sam@affine.pro",
                    "Tuesday demo — confirmed for 10:00",
                    sam_body,
                    ApprovalState::Approved,
                    Some(decided),
                ),
            ],
        );
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![text(
                "Sent. The email to sam@affine.pro went out at 10:14; it is in this box's outbox and the audit trail.",
            )],
        );
        s.user("Now draft the October investor update for Priya.");
        s.assistant(
            TurnKind::Act,
            MessageStatus::Done,
            vec![
                text(DRAFT_LEAD),
                approval_card(
                    "01J9APPROVAL0PRIYA00000000",
                    "email",
                    DRAFT_TO,
                    DRAFT_SUBJECT,
                    DRAFT_BODY,
                    ApprovalState::Pending,
                    None,
                ),
                Part::Status {
                    id: new_id(),
                    code: StatusKind::WaitingApproval,
                    message: format!("Send email to {DRAFT_TO}: {DRAFT_SUBJECT}"),
                },
            ],
        );
        let _ = drafts.set_field(drafts.len() - 1, "waitingApproval", json!(true));
        drafts.commit();
    }
    add_doc(shared, drafts);

    let affine = ChatDoc::new(CHAT_AFFINE_MAIN, PROJECT_AFFINE);
    {
        let mut s = Seeder {
            doc: &affine,
            t: base.saturating_sub(23 * 60 * 60_000),
        };
        s.user("Summarise yesterday's design review.");
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![text(
                "Three decisions: keep the canvas infinite, move export to the share sheet, and drop the legacy sidebar. Two open questions are in the thread.",
            )],
        );
        affine.commit();
    }
    add_doc(shared, affine);

    let comms = ChatDoc::new(CHAT_COMMS_MAIN, PROJECT_COMMS);
    {
        let mut s = Seeder {
            doc: &comms,
            t: base.saturating_sub(2 * 60 * 60_000),
        };
        s.user("Send the weekly update to the team.");
        s.assistant(
            TurnKind::Act,
            MessageStatus::Error,
            vec![
                tool("read_file", "updates/2026-10-13.md", "1.2 KB", 300),
                Part::Error {
                    id: new_id(),
                    code: "sandbox_timeout".into(),
                    message: "Sandbox did not answer in 60 s".into(),
                },
            ],
        );
        comms.commit();
    }
    add_doc(shared, comms);
}
