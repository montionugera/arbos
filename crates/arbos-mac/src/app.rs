//! `AppState`: the one shared entity. Views observe it (`cx.observe`) and
//! subscribe to the narrow [`AppEvent::TranscriptText`] for streaming appends
//! (zeron-study §4.4 step 4). It owns the sync channel; nothing here touches
//! GPUI elements.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{App, Context, EventEmitter, RenderImage, Task};
use gpui_tokio::Tokio;
use tokio::sync::mpsc;

use crate::auth;
use crate::model::{
    new_id, now_ms, AttachmentKind, BoxState, ChannelsView, Chat, ChatKind, ConnectorsView, Effort,
    Id, Link, LocalView, Machine, Message, MessageStatus, Part, Project, ProjectDocView,
    QuestionState, Role, Routine, RoutineRun, RoutineTrigger, Skill, StatusKind, VaultNewItem,
    VaultRequestStatus, VaultView,
};
use crate::model::{AccountView, Person};
use crate::sync::{ClientCommand, SyncEvent, SyncSource};
use crate::ui::notify::{Gate, Notifier, NotifyEvent, NotifyKind};

/// How often held notification batches are re-checked.
const NOTIFY_TICK_MS: u64 = 5_000;
use crate::update::{self, UpdateCommand, UpdateEvent, UpdatePhase, UpdateState};

/// How the sign-in screen signs this device in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInMode {
    /// "Sign in with GitHub" through the relay's OAuth flow.
    GitHub,
    /// The relay runs `ARBOS_AUTH_MODE=paircode`: one-time code from its log.
    PairCode,
}

/// Sign-in screen state (first launch, after sign-out, after a revoked token).
#[derive(Clone, Debug)]
pub struct Pairing {
    pub relay_url: String,
    pub mode: SignInMode,
    pub busy: bool,
    pub error: Option<String>,
}

/// A downloaded blob shown in the Details drawer.
#[derive(Clone, Debug)]
pub struct BlobView {
    pub mime: String,
    pub bytes: Arc<Vec<u8>>,
    pub saved_to: Option<PathBuf>,
}

/// Transient bottom-right message.
#[derive(Clone, Debug)]
pub struct Notice {
    pub text: String,
    pub at: u64,
}

pub const NOTICE_TTL_MS: u64 = 6_000;

#[derive(Clone, Debug)]
pub enum AppEvent {
    /// Pure text append on a streaming message; only the transcript re-syncs.
    TranscriptText { chat_id: Id },
    /// Anything else in a chat changed.
    TranscriptChanged { chat_id: Id },
    /// F11: speech-to-text finished for a `transcribe` request.
    VoiceTranscript {
        request_id: u64,
        text: String,
        error: Option<String>,
        mock: bool,
    },
    /// F11: text-to-speech finished for a `speak` request.
    VoiceSpeech {
        request_id: u64,
        bytes: Arc<Vec<u8>>,
        mime: String,
        error: Option<String>,
        mock: bool,
    },
    /// F11: the speaker icon on a reply was clicked.
    ReplaySpeech { message_id: Id },
}

/// What the composer should look like for the active chat (§3.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerMode {
    /// Send arrow. Sending is always allowed: the agent multitasks, and a
    /// running turn shows as a small indicator with its own Stop.
    Send,
    /// S8: the agent asked a question.
    Answer,
    /// S18: an approval card is pending; the composer waits for Approve/Reject.
    Approve,
}

/// Per-chat UI state kept across tab switches.
#[derive(Clone, Debug, Default)]
pub struct ChatUi {
    /// S3 → S9: the composer dropped to the bottom; it never returns.
    pub composer_dropped: bool,
    /// When the drop animation started (ms since epoch).
    pub drop_started_at: Option<u64>,
}

pub struct AppState {
    pub source_name: &'static str,
    pub pairing: Option<Pairing>,
    pub device_id: Option<String>,
    /// Signed-in account (`/auth/token` user, then `/me`).
    pub account: Option<AccountView>,
    /// The tree mark (the macOS app icon), drawn on the sign-in screen.
    pub mark: Option<Arc<RenderImage>>,
    /// People chats (D72): the new-chat sheet, directory results, typing, avatars.
    pub new_chat_open: bool,
    pub people_query: String,
    pub people_results: Vec<Person>,
    pub people_busy: bool,
    pub people_error: Option<String>,
    /// handle → until (ms) per chat; shown as "@sam is typing…".
    pub typing: HashMap<Id, (String, u64)>,
    /// Decoded avatars by `avatar_url`; `None` while a fetch is in flight.
    pub avatars: HashMap<String, Option<Arc<RenderImage>>>,
    /// A DM just started from the sheet: select it when it arrives.
    awaiting_dm: Option<String>,
    /// The sheet invites into this project instead of starting a DM (step 3).
    pub invite_target: Option<Id>,
    /// Avatar reply reaction window per project (ms): plays once after a turn.
    pub avatar_reply_until: HashMap<Id, u64>,
    /// Avatar picker open for this project.
    pub avatar_picker: Option<Id>,
    /// Last time we told the room we are typing, per chat.
    typing_sent_at: HashMap<Id, u64>,
    /// Set by a fresh sign-in: with no projects yet, create "Home" and land in it.
    first_project_pending: bool,
    pub projects: Vec<Project>,
    /// Picker contents; empty = the fixture's static list.
    pub models: Vec<String>,
    pub project_docs: HashMap<Id, ProjectDocView>,
    /// The skills library (F5), as last fetched from the source.
    pub skills: Vec<Skill>,
    pub chats: HashMap<Id, Vec<Chat>>,
    pub messages: HashMap<Id, Vec<Message>>,
    pub chat_ui: HashMap<Id, ChatUi>,
    pub blobs: HashMap<String, BlobView>,
    /// Decoded inline thumbnails of image attachments, by blob key (F10).
    pub thumbnails: HashMap<String, Arc<RenderImage>>,
    /// Thumbnail fetches in flight or failed (never asked twice).
    thumbnails_requested: HashSet<String>,
    pub browser_frame: Option<Arc<RenderImage>>,
    pub browser_frames_seen: u64,
    pub notices: Vec<Notice>,
    pub selected_project: Option<Id>,
    pub active_tab: HashMap<Id, Id>,
    pub link: Link,
    pub rail_collapsed: bool,
    pub right_panel_open: bool,
    pub details_open: Option<DetailsTarget>,
    pub new_project_open: bool,
    /// Debug: per-message "Show details" disclosures in the transcript.
    pub show_details: bool,
    /// The user's one machine (projects are workspaces on it).
    pub machine: Machine,
    pub machine_hover: bool,
    /// Settings → Connectors (F1).
    pub connectors: ConnectorsView,
    /// Settings → Vault (F8).
    pub vault: VaultView,
    /// Settings → Routines, per project (F3).
    pub routines: HashMap<Id, Vec<Routine>>,
    /// Run history per routine, newest first.
    pub routine_runs: HashMap<Id, Vec<RoutineRun>>,
    /// System notifications (D49); the Shell flips `window_active`.
    pub notifier: Notifier,
    pub window_active: bool,
    /// Settings → Channels (F7).
    pub channels: ChannelsView,
    /// In-app update (D42): what the rail button and settings show.
    pub update: UpdateState,
    /// Settings → This Mac (F9).
    pub local: LocalView,
    /// F11: replies are spoken and a mic transcript sends at once.
    pub voice_mode: bool,
    /// F11: assistant messages whose speech is cached (speaker icon replays).
    pub spoken: HashSet<Id>,
    awaiting_new_project: bool,
    commands: mpsc::UnboundedSender<ClientCommand>,
    update_commands: mpsc::UnboundedSender<UpdateCommand>,
    _pump: Task<()>,
    _notify_tick: Task<()>,
    _update_pump: Task<()>,
}

/// What the Details drawer (stub) describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetailsTarget {
    Message { chat_id: Id, message_id: Id },
    Blob(String),
}

impl EventEmitter<AppEvent> for AppState {}

impl AppState {
    pub fn new(source: Box<dyn SyncSource>, cx: &mut Context<Self>) -> Self {
        let source_name = source.name();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<SyncEvent>();
        let (command_tx, command_rx) = mpsc::unbounded_channel::<ClientCommand>();

        let run = Tokio::spawn(cx, async move {
            if let Err(err) = source.run(event_tx, command_rx).await {
                eprintln!("sync source stopped: {err:#}");
            }
        });

        let pump = cx.spawn(async move |this, cx| {
            let _run = run;
            while let Some(event) = event_rx.recv().await {
                let applied = this.update(cx, |state, cx| state.apply(event, cx));
                if applied.is_err() {
                    break;
                }
            }
        });

        let notify_tick = cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(NOTIFY_TICK_MS))
                .await;
            if this.update(cx, |state, _| state.notify_tick()).is_err() {
                break;
            }
        });

        let update_state = UpdateState::new();
        let (update_event_tx, mut update_event_rx) = mpsc::unbounded_channel::<UpdateEvent>();
        let (update_command_tx, update_command_rx) = mpsc::unbounded_channel::<UpdateCommand>();
        let update_worker = Tokio::spawn(
            cx,
            update::run(update_state.clone(), update_command_rx, update_event_tx),
        );
        let update_pump = cx.spawn(async move |this, cx| {
            let _worker = update_worker;
            while let Some(event) = update_event_rx.recv().await {
                let applied = this.update(cx, |state, cx| state.apply_update(event, cx));
                if applied.is_err() {
                    break;
                }
            }
        });

        Self {
            source_name,
            pairing: None,
            account: None,
            mark: decode_image(include_bytes!(
                "../../../site/assets/logo/arbos-macos-icon-1024.png"
            ))
            .map(Arc::new),
            first_project_pending: false,
            new_chat_open: false,
            people_query: String::new(),
            people_results: Vec::new(),
            people_busy: false,
            people_error: None,
            typing: HashMap::new(),
            avatars: HashMap::new(),
            awaiting_dm: None,
            invite_target: None,
            avatar_reply_until: HashMap::new(),
            avatar_picker: None,
            typing_sent_at: HashMap::new(),
            device_id: None,
            projects: Vec::new(),
            models: Vec::new(),
            project_docs: HashMap::new(),
            skills: Vec::new(),
            chats: HashMap::new(),
            messages: HashMap::new(),
            chat_ui: HashMap::new(),
            blobs: HashMap::new(),
            thumbnails: HashMap::new(),
            thumbnails_requested: HashSet::new(),
            browser_frame: None,
            browser_frames_seen: 0,
            notices: Vec::new(),
            selected_project: None,
            active_tab: HashMap::new(),
            link: Link::Connecting,
            rail_collapsed: false,
            right_panel_open: false,
            details_open: None,
            new_project_open: false,
            show_details: false,
            machine: Machine::default(),
            machine_hover: false,
            connectors: ConnectorsView::default(),
            vault: VaultView::default(),
            routines: HashMap::new(),
            routine_runs: HashMap::new(),
            notifier: Notifier::new(),
            window_active: true,
            channels: ChannelsView::default(),
            update: update_state,
            local: LocalView::default(),
            voice_mode: false,
            spoken: HashSet::new(),
            awaiting_new_project: false,
            commands: command_tx,
            update_commands: update_command_tx,
            _pump: pump,
            _notify_tick: notify_tick,
            _update_pump: update_pump,
        }
    }

    fn apply_update(&mut self, event: UpdateEvent, cx: &mut Context<Self>) {
        match event {
            UpdateEvent::Checking => {
                if matches!(self.update.phase, UpdatePhase::Idle | UpdatePhase::UpToDate) {
                    self.update.phase = UpdatePhase::Checking;
                }
            }
            UpdateEvent::UpToDate => {
                self.update.last_checked_at = Some(now_ms());
                if !self.update.busy() {
                    self.update.phase = UpdatePhase::UpToDate;
                }
            }
            UpdateEvent::Available(available) => {
                let available = *available;
                self.update.last_checked_at = Some(now_ms());
                if !self.update.busy() {
                    let fresh = self.update.available() != Some(&available);
                    self.update.phase = UpdatePhase::Available(available.clone());
                    if fresh {
                        self.push_notice(format!(
                            "Arbos {} is ready to install — press Update in the rail.",
                            available.item.version
                        ));
                    }
                }
            }
            UpdateEvent::Progress { got, total } => {
                if let Some(available) = self.update.available().cloned() {
                    self.update.phase = UpdatePhase::Downloading {
                        available,
                        got,
                        total,
                    };
                }
            }
            UpdateEvent::Installing => {
                if let Some(available) = self.update.available().cloned() {
                    self.update.phase = UpdatePhase::Installing(available);
                }
            }
            UpdateEvent::Installed(installed) => {
                if let Some(available) = self.update.available().cloned() {
                    self.update.phase = UpdatePhase::Restarting(available);
                }
                // One frame shows "Restarting", then the relauncher takes over.
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(700))
                        .await;
                    let _ = this.update(cx, |state, cx| match update::relaunch(&installed) {
                        Ok(()) => cx.quit(),
                        Err(err) => {
                            state.update.phase = UpdatePhase::Failed {
                                message: format!("installed, but could not relaunch: {err:#}"),
                                available: state.update.available().cloned(),
                            };
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            UpdateEvent::Failed(message) => {
                self.update.phase = UpdatePhase::Failed {
                    available: self.update.available().cloned(),
                    message,
                };
            }
        }
        cx.notify();
    }

    pub fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if self.update.busy() {
            return;
        }
        let _ = self.update_commands.send(UpdateCommand::Check);
        cx.notify();
    }

    /// The rail button: download, verify, swap, relaunch in one click.
    pub fn install_update(&mut self, cx: &mut Context<Self>) {
        if self.update.busy() {
            return;
        }
        let Some(available) = self.update.available().cloned() else {
            return;
        };
        self.update.phase = UpdatePhase::Downloading {
            available,
            got: 0,
            total: None,
        };
        let _ = self.update_commands.send(UpdateCommand::Install);
        cx.notify();
    }

    fn apply(&mut self, event: SyncEvent, cx: &mut Context<Self>) {
        match event {
            SyncEvent::NeedsPairing { relay_url } => {
                let mode = self
                    .pairing
                    .as_ref()
                    .map(|p| p.mode)
                    .unwrap_or(SignInMode::GitHub);
                self.pairing = Some(Pairing {
                    relay_url,
                    mode,
                    busy: false,
                    error: None,
                });
                self.device_id = None;
                cx.notify();
            }
            SyncEvent::Paired { device_id } => {
                self.pairing = None;
                self.device_id = Some(device_id);
                // Land in a chat: if this account has no project yet, make "Home".
                self.first_project_pending = true;
                cx.notify();
            }
            SyncEvent::Account(account) => {
                self.account = account;
                cx.notify();
            }
            SyncEvent::PeopleSearch { query, results } => {
                if query == self.people_query {
                    self.people_results = results;
                    self.people_busy = false;
                    let urls: Vec<String> = self
                        .people_results
                        .iter()
                        .map(|p| p.avatar_url.clone())
                        .collect();
                    for url in urls {
                        self.want_avatar(url);
                    }
                }
                cx.notify();
            }
            SyncEvent::PeopleError(err) => {
                self.people_busy = false;
                self.people_error = Some(err);
                self.awaiting_dm = None;
                cx.notify();
            }
            SyncEvent::Typing {
                chat_id,
                handle,
                active,
            } => {
                if active {
                    self.typing.insert(chat_id, (handle, now_ms() + 6_000));
                } else {
                    self.typing.remove(&chat_id);
                }
                cx.notify();
            }
            SyncEvent::Avatar { url, bytes } => {
                let image = decode_image(&bytes).map(Arc::new);
                self.avatars.insert(url, image);
                cx.notify();
            }
            SyncEvent::PairingFailed(err) => {
                if let Some(p) = &mut self.pairing {
                    p.busy = false;
                    p.error = Some(err);
                }
                cx.notify();
            }
            SyncEvent::ChatCreated {
                project_id,
                local_id,
                chat,
            } => {
                let tabs = self.chats.entry(project_id.clone()).or_default();
                match tabs.iter_mut().find(|c| c.id == local_id) {
                    Some(slot) => *slot = chat.clone(),
                    None => tabs.push(chat.clone()),
                }
                if let Some(msgs) = self.messages.remove(&local_id) {
                    self.messages.insert(chat.id.clone(), msgs);
                }
                if let Some(ui) = self.chat_ui.remove(&local_id) {
                    self.chat_ui.insert(chat.id.clone(), ui);
                }
                if self.active_tab.get(&project_id) == Some(&local_id) {
                    self.active_tab.insert(project_id, chat.id.clone());
                    self.command(ClientCommand::OpenChat {
                        chat_id: chat.id.clone(),
                    });
                }
                cx.emit(AppEvent::TranscriptChanged { chat_id: chat.id });
                cx.notify();
            }
            SyncEvent::ProjectDoc { project_id, view } => {
                self.project_docs.insert(project_id, view);
                cx.notify();
            }
            SyncEvent::Blob {
                project_id,
                key,
                mime,
                bytes,
                saved_to,
            } => {
                if self.selected_project.as_deref() != Some(project_id.as_str()) {
                    return;
                }
                if let Some(path) = &saved_to {
                    self.push_notice(format!("Saved to {}", path.display()));
                }
                self.blobs.insert(
                    key.clone(),
                    BlobView {
                        mime,
                        bytes,
                        saved_to,
                    },
                );
                self.details_open = Some(DetailsTarget::Blob(key));
                cx.notify();
            }
            SyncEvent::Thumbnail { key, bytes } => {
                if let Some(image) = decode_image(&bytes) {
                    self.thumbnails.insert(key, Arc::new(image));
                    if let Some(chat_id) = self.active_chat_id() {
                        cx.emit(AppEvent::TranscriptChanged { chat_id });
                    }
                    cx.notify();
                }
            }
            SyncEvent::AttachmentSaved { path } => {
                self.push_notice(format!("Saved to {}", path.display()));
                crate::ui::attachments::reveal(&path);
                cx.notify();
            }
            SyncEvent::BrowserFrame { project_id, jpeg } => {
                if self.selected_project.as_deref() != Some(project_id.as_str()) {
                    return;
                }
                if let Some(image) = decode_image(&jpeg) {
                    self.browser_frame = Some(Arc::new(image));
                    self.browser_frames_seen += 1;
                    cx.notify();
                }
            }
            SyncEvent::Notice(text) => {
                self.push_notice(text);
                cx.notify();
            }
            SyncEvent::Models(models) => {
                self.models = models;
                cx.notify();
            }
            SyncEvent::Projects(projects) => {
                for p in &projects {
                    let crossed = p.spend_cap_usd > 0.0 && p.spend_today_usd >= p.spend_cap_usd;
                    let was_below = self
                        .project(&p.id)
                        .map(|old| old.spend_today_usd < old.spend_cap_usd)
                        .unwrap_or(false);
                    if crossed && was_below {
                        self.notify(
                            p.id.clone(),
                            None,
                            NotifyKind::SpendCapReached,
                            format!("${:.2} of ${:.2} today", p.spend_today_usd, p.spend_cap_usd),
                        );
                    }
                }
                for p in &projects {
                    for person in &p.people {
                        let url = person.avatar_url.clone();
                        self.want_avatar(url);
                    }
                }
                if let Some(handle) = self.awaiting_dm.clone() {
                    if let Some(space) = projects
                        .iter()
                        .find(|p| p.is_people() && p.people.iter().any(|x| x.handle == handle))
                    {
                        self.awaiting_dm = None;
                        self.new_chat_open = false;
                        let id = space.id.clone();
                        self.projects = projects.clone();
                        self.select_project(id, cx);
                    }
                }
                if self.first_project_pending {
                    self.first_project_pending = false;
                    if projects.is_empty() && !self.awaiting_new_project {
                        self.awaiting_new_project = true;
                        self.command(ClientCommand::CreateProject {
                            name: "Home".into(),
                        });
                    }
                }
                // A project this device just created becomes the selection.
                if self.awaiting_new_project {
                    let known: HashSet<Id> = self.projects.iter().map(|p| p.id.clone()).collect();
                    if let Some(fresh) = projects.iter().find(|p| !known.contains(&p.id)) {
                        self.awaiting_new_project = false;
                        let id = fresh.id.clone();
                        self.projects = projects.clone();
                        self.select_project(id, cx);
                    }
                }
                self.projects = projects;
                if self.selected_project.is_none() {
                    let pick = self
                        .projects
                        .iter()
                        .find(|p| p.name == "Arbos")
                        .or(self.projects.last())
                        .map(|p| p.id.clone());
                    if let Some(id) = pick {
                        self.select_project(id, cx);
                    }
                }
                cx.notify();
            }
            SyncEvent::Machine(machine) => {
                self.machine = machine;
                cx.notify();
            }
            SyncEvent::ProjectStatus {
                project_id,
                state,
                detail,
                progress,
            } => {
                let failed = state == BoxState::Error
                    && self
                        .project(&project_id)
                        .is_some_and(|p| p.state != BoxState::Error);
                if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
                    p.state = state;
                    p.state_detail = detail.clone();
                    p.wake_progress = progress;
                }
                if failed {
                    self.notify(
                        project_id,
                        None,
                        NotifyKind::WakeFailed,
                        detail.unwrap_or_else(|| "The box did not come up".into()),
                    );
                }
                cx.notify();
            }
            SyncEvent::Chats { project_id, chats } => {
                let active = self.active_tab.get(&project_id).cloned();
                let still_there = active
                    .as_ref()
                    .map(|a| chats.iter().any(|c| &c.id == a))
                    .unwrap_or(false);
                if !still_there {
                    if let Some(first) = chats.first() {
                        self.active_tab.insert(project_id.clone(), first.id.clone());
                    }
                }
                self.chats.insert(project_id, chats);
                cx.notify();
            }
            SyncEvent::ChatSnapshot { chat_id, messages } => {
                let dropped = !messages.is_empty();
                let ui = self.chat_ui.entry(chat_id.clone()).or_default();
                if dropped {
                    ui.composer_dropped = true;
                }
                self.messages.insert(chat_id.clone(), messages);
                self.request_thumbnails(&chat_id);
                cx.emit(AppEvent::TranscriptChanged { chat_id });
                cx.notify();
            }
            SyncEvent::MessageUpsert { chat_id, message } => {
                let message = *message;
                self.request_thumbnails_for(&chat_id, std::slice::from_ref(&message));
                let list = self.messages.entry(chat_id.clone()).or_default();
                let mut text_only = false;
                let mut appended = 0;
                let mut was_streaming = false;
                let mut had_open_question = false;
                let mut is_new = false;
                match list.iter_mut().find(|m| m.id == message.id) {
                    Some(existing) => {
                        let old_len = existing.text_len();
                        let new_len = message.text_len();
                        text_only = existing.status == message.status
                            && existing.parts.len() == message.parts.len()
                            && new_len > old_len
                            && same_shape(existing, &message);
                        appended = new_len.saturating_sub(old_len);
                        was_streaming = existing.is_streaming();
                        had_open_question = needs_input(existing);
                        *existing = message.clone();
                    }
                    None => {
                        is_new = true;
                        list.push(message.clone());
                    }
                }
                self.notify_for_message(
                    &chat_id,
                    &message,
                    was_streaming,
                    had_open_question,
                    is_new,
                );
                if text_only && appended > 0 {
                    cx.emit(AppEvent::TranscriptText { chat_id });
                } else {
                    cx.emit(AppEvent::TranscriptChanged { chat_id });
                    cx.notify();
                }
            }
            SyncEvent::Link(link) => {
                self.link = link;
                cx.notify();
            }
            SyncEvent::Connectors(view) => {
                self.connectors = view;
                cx.notify();
            }
            SyncEvent::ConnectorAuth { provider, auth_url } => {
                cx.open_url(&auth_url);
                self.push_notice(format!(
                    "Opened the browser to connect {provider}; the relay finishes the sign-in."
                ));
                cx.notify();
            }
            SyncEvent::Vault(view) => {
                self.vault = view;
                cx.notify();
            }
            SyncEvent::Routines {
                project_id,
                routines,
            } => {
                self.routines.insert(project_id, routines);
                cx.notify();
            }
            SyncEvent::RoutineRuns { routine_id, runs } => {
                self.routine_runs.insert(routine_id, runs);
                cx.notify();
            }
            SyncEvent::Skills(mut skills) => {
                skills.sort_by_key(|s| s.name.to_lowercase());
                self.skills = skills;
                cx.notify();
            }
            SyncEvent::Channels(view) => {
                self.channels = view;
                cx.notify();
            }
            SyncEvent::LocalDevices(view) => {
                self.local = view;
                cx.notify();
            }
            SyncEvent::Transcript {
                request_id,
                text,
                error,
                mock,
            } => {
                cx.emit(AppEvent::VoiceTranscript {
                    request_id,
                    text,
                    error,
                    mock,
                });
            }
            SyncEvent::Speech {
                request_id,
                bytes,
                mime,
                error,
                mock,
            } => {
                cx.emit(AppEvent::VoiceSpeech {
                    request_id,
                    bytes,
                    mime,
                    error,
                    mock,
                });
            }
        }
    }

    // ----- selectors -------------------------------------------------------

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn selected(&self) -> Option<&Project> {
        self.selected_project
            .as_deref()
            .and_then(|id| self.project(id))
    }

    pub fn tabs(&self, project_id: &str) -> &[Chat] {
        self.chats
            .get(project_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn active_chat(&self) -> Option<&Chat> {
        let project_id = self.selected_project.as_deref()?;
        let chat_id = self.active_tab.get(project_id)?;
        self.tabs(project_id).iter().find(|c| &c.id == chat_id)
    }

    pub fn active_chat_id(&self) -> Option<Id> {
        self.active_chat().map(|c| c.id.clone())
    }

    pub fn messages_of(&self, chat_id: &str) -> &[Message] {
        self.messages
            .get(chat_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn chat_is_streaming(&self, chat_id: &str) -> bool {
        self.messages_of(chat_id).iter().any(|m| m.is_streaming())
    }

    pub fn project_is_streaming(&self, project_id: &str) -> bool {
        self.tabs(project_id)
            .iter()
            .any(|c| self.chat_is_streaming(&c.id))
    }

    pub fn project_unread(&self, project_id: &str) -> usize {
        self.tabs(project_id).iter().filter(|c| c.unread).count()
    }

    /// S8: the last assistant message ended with an `ask_user` question and
    /// nothing has been sent since. A typed widget question is answered from
    /// its card, so it does not switch the composer.
    pub fn chat_needs_answer(&self, chat_id: &str) -> bool {
        match self.messages_of(chat_id).last() {
            Some(m) if m.role == Role::Assistant && !m.is_streaming() => {
                if m.has_question_part() {
                    m.open_questions()
                        .any(|p| matches!(p, Part::Question { widget: None, .. }))
                } else {
                    m.parts.iter().any(|p| {
                        matches!(
                            p,
                            Part::Status {
                                code: StatusKind::AskUser,
                                ..
                            }
                        )
                    })
                }
            }
            Some(_) | None => false,
        }
    }

    /// The newest open plain question's id in the last assistant message (S8 composer answer).
    fn open_plain_question_id(&self, chat_id: &str) -> Option<String> {
        let m = self.messages_of(chat_id).last()?;
        m.open_questions().rev().find_map(|p| match p {
            Part::Question {
                question_id,
                widget: None,
                ..
            } => Some(question_id.clone()),
            _ => None,
        })
    }

    /// S18: the newest assistant message carries an approval card nobody has decided yet.
    pub fn chat_needs_approval(&self, chat_id: &str) -> bool {
        match self.messages_of(chat_id).last() {
            Some(m) if m.role == Role::Assistant && !m.is_streaming() => {
                m.pending_approvals().next().is_some()
            }
            Some(_) | None => false,
        }
    }

    pub fn composer_mode(&self, chat_id: &str) -> ComposerMode {
        if self.chat_needs_answer(chat_id) {
            ComposerMode::Answer
        } else if self.chat_needs_approval(chat_id) {
            ComposerMode::Approve
        } else {
            ComposerMode::Send
        }
    }

    pub fn chat_ui(&self, chat_id: &str) -> ChatUi {
        self.chat_ui.get(chat_id).cloned().unwrap_or_default()
    }

    pub fn window_is_narrow(&self, width: f32) -> bool {
        width < crate::ui::theme::NARROW_WINDOW
    }

    // ----- commands --------------------------------------------------------

    fn command(&self, cmd: ClientCommand) {
        let _ = self.commands.send(cmd);
    }

    pub fn select_project(&mut self, project_id: Id, cx: &mut Context<Self>) {
        self.selected_project = Some(project_id.clone());
        if let Some(chat_id) = self.active_tab.get(&project_id).cloned() {
            self.command(ClientCommand::OpenChat { chat_id });
        } else if let Some(first) = self.tabs(&project_id).first() {
            let id = first.id.clone();
            self.active_tab.insert(project_id, id.clone());
            self.command(ClientCommand::OpenChat { chat_id: id });
        }
        cx.notify();
    }

    pub fn select_tab(&mut self, chat_id: Id, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        self.active_tab.insert(project_id, chat_id.clone());
        if let Some(chat) = self
            .chats
            .values_mut()
            .flat_map(|v| v.iter_mut())
            .find(|c| c.id == chat_id)
        {
            chat.unread = false;
        }
        self.command(ClientCommand::MarkRead {
            chat_id: chat_id.clone(),
        });
        self.command(ClientCommand::OpenChat { chat_id });
        cx.notify();
    }

    pub fn new_tab(&mut self, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        let chat_id = new_id();
        self.chats
            .entry(project_id.clone())
            .or_default()
            .push(Chat {
                id: chat_id.clone(),
                project_id: project_id.clone(),
                title: "New chat".into(),
                is_pinned: false,
                is_system: false,
                kind: ChatKind::Chat,
                participants: Vec::new(),
                unread: false,
                last_message_at: now_ms(),
            });
        self.messages.insert(chat_id.clone(), Vec::new());
        self.chat_ui.insert(chat_id.clone(), ChatUi::default());
        self.active_tab.insert(project_id.clone(), chat_id.clone());
        self.command(ClientCommand::NewChat {
            project_id,
            chat_id: chat_id.clone(),
        });
        self.command(ClientCommand::OpenChat { chat_id });
        cx.notify();
    }

    /// F6: a group chat hosted by the selected project with `participant` (and, when the active
    /// tab is already a group, the participant is added there instead).
    pub fn add_project_to_chat(&mut self, participant: Id, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        if let Some(group) = self.active_chat().filter(|c| c.is_group()).cloned() {
            if group.participants.contains(&participant) {
                return;
            }
            if let Some(slot) = self
                .chats
                .get_mut(&project_id)
                .and_then(|tabs| tabs.iter_mut().find(|c| c.id == group.id))
            {
                slot.participants.push(participant.clone());
            }
            self.command(ClientCommand::GroupAddParticipant {
                project_id,
                chat_id: group.id,
                participant,
            });
            cx.notify();
            return;
        }
        let name = self
            .projects
            .iter()
            .find(|p| p.id == participant)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "project".into());
        let chat_id = new_id();
        self.chats
            .entry(project_id.clone())
            .or_default()
            .push(Chat {
                id: chat_id.clone(),
                project_id: project_id.clone(),
                title: format!("Group · {name}"),
                is_pinned: false,
                is_system: false,
                kind: ChatKind::Group,
                participants: vec![project_id.clone(), participant.clone()],
                unread: false,
                last_message_at: now_ms(),
            });
        self.messages.insert(chat_id.clone(), Vec::new());
        self.chat_ui.insert(chat_id.clone(), ChatUi::default());
        self.active_tab.insert(project_id.clone(), chat_id.clone());
        self.command(ClientCommand::GroupCreate {
            project_id,
            chat_id: chat_id.clone(),
            participants: vec![participant],
            title: None,
        });
        self.command(ClientCommand::OpenChat { chat_id });
        cx.notify();
    }

    /// Returns false for the pinned tab (S3.8: it never closes).
    pub fn close_tab(&mut self, chat_id: Id, cx: &mut Context<Self>) -> bool {
        let Some(project_id) = self.selected_project.clone() else {
            return false;
        };
        let tabs = self.chats.entry(project_id.clone()).or_default();
        let Some(ix) = tabs.iter().position(|c| c.id == chat_id) else {
            return false;
        };
        if tabs[ix].is_pinned {
            return false;
        }
        tabs.remove(ix);
        if self.active_tab.get(&project_id) == Some(&chat_id) {
            let next = tabs
                .get(ix.saturating_sub(1))
                .or(tabs.first())
                .map(|c| c.id.clone());
            if let Some(next) = next {
                self.active_tab.insert(project_id.clone(), next.clone());
                self.command(ClientCommand::OpenChat { chat_id: next });
            }
        }
        self.command(ClientCommand::CloseChat { chat_id });
        cx.notify();
        true
    }

    pub fn send(&mut self, text: String, attachments: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.send_with(text, attachments, None, cx);
    }

    /// F11: a spoken message; `wav` is kept on the row as an `audio` attachment.
    pub fn send_voice(&mut self, text: String, wav: Arc<Vec<u8>>, cx: &mut Context<Self>) {
        self.send_with(text, Vec::new(), Some(wav), cx);
    }

    fn send_with(
        &mut self,
        text: String,
        attachments: Vec<PathBuf>,
        voice_wav: Option<Arc<Vec<u8>>>,
        cx: &mut Context<Self>,
    ) {
        let Some(chat_id) = self.active_chat_id() else {
            return;
        };
        let message_id = new_id();
        let answer = self.chat_needs_answer(&chat_id);
        {
            let ui = self.chat_ui.entry(chat_id.clone()).or_default();
            if !ui.composer_dropped {
                ui.composer_dropped = true;
                ui.drop_started_at = Some(now_ms());
            }
        }
        // Title from the first message (§3.4); the source also queues a
        // `retitle` command for the box.
        if let Some(chat) = self
            .chats
            .values_mut()
            .flat_map(|v| v.iter_mut())
            .find(|c| c.id == chat_id && !c.is_pinned && c.title == "New chat")
        {
            chat.title = title_from(&text);
        }
        // Optimistic echo: the row shows at once; the source's frame with the
        // same id replaces it (§3.6).
        let mut parts: Vec<Part> = attachments
            .iter()
            .map(|path| Part::Attachment {
                id: new_id(),
                attachment: crate::model::Attachment {
                    name: path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("attachment")
                        .to_string(),
                    mime: "application/octet-stream".into(),
                    blob_ref: None,
                    bytes: None,
                    kind: None,
                    title: None,
                },
            })
            .collect();
        if voice_wav.is_some() {
            parts.push(Part::Attachment {
                id: new_id(),
                attachment: crate::model::Attachment {
                    name: "voice-memo.wav".into(),
                    mime: "audio/wav".into(),
                    blob_ref: None,
                    bytes: None,
                    kind: Some(AttachmentKind::Audio),
                    title: None,
                },
            });
        }
        parts.push(Part::Text {
            id: new_id(),
            text: text.clone(),
        });
        self.messages
            .entry(chat_id.clone())
            .or_default()
            .push(Message {
                id: message_id.clone(),
                chat_id: chat_id.clone(),
                role: Role::User,
                sender: "user".into(),
                status: MessageStatus::Queued,
                turn: None,
                decision: None,
                subagent_id: None,
                subagent_name: None,
                in_reply_to: None,
                routine_name: None,
                sender_name: None,
                sender_icon: None,
                sender_handle: None,
                sender_avatar: None,
                mentions: Vec::new(),
                reply_to: None,
                asked_by_handle: None,
                owner_handle: None,
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
            });
        if answer {
            let question_id = self.open_plain_question_id(&chat_id);
            let value = question_id
                .as_ref()
                .map(|_| serde_json::Value::String(text.clone()));
            if let Some(qid) = &question_id {
                self.mark_question_answered(&chat_id, qid, value.clone());
            }
            self.command(ClientCommand::Answer {
                chat_id: chat_id.clone(),
                message_id,
                text,
                question_id,
                value,
            });
        } else if let Some(wav) = voice_wav {
            self.command(ClientCommand::SendVoice {
                chat_id: chat_id.clone(),
                message_id,
                text,
                wav,
            });
        } else {
            self.command(ClientCommand::Send {
                chat_id: chat_id.clone(),
                message_id,
                text,
                attachments,
            });
        }
        cx.emit(AppEvent::TranscriptChanged { chat_id });
        cx.notify();
    }

    // ----- voice (F11) -----------------------------------------------------

    pub fn set_voice_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.voice_mode != on {
            self.voice_mode = on;
            if let Some(chat_id) = self.active_chat_id() {
                cx.emit(AppEvent::TranscriptChanged { chat_id });
            }
            cx.notify();
        }
    }

    /// Speech-to-text of a 16 kHz mono WAV through the relay proxy; the answer
    /// arrives as [`AppEvent::VoiceTranscript`] with the same `request_id`.
    pub fn transcribe(&mut self, request_id: u64, wav: Arc<Vec<u8>>) {
        let project_id = self.selected_project.clone().unwrap_or_default();
        self.command(ClientCommand::Transcribe {
            project_id,
            request_id,
            wav,
        });
    }

    /// Text-to-speech through the relay proxy; answered by [`AppEvent::VoiceSpeech`].
    pub fn speak(&mut self, request_id: u64, text: String) {
        let project_id = self.selected_project.clone().unwrap_or_default();
        self.command(ClientCommand::Speak {
            project_id,
            request_id,
            text,
        });
    }

    /// The reply's speech is cached: show the speaker icon under it.
    pub fn mark_spoken(&mut self, message_id: Id, cx: &mut Context<Self>) {
        if self.spoken.insert(message_id.clone()) {
            if let Some(chat_id) = self.chat_of_message(&message_id) {
                cx.emit(AppEvent::TranscriptChanged { chat_id });
            }
            cx.notify();
        }
    }

    pub fn replay_speech(&mut self, message_id: Id, cx: &mut Context<Self>) {
        cx.emit(AppEvent::ReplaySpeech { message_id });
    }

    fn chat_of_message(&self, message_id: &str) -> Option<Id> {
        self.messages
            .iter()
            .find(|(_, list)| list.iter().any(|m| m.id == message_id))
            .map(|(chat_id, _)| chat_id.clone())
    }

    /// Answer a typed widget card: the rendered text becomes the user's bubble at once
    /// (optimistic), the card flips to answered, and the structured value goes to the box.
    pub fn answer_question(
        &mut self,
        chat_id: Id,
        question_id: String,
        value: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let widget = self.messages_of(&chat_id).iter().find_map(|m| {
            m.parts.iter().find_map(|p| match p {
                Part::Question {
                    question_id: qid,
                    widget,
                    ..
                } if *qid == question_id => Some(widget.clone()),
                _ => None,
            })
        });
        let text = match widget.flatten() {
            Some(w) => w.answer_text(&value),
            None => format!(
                "Answer: {}",
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string())
            ),
        };
        let message_id = new_id();
        {
            let ui = self.chat_ui.entry(chat_id.clone()).or_default();
            if !ui.composer_dropped {
                ui.composer_dropped = true;
                ui.drop_started_at = Some(now_ms());
            }
        }
        self.mark_question_answered(&chat_id, &question_id, Some(value.clone()));
        self.messages
            .entry(chat_id.clone())
            .or_default()
            .push(Message {
                id: message_id.clone(),
                chat_id: chat_id.clone(),
                role: Role::User,
                sender: "user".into(),
                status: MessageStatus::Queued,
                turn: None,
                decision: None,
                subagent_id: None,
                subagent_name: None,
                in_reply_to: None,
                usage: None,
                return_contract: None,
                waiting_user: false,
                waiting_approval: false,
                model: None,
                effort: None,
                routine_name: None,
                sender_name: None,
                sender_icon: None,
                sender_handle: None,
                sender_avatar: None,
                mentions: Vec::new(),
                reply_to: None,
                asked_by_handle: None,
                owner_handle: None,
                created_at: now_ms(),
                completed_at: None,
                pending: true,
                parts: vec![Part::Text {
                    id: new_id(),
                    text: text.clone(),
                }],
                hidden_parts: Vec::new(),
            });
        self.command(ClientCommand::Answer {
            chat_id: chat_id.clone(),
            message_id,
            text,
            question_id: Some(question_id),
            value: Some(value),
        });
        cx.emit(AppEvent::TranscriptChanged { chat_id });
        cx.notify();
    }

    fn mark_question_answered(
        &mut self,
        chat_id: &str,
        question_id: &str,
        value: Option<serde_json::Value>,
    ) {
        let Some(list) = self.messages.get_mut(chat_id) else {
            return;
        };
        for m in list.iter_mut() {
            for p in m.parts.iter_mut() {
                if let Part::Question {
                    question_id: qid,
                    answer,
                    state,
                    answered_at,
                    ..
                } = p
                {
                    if qid == question_id {
                        *answer = value.clone();
                        *state = QuestionState::Answered;
                        *answered_at = Some(now_ms());
                        m.waiting_user = false;
                    }
                }
            }
        }
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(chat_id) = self.active_chat_id() {
            self.command(ClientCommand::Stop { chat_id });
        }
        cx.notify();
    }

    pub fn retry(&mut self, chat_id: Id, message_id: Id, cx: &mut Context<Self>) {
        self.command(ClientCommand::Retry {
            chat_id,
            message_id,
        });
        cx.notify();
    }

    /// S18: approve an approval card; the box dispatches and flips the card's state.
    pub fn approve(&mut self, chat_id: Id, approval_id: String, cx: &mut Context<Self>) {
        self.forward_vault_decision(&chat_id, &approval_id, true, cx);
        self.command(ClientCommand::Approve {
            chat_id,
            approval_id,
        });
        cx.notify();
    }

    /// S18: reject an approval card; nothing is dispatched.
    pub fn reject(
        &mut self,
        chat_id: Id,
        approval_id: String,
        reason: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.forward_vault_decision(&chat_id, &approval_id, false, cx);
        self.command(ClientCommand::Reject {
            chat_id,
            approval_id,
            reason,
        });
        cx.notify();
    }

    /// F8: a vault card carries the relay request id in its payload; the relay row is the
    /// authority (`POST /vault/requests/{id}/decide` needs a device token), so the card's
    /// decision goes there too.
    fn forward_vault_decision(
        &mut self,
        chat_id: &Id,
        approval_id: &str,
        approve: bool,
        cx: &mut Context<Self>,
    ) {
        let request_id = self
            .messages
            .get(chat_id)
            .into_iter()
            .flatten()
            .flat_map(|m| m.parts.iter())
            .find_map(|p| match p {
                Part::Approval {
                    approval_id: id,
                    payload,
                    ..
                } if id == approval_id => payload
                    .get("vaultRequestId")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                _ => None,
            });
        if let Some(request_id) = request_id {
            self.vault_decide(request_id, approve, cx);
        }
    }

    pub fn set_model(&mut self, model: String, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        let effort = self.selected().map(|p| p.effort).unwrap_or(Effort::High);
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.model = model.clone();
        }
        self.command(ClientCommand::SetModel {
            project_id,
            model,
            effort,
        });
        cx.notify();
    }

    pub fn set_effort(&mut self, effort: Effort, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        let model = self.selected().map(|p| p.model.clone()).unwrap_or_default();
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.effort = effort;
        }
        self.command(ClientCommand::SetModel {
            project_id,
            model,
            effort,
        });
        cx.notify();
    }

    pub fn toggle_rail(&mut self, cx: &mut Context<Self>) {
        self.rail_collapsed = !self.rail_collapsed;
        cx.notify();
    }

    pub fn machine_start(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::MachineStart);
        cx.notify();
    }

    pub fn machine_stop(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::MachineStop);
        cx.notify();
    }

    // ----- avatars

    /// Character slug a project shows (own choice or the hash pick).
    pub fn avatar_slug(&self, project: &Project) -> Option<String> {
        crate::ui::avatars::slug_for(project, &self.projects)
    }

    /// Animation state for a project's avatar from what its agent is doing.
    pub fn avatar_state(&self, project_id: &str) -> crate::ui::avatars::AvatarState {
        use crate::ui::avatars::AvatarState;
        if self.project_is_streaming(project_id) {
            return AvatarState::Working;
        }
        match self.avatar_reply_until.get(project_id) {
            Some(until) if *until > now_ms() => AvatarState::Reply,
            _ => AvatarState::Idle,
        }
    }

    pub fn set_avatar(&mut self, project_id: Id, slug: String, cx: &mut Context<Self>) {
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.avatar = Some(slug.clone());
        }
        self.avatar_picker = None;
        self.command(ClientCommand::SetAvatar { project_id, slug });
        cx.notify();
    }

    pub fn toggle_avatar_picker(&mut self, project_id: Id, cx: &mut Context<Self>) {
        self.avatar_picker = match &self.avatar_picker {
            Some(id) if *id == project_id => None,
            _ => Some(project_id),
        };
        cx.notify();
    }

    pub fn machine_restart(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::MachineRestart);
        cx.notify();
    }

    pub fn toggle_show_details(&mut self, cx: &mut Context<Self>) {
        self.show_details = !self.show_details;
        if let Some(chat_id) = self.active_chat_id() {
            cx.emit(AppEvent::TranscriptChanged { chat_id });
        }
        cx.notify();
    }

    pub fn toggle_right_panel(&mut self, cx: &mut Context<Self>) {
        self.right_panel_open = !self.right_panel_open;
        let on = self.right_panel_open;
        self.watch_browser(on, cx);
        cx.notify();
    }

    pub fn open_details(&mut self, target: DetailsTarget, cx: &mut Context<Self>) {
        self.details_open = Some(target);
        cx.notify();
    }

    pub fn close_details(&mut self, cx: &mut Context<Self>) {
        self.details_open = None;
        cx.notify();
    }

    // ----- system notifications (D49) --------------------------------------

    fn notify_gate(&self, project_id: &str, chat_id: Option<&str>) -> Gate {
        let project_shown = self.selected_project.as_deref() == Some(project_id);
        let chat_shown = match chat_id {
            Some(id) => self.active_chat_id().as_deref() == Some(id),
            None => true,
        };
        Gate {
            streaming: self.project_is_streaming(project_id),
            visible: self.window_active && project_shown && chat_shown,
        }
    }

    fn notify(&mut self, project_id: Id, chat_id: Option<&str>, kind: NotifyKind, detail: String) {
        let Some(project) = self.project(&project_id) else {
            return;
        };
        let event = NotifyEvent {
            project_id: project.id.clone(),
            project_name: project.name.clone(),
            kind,
            detail,
        };
        let gate = self.notify_gate(&project_id, chat_id);
        self.notifier.offer(event, now_ms(), gate);
    }

    fn notify_for_message(
        &mut self,
        chat_id: &str,
        message: &Message,
        was_streaming: bool,
        had_open_question: bool,
        is_new: bool,
    ) {
        let Some(chat) = self
            .chats
            .values()
            .flat_map(|v| v.iter())
            .find(|c| c.id == chat_id)
        else {
            return;
        };
        let project_id = chat.project_id.clone();
        let title = chat.title.clone();
        let is_system = chat.is_system;

        let sender = message.sender.as_str();
        let in_people = self
            .projects
            .iter()
            .any(|p| p.id == project_id && p.is_people());
        if message.role == Role::User {
            // Someone else wrote in a people chat (D72 + D49): "@sam: …".
            if is_new && in_people && !self.is_mine(message) {
                let handle = message
                    .sender_handle
                    .clone()
                    .unwrap_or_else(|| "someone".into());
                self.notify(
                    project_id,
                    Some(chat_id),
                    NotifyKind::DirectMessage,
                    format!("@{handle}: {}", preview(message)),
                );
                return;
            }
            // A prompt another project injected is news; our own typed
            // messages and routine prompts are not (the routine's reply is).
            if is_new && (sender.starts_with("project:") || (is_system && sender != "user")) {
                self.notify(
                    project_id,
                    Some(chat_id),
                    NotifyKind::CrossProject,
                    preview(message),
                );
            }
            return;
        }
        if needs_input(message) && !had_open_question {
            let question = message
                .open_questions()
                .next_back()
                .and_then(|p| match p {
                    Part::Question { question, .. } => Some(question.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| preview(message));
            self.notify(project_id, Some(chat_id), NotifyKind::NeedsInput, question);
            return;
        }
        let finished = !message.is_streaming() && message.status != MessageStatus::Queued;
        if !finished || !(was_streaming || is_new) {
            return;
        }
        if message.role == Role::Assistant && was_streaming {
            // The avatar's short reply reaction, once per completed turn.
            let slug = self
                .projects
                .iter()
                .find(|p| p.id == project_id)
                .and_then(|p| self.avatar_slug(p));
            let len = slug
                .as_deref()
                .map(crate::ui::avatars::reply_duration_ms)
                .unwrap_or(1000);
            self.avatar_reply_until
                .insert(project_id.clone(), now_ms() + len);
        }
        if message.role == Role::Project || sender.starts_with("project:") {
            self.notify(
                project_id,
                Some(chat_id),
                NotifyKind::CrossProject,
                preview(message),
            );
            return;
        }
        if message.role != Role::Assistant {
            return;
        }
        let failed = message.status == MessageStatus::Error;
        if let Some(routine) = self.routine_behind(chat_id, message) {
            let detail = if failed {
                format!("{routine}: run failed")
            } else {
                let p = preview(message);
                if p.is_empty() {
                    routine
                } else {
                    format!("{routine}: {p}")
                }
            };
            self.notify(project_id, Some(chat_id), NotifyKind::RoutineRan, detail);
            return;
        }
        let detail = if failed {
            format!("{title}: turn failed")
        } else {
            let p = preview(message);
            if p.is_empty() {
                title
            } else {
                p
            }
        };
        self.notify(project_id, Some(chat_id), NotifyKind::TurnFinished, detail);
    }

    /// The routine whose prompt this assistant reply answers (F3: the prompt is
    /// a user-role message with `sender: routine:<id>` and `routineName`).
    fn routine_behind(&self, chat_id: &str, reply: &Message) -> Option<String> {
        let list = self.messages_of(chat_id);
        let prompt = match reply.in_reply_to.as_deref() {
            Some(id) => list.iter().find(|m| m.id == id),
            None => {
                let at = list
                    .iter()
                    .position(|m| m.id == reply.id)
                    .unwrap_or(list.len());
                list[..at].iter().rev().find(|m| m.role == Role::User)
            }
        }?;
        let id = prompt.sender.strip_prefix("routine:")?;
        Some(
            prompt
                .routine_name
                .clone()
                .unwrap_or_else(|| format!("Routine {id}")),
        )
    }

    /// F2 approvals call this when a gate card appears; no caller on this
    /// branch yet.
    #[allow(dead_code)]
    pub fn notify_approval_requested(
        &mut self,
        project_id: Id,
        chat_id: Option<&str>,
        what: String,
    ) {
        self.notify(project_id, chat_id, NotifyKind::ApprovalRequested, what);
    }

    fn notify_tick(&mut self) {
        let gates: HashMap<Id, Gate> = self
            .projects
            .iter()
            .map(|p| (p.id.clone(), self.notify_gate(&p.id, None)))
            .collect();
        self.notifier
            .tick(now_ms(), |id| gates.get(id).copied().unwrap_or_default());
    }

    pub fn set_window_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.window_active != active {
            self.window_active = active;
            cx.notify();
        }
    }

    pub fn set_notifications_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.notifier.enabled = enabled;
        cx.notify();
    }

    pub fn push_notice(&mut self, text: impl Into<String>) {
        let now = now_ms();
        self.notices
            .retain(|n| now.saturating_sub(n.at) < NOTICE_TTL_MS);
        self.notices.push(Notice {
            text: text.into(),
            at: now,
        });
    }

    pub fn live_notices(&self) -> Vec<Notice> {
        let now = now_ms();
        self.notices
            .iter()
            .filter(|n| now.saturating_sub(n.at) < NOTICE_TTL_MS)
            .cloned()
            .collect()
    }

    pub fn pair(&mut self, code: String, cx: &mut Context<Self>) {
        if let Some(p) = &mut self.pairing {
            p.busy = true;
            p.error = None;
        }
        self.command(ClientCommand::Pair { code });
        cx.notify();
    }

    /// "Sign in with GitHub": run the browser leg, then exchange the grant.
    pub fn sign_in(&mut self, cx: &mut Context<Self>) {
        let Some(p) = &mut self.pairing else {
            return;
        };
        if p.busy {
            return;
        }
        p.busy = true;
        p.error = None;
        let relay = p.relay_url.clone();
        cx.notify();
        let this = cx.entity().downgrade();
        auth::begin(relay, device_name(), cx, move |outcome, cx| {
            let _ = this.update(cx, |state, cx| {
                match outcome {
                    Ok(auth::SignIn::Grant(grant_code)) => {
                        state.command(ClientCommand::SignIn { grant_code });
                    }
                    Ok(auth::SignIn::PairCodeMode) => {
                        if let Some(p) = &mut state.pairing {
                            p.mode = SignInMode::PairCode;
                            p.busy = false;
                        }
                    }
                    Err(err) => {
                        if let Some(p) = &mut state.pairing {
                            p.busy = false;
                            p.error = Some(format!("{err:#}"));
                        }
                    }
                }
                cx.notify();
            });
        });
    }

    // ----- people chats (D72)

    /// `gh_<id>` of the signed-in user; the fixture signs in as `gh_1`.
    pub fn me_id(&self) -> String {
        self.account
            .as_ref()
            .map(|a| a.id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| {
                if self.source_name == "fixture" {
                    crate::sync::fixture::ME_ID.into()
                } else {
                    String::new()
                }
            })
    }

    pub fn me_handle(&self) -> String {
        self.account
            .as_ref()
            .map(|a| a.login.clone())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| {
                if self.source_name == "fixture" {
                    crate::sync::fixture::ME_HANDLE.into()
                } else {
                    String::new()
                }
            })
    }

    /// Projects of mine (not people chats, not shared with me) — the add-agent picker.
    pub fn my_projects(&self) -> impl Iterator<Item = &Project> {
        self.projects.iter().filter(|p| !p.is_people() && !p.shared)
    }

    pub fn people_spaces(&self) -> impl Iterator<Item = &Project> {
        self.projects.iter().filter(|p| p.is_people())
    }

    /// Is this message mine? (`sender: user:<my id>` or the plain `user` of a project chat.)
    pub fn is_mine(&self, message: &Message) -> bool {
        let sender = message.sender.as_str();
        sender == "user" || sender == format!("user:{}", self.me_id())
    }

    pub fn open_new_chat(&mut self, cx: &mut Context<Self>) {
        self.new_chat_open = true;
        self.new_project_open = false;
        self.invite_target = None;
        self.people_query.clear();
        self.people_results.clear();
        self.people_error = None;
        cx.notify();
    }

    /// Project header "Invite": the same sheet, but a pick adds a member.
    pub fn open_invite(&mut self, project_id: Id, cx: &mut Context<Self>) {
        self.open_new_chat(cx);
        self.invite_target = Some(project_id);
        cx.notify();
    }

    pub fn close_new_chat(&mut self, cx: &mut Context<Self>) {
        self.new_chat_open = false;
        self.awaiting_dm = None;
        self.invite_target = None;
        cx.notify();
    }

    /// The sheet's pick: invite into the target project or start a DM.
    pub fn pick_person(&mut self, handle: String, cx: &mut Context<Self>) {
        match self.invite_target.clone() {
            Some(project_id) => {
                self.new_chat_open = false;
                self.invite_target = None;
                self.invite_to_project(project_id, handle, cx);
            }
            None => self.start_dm(handle, cx),
        }
    }

    pub fn search_people(&mut self, query: String, cx: &mut Context<Self>) {
        self.people_query = query.clone();
        self.people_error = None;
        let q = query.trim().trim_start_matches('@');
        if q.is_empty() {
            self.people_results.clear();
            self.people_busy = false;
        } else {
            self.people_busy = true;
            self.command(ClientCommand::SearchPeople { query });
        }
        cx.notify();
    }

    pub fn start_dm(&mut self, handle: String, cx: &mut Context<Self>) {
        let handle = handle.trim().trim_start_matches('@').to_string();
        if handle.is_empty() {
            return;
        }
        let existing = self
            .people_spaces()
            .find(|p| p.person().map(|x| x.handle == handle).unwrap_or(false))
            .map(|p| p.id.clone());
        if let Some(existing) = existing {
            self.new_chat_open = false;
            self.select_project(existing, cx);
            return;
        }
        self.people_busy = true;
        self.awaiting_dm = Some(handle.clone());
        self.command(ClientCommand::StartDm { handle });
        cx.notify();
    }

    /// Composer keystroke in a people chat: at most one presence ping per 3 s.
    pub fn typed_in(&mut self, chat_id: &Id) {
        let Some(space) = self.project_of_chat(chat_id) else {
            return;
        };
        if !space.is_people() {
            return;
        }
        let now = now_ms();
        let last = self.typing_sent_at.get(chat_id).copied().unwrap_or(0);
        if now.saturating_sub(last) > 3_000 {
            self.typing_sent_at.insert(chat_id.clone(), now);
            self.command(ClientCommand::SetTyping {
                chat_id: chat_id.clone(),
                active: true,
            });
        }
    }

    pub fn typing_in(&self, chat_id: &str) -> Option<String> {
        self.typing
            .get(chat_id)
            .filter(|(_, until)| *until > now_ms())
            .map(|(handle, _)| handle.clone())
    }

    fn project_of_chat(&self, chat_id: &str) -> Option<&Project> {
        let pid = self
            .chats
            .values()
            .flat_map(|v| v.iter())
            .find(|c| c.id == chat_id)
            .map(|c| c.project_id.clone())?;
        self.projects.iter().find(|p| p.id == pid)
    }

    pub fn add_agent(&mut self, chat_id: Id, project_id: Id, cx: &mut Context<Self>) {
        self.command(ClientCommand::AddAgent {
            chat_id,
            project_id,
        });
        cx.notify();
    }

    pub fn remove_agent(&mut self, chat_id: Id, project_id: Id, cx: &mut Context<Self>) {
        self.command(ClientCommand::RemoveAgent {
            chat_id,
            project_id,
        });
        cx.notify();
    }

    pub fn remove_member(&mut self, chat_id: Id, user_id: String, cx: &mut Context<Self>) {
        self.command(ClientCommand::RemoveMember { chat_id, user_id });
        cx.notify();
    }

    pub fn leave_chat(&mut self, chat_id: Id, cx: &mut Context<Self>) {
        if self.selected_project.as_deref() == Some(chat_id.as_str()) {
            self.selected_project = None;
        }
        self.command(ClientCommand::LeaveChat { chat_id });
        cx.notify();
    }

    pub fn block_user(&mut self, handle: String, cx: &mut Context<Self>) {
        self.command(ClientCommand::BlockUser { handle });
        cx.notify();
    }

    pub fn invite_to_project(&mut self, project_id: Id, handle: String, cx: &mut Context<Self>) {
        let handle = handle.trim().trim_start_matches('@').to_string();
        if handle.is_empty() {
            return;
        }
        self.command(ClientCommand::InviteToProject { project_id, handle });
        cx.notify();
    }

    pub fn remove_project_member(
        &mut self,
        project_id: Id,
        user_id: String,
        cx: &mut Context<Self>,
    ) {
        self.command(ClientCommand::RemoveProjectMember {
            project_id,
            user_id,
        });
        cx.notify();
    }

    /// Decoded avatar for a URL, requesting it once when unknown.
    pub fn avatar(&self, url: &str) -> Option<Arc<RenderImage>> {
        self.avatars.get(url).cloned().flatten()
    }

    fn want_avatar(&mut self, url: String) {
        if url.is_empty() || self.avatars.contains_key(&url) {
            return;
        }
        self.avatars.insert(url.clone(), None);
        self.command(ClientCommand::FetchAvatar { url });
    }

    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::SignOut);
        self.account = None;
        self.projects.clear();
        self.chats.clear();
        self.selected_project = None;
        cx.notify();
    }

    pub fn create_project(&mut self, name: String, cx: &mut Context<Self>) {
        self.new_project_open = false;
        self.awaiting_new_project = true;
        self.command(ClientCommand::CreateProject { name });
        self.push_notice("Creating project…");
        cx.notify();
    }

    pub fn fetch_blob(&mut self, key: String, save: bool, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        self.command(ClientCommand::FetchBlob {
            project_id,
            key,
            save,
        });
        cx.notify();
    }

    /// Ask the source for the bytes of every image attachment in a chat that has no thumbnail yet.
    fn request_thumbnails(&mut self, chat_id: &Id) {
        let messages = self.messages.get(chat_id).cloned().unwrap_or_default();
        self.request_thumbnails_for(chat_id, &messages);
    }

    fn request_thumbnails_for(&mut self, chat_id: &Id, messages: &[Message]) {
        let project_id = self
            .chats
            .iter()
            .find(|(_, tabs)| tabs.iter().any(|c| &c.id == chat_id))
            .map(|(pid, _)| pid.clone())
            .or_else(|| self.selected_project.clone());
        let Some(project_id) = project_id else {
            return;
        };
        for message in messages {
            for part in &message.parts {
                let Part::Attachment { attachment, .. } = part else {
                    continue;
                };
                if attachment.kind() != AttachmentKind::Image {
                    continue;
                }
                let Some(key) = attachment.blob_ref.clone() else {
                    continue;
                };
                if !self.thumbnails_requested.insert(key.clone()) {
                    continue;
                }
                self.command(ClientCommand::FetchThumbnail {
                    project_id: project_id.clone(),
                    key,
                });
            }
        }
    }

    /// Download an attachment to `~/Downloads/<name>`; the source answers with `AttachmentSaved`.
    pub fn download_attachment(&mut self, key: String, name: String, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        self.push_notice(format!("Downloading {name}…"));
        self.command(ClientCommand::DownloadAttachment {
            project_id,
            key,
            name,
        });
        cx.notify();
    }

    pub fn watch_browser(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        if !on {
            self.browser_frame = None;
        }
        self.command(ClientCommand::WatchBrowser { project_id, on });
        cx.notify();
    }

    // ----- connectors (F1) -------------------------------------------------

    pub fn refresh_connectors(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::RefreshConnectors);
        cx.notify();
    }

    pub fn connect_connector(
        &mut self,
        provider: String,
        token: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.push_notice(if token.is_some() {
            format!("Saving the {provider} token on the relay…")
        } else {
            format!("Starting {provider} sign-in…")
        });
        self.command(ClientCommand::ConnectorConnect { provider, token });
        cx.notify();
    }

    pub fn disconnect_connector(&mut self, id: Id, cx: &mut Context<Self>) {
        self.connectors.items.retain(|c| c.id != id);
        self.command(ClientCommand::ConnectorDisconnect { id });
        cx.notify();
    }

    // ----- this mac (F9) ---------------------------------------------------

    pub fn local_refresh(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::LocalRefresh);
        cx.notify();
    }

    pub fn local_set_allow_reads(&mut self, device_id: Id, allow: bool, cx: &mut Context<Self>) {
        if let Some(d) = self
            .local
            .devices
            .iter_mut()
            .find(|d| d.device_id == device_id)
        {
            d.allow_reads = allow;
        }
        self.command(ClientCommand::LocalSetAllowReads { device_id, allow });
        cx.notify();
    }

    pub fn local_forget(&mut self, device_id: Id, cx: &mut Context<Self>) {
        self.local.devices.retain(|d| d.device_id != device_id);
        self.command(ClientCommand::LocalForget { device_id });
        cx.notify();
    }

    // ----- vault (F8) ------------------------------------------------------

    pub fn vault_refresh(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::VaultRefresh);
        cx.notify();
    }

    pub fn vault_add_item(&mut self, item: VaultNewItem, cx: &mut Context<Self>) {
        self.push_notice(format!("Saving {} on the relay…", item.name));
        self.command(ClientCommand::VaultAddItem { item });
        cx.notify();
    }

    pub fn vault_delete_item(&mut self, id: Id, cx: &mut Context<Self>) {
        self.vault.items.retain(|i| i.id != id);
        self.command(ClientCommand::VaultDeleteItem { id });
        cx.notify();
    }

    pub fn vault_decide(&mut self, request_id: Id, approve: bool, cx: &mut Context<Self>) {
        if let Some(r) = self.vault.requests.iter_mut().find(|r| r.id == request_id) {
            r.status = if approve {
                VaultRequestStatus::Approved
            } else {
                VaultRequestStatus::Denied
            };
            r.decided_at = Some(now_ms() as i64);
        }
        self.command(ClientCommand::VaultDecide {
            request_id,
            approve,
        });
        cx.notify();
    }

    // ----- routines (F3) ---------------------------------------------------

    pub fn routines_of(&self, project_id: &str) -> &[Routine] {
        self.routines
            .get(project_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn routine_runs_of(&self, routine_id: &str) -> &[RoutineRun] {
        self.routine_runs
            .get(routine_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn routines_refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(project_id) = self.selected_project.clone() {
            self.command(ClientCommand::RoutinesRefresh { project_id });
        }
        cx.notify();
    }

    pub fn routine_create(
        &mut self,
        name: String,
        prompt: String,
        chat_id: Id,
        trigger: RoutineTrigger,
        cx: &mut Context<Self>,
    ) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        self.command(ClientCommand::RoutineCreate {
            project_id,
            name,
            prompt,
            chat_id,
            trigger,
        });
        cx.notify();
    }

    pub fn routine_set_enabled(&mut self, routine_id: Id, enabled: bool, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        if let Some(r) = self
            .routines
            .get_mut(&project_id)
            .and_then(|v| v.iter_mut().find(|r| r.id == routine_id))
        {
            r.enabled = enabled;
        }
        self.command(ClientCommand::RoutineSetEnabled {
            project_id,
            routine_id,
            enabled,
        });
        cx.notify();
    }

    pub fn routine_run_now(&mut self, routine_id: Id, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        self.command(ClientCommand::RoutineRunNow {
            project_id,
            routine_id,
        });
        self.push_notice("Routine queued");
        cx.notify();
    }

    pub fn routine_load_runs(&mut self, routine_id: Id, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        self.command(ClientCommand::RoutineRuns {
            project_id,
            routine_id,
        });
        cx.notify();
    }

    pub fn routine_delete(&mut self, routine_id: Id, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        if let Some(v) = self.routines.get_mut(&project_id) {
            v.retain(|r| r.id != routine_id);
        }
        self.command(ClientCommand::RoutineDelete {
            project_id,
            routine_id,
        });
        cx.notify();
    }

    /// History "open": switch to the chat that holds the reply.
    pub fn open_chat_of_routine(&mut self, routine_id: &str, cx: &mut Context<Self>) {
        let chat_id = self
            .selected_project
            .as_deref()
            .and_then(|p| self.routines_of(p).iter().find(|r| r.id == routine_id))
            .map(|r| r.chat_id.clone());
        if let Some(chat_id) = chat_id {
            self.select_tab(chat_id, cx);
        }
    }

    // ----- skills (F5) -------------------------------------------------------

    pub fn refresh_skills(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::SkillsRefresh);
        cx.notify();
    }

    pub fn save_skill(&mut self, skill: Skill, cx: &mut Context<Self>) {
        // Optimistic: the list shows the edit at once; the source's `Skills`
        // event replaces it with the stored version.
        match self
            .skills
            .iter_mut()
            .find(|s| s.id == skill.id && !skill.id.is_empty())
        {
            Some(existing) => *existing = skill.clone(),
            None => self.skills.push(skill.clone()),
        }
        self.command(ClientCommand::SkillSave { skill });
        cx.notify();
    }

    pub fn delete_skill(&mut self, id: Id, cx: &mut Context<Self>) {
        self.skills.retain(|s| s.id != id);
        self.command(ClientCommand::SkillDelete { id });
        cx.notify();
    }

    // ----- channels (F7) ---------------------------------------------------

    pub fn refresh_channels(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::RefreshChannels);
        cx.notify();
    }

    /// Mint a pairing code for the selected project (its pinned chat).
    pub fn new_channel_pairing(&mut self, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            self.push_notice("Select a project first.");
            cx.notify();
            return;
        };
        self.command(ClientCommand::ChannelPairNew {
            project_id,
            chat_id: None,
        });
        cx.notify();
    }

    pub fn unbind_channel(&mut self, id: Id, cx: &mut Context<Self>) {
        self.channels.bindings.retain(|b| b.id != id);
        self.command(ClientCommand::ChannelUnbind { id });
        cx.notify();
    }

    pub fn selected_doc(&self) -> Option<&ProjectDocView> {
        self.selected_project
            .as_deref()
            .and_then(|id| self.project_docs.get(id))
    }

    /// The live browser card of the selected project, if any (S16).
    pub fn live_browser(&self) -> Option<(String, String, String)> {
        let project_id = self.selected_project.as_deref()?;
        for chat in self.tabs(project_id) {
            for m in self.messages_of(&chat.id).iter().rev() {
                for p in &m.parts {
                    if let Part::Browser {
                        title,
                        url,
                        last_action,
                        live: true,
                        ..
                    } = p
                    {
                        return Some((title.clone(), url.clone(), last_action.clone()));
                    }
                }
            }
        }
        None
    }

    pub fn box_state(&self) -> Option<BoxState> {
        self.selected().map(|p| p.state)
    }
}

pub fn title_from(text: &str) -> String {
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

/// PNG/JPEG → BGRA frame for GPUI (`RenderImage` is BGRA).
pub fn decode_image(bytes: &[u8]) -> Option<RenderImage> {
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    let mut raw = img.into_raw();
    for px in raw.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(w, h, raw)?;
    Some(RenderImage::new(vec![image::Frame::new(buffer)]))
}

fn same_shape(a: &Message, b: &Message) -> bool {
    a.parts
        .iter()
        .zip(b.parts.iter())
        .all(|(x, y)| match (x, y) {
            (Part::Text { id: xa, .. }, Part::Text { id: ya, .. }) => xa == ya,
            (Part::Reasoning { id: xa, .. }, Part::Reasoning { id: ya, .. }) => xa == ya,
            (Part::Text { .. }, _) | (Part::Reasoning { .. }, _) => false,
            (other_a, other_b) => other_a == other_b,
        })
}

/// The message is waiting on the user: an open `question` part or the legacy
/// `waiting_user` status.
fn needs_input(m: &Message) -> bool {
    m.open_questions().next().is_some()
        || m.parts.iter().any(|p| {
            matches!(
                p,
                Part::Status {
                    code: StatusKind::AskUser,
                    ..
                }
            )
        })
}

/// First non-empty text line, clipped for a notification body.
fn preview(m: &Message) -> String {
    const MAX: usize = 96;
    let line = m
        .parts
        .iter()
        .find_map(|p| match p {
            Part::Text { text, .. } => text.lines().map(str::trim).find(|l| !l.is_empty()),
            _ => None,
        })
        .unwrap_or("");
    if line.chars().count() <= MAX {
        line.to_string()
    } else {
        let cut: String = line.chars().take(MAX - 1).collect();
        format!("{cut}…")
    }
}

pub fn init_source(cx: &mut App) -> Box<dyn SyncSource> {
    let _ = cx;
    crate::sync::source_from_env()
}

/// What the relay lists this device as (`device_name` on sign-in).
fn device_name() -> String {
    let host = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "Mac".into());
    let host = host.trim_end_matches(".local");
    if cfg!(target_os = "macos") {
        format!("{host} (Mac)")
    } else {
        host.to_string()
    }
}
