//! The data layer. A [`SyncSource`] feeds the UI with [`SyncEvent`]s and takes
//! [`ClientCommand`]s back. Two sources exist: [`fixture::FixtureSource`]
//! (local Loro docs, scripted agent) and [`relay::RelaySource`] (HTTP + one
//! WebSocket per room on `arbos-relay`, `crdt-sync.md` §7–§9).
//!
//! Everything in this module is GPUI-free. Wire frames and the doc schema
//! constants come from `arbos-core`; the client-side mirror (`doc.rs`) and
//! the sources are candidates to move there later (`arbos-design.md` §7.2).

pub mod credentials;
pub mod doc;
pub mod fixture;
pub mod relay;

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::sync::mpsc;

use crate::model::{AccountView, Person};
use crate::model::{
    BoxState, ChannelsView, Chat, ConnectorsView, Effort, Id, Link, LocalView, Machine, Message,
    Project, ProjectDocView, Routine, RoutineRun, RoutineTrigger, Skill, VaultNewItem, VaultView,
};

/// Something the source learned; the UI applies it to its mirror.
#[derive(Clone, Debug)]
pub enum SyncEvent {
    /// No device token for this relay yet: show the pairing screen.
    NeedsPairing {
        relay_url: String,
    },
    /// Who is signed in (`/auth/token` or `/me`); `None` after sign-out.
    Account(Option<AccountView>),
    /// `GET /users/search` results for the new-chat sheet (D72).
    PeopleSearch {
        query: String,
        results: Vec<Person>,
    },
    /// Someone in a people chat started/stopped typing (room presence).
    Typing {
        chat_id: Id,
        handle: String,
        active: bool,
    },
    /// Avatar bytes for an `avatar_url` (PNG/JPEG), fetched once.
    Avatar {
        url: String,
        bytes: Arc<Vec<u8>>,
    },
    /// Error from a people-chat action (search, DM, members), shown inline.
    PeopleError(String),
    /// Pairing finished (or failed with a message).
    Paired {
        device_id: String,
    },
    PairingFailed(String),
    /// Registry: the full project list, rail order.
    Projects(Vec<Project>),
    /// Model ids the relay proxy can serve (picker contents).
    Models(Vec<String>),
    /// The user's machine (`status.box` at user level).
    Machine(Machine),
    /// Workspace lifecycle for one project (`status.workspace` or relay `STATUS`).
    ProjectStatus {
        project_id: Id,
        state: BoxState,
        detail: Option<String>,
        progress: Option<f32>,
    },
    /// Project doc `chats` index for one project, in tab order.
    Chats {
        project_id: Id,
        chats: Vec<Chat>,
    },
    /// The relay created a chat for a locally minted tab; swap the id.
    ChatCreated {
        project_id: Id,
        local_id: Id,
        chat: Chat,
    },
    /// The project doc: threads (T1 index), status, settings, memory.
    ProjectDoc {
        project_id: Id,
        view: ProjectDocView,
    },
    /// Full message list of one chat doc (after a checkpoint or a list-level edit).
    ChatSnapshot {
        chat_id: Id,
        messages: Vec<Message>,
    },
    /// One message changed (streaming text, status, a new part).
    MessageUpsert {
        chat_id: Id,
        message: Box<Message>,
    },
    /// A blob the user asked for (file card, tool output).
    Blob {
        project_id: Id,
        key: String,
        mime: String,
        bytes: Arc<Vec<u8>>,
        saved_to: Option<PathBuf>,
    },
    /// Bytes of an image attachment for its inline thumbnail (F10); keys are content hashes.
    Thumbnail {
        key: String,
        bytes: Arc<Vec<u8>>,
    },
    /// An attachment landed in ~/Downloads (F10); the app reveals it.
    AttachmentSaved {
        path: PathBuf,
    },
    /// One JPEG frame from the box's browser stream (S16).
    BrowserFrame {
        project_id: Id,
        jpeg: Arc<Vec<u8>>,
    },
    /// Relay link state (S14).
    Link(Link),
    /// Short user-facing notice (errors from HTTP calls, etc.).
    Notice(String),
    /// Settings → Connectors: catalog + connected accounts (F1).
    Connectors(ConnectorsView),
    /// An OAuth flow started: open this URL in the browser; the relay finishes the callback.
    ConnectorAuth {
        provider: String,
        auth_url: String,
    },
    /// Settings → Vault: items (never values) and requests (F8).
    Vault(VaultView),
    /// Routines of one project (Settings → Routines).
    Routines {
        project_id: Id,
        routines: Vec<Routine>,
    },
    /// Run history of one routine, newest first.
    RoutineRuns {
        routine_id: Id,
        runs: Vec<RoutineRun>,
    },
    /// The skills library (F5), whole, after a fetch or a write.
    Skills(Vec<Skill>),
    /// Settings → Channels: platform states, bound chats, active pairing code (F7).
    Channels(ChannelsView),
    /// Settings → This Mac: paired `arbos-local` helpers with online state (F9).
    LocalDevices(LocalView),
    /// Speech-to-text result for `ClientCommand::Transcribe` (F11).
    Transcript {
        request_id: u64,
        text: String,
        error: Option<String>,
        mock: bool,
    },
    /// Text-to-speech result for `ClientCommand::Speak` (F11): encoded audio (WAV by default).
    Speech {
        request_id: u64,
        bytes: Arc<Vec<u8>>,
        mime: String,
        error: Option<String>,
        mock: bool,
    },
}

/// Something the user did; the source writes it into the doc / outbox.
#[derive(Clone, Debug)]
pub enum ClientCommand {
    Pair {
        code: String,
    },
    /// Exchange the one-time grant code from the browser leg (`POST /auth/token`).
    SignIn {
        grant_code: String,
    },
    /// `POST /auth/logout`, forget the tokens, back to the sign-in screen.
    SignOut,
    /// New-chat sheet: `GET /users/search?q=`.
    SearchPeople {
        query: String,
    },
    /// `POST /chats/dm {handle}`; the chat lands in the next `Projects` event.
    StartDm {
        handle: String,
    },
    /// Presence `typing` for a people chat.
    SetTyping {
        chat_id: Id,
        active: bool,
    },
    /// `POST /chats/{id}/agents {project_id}` (own project only).
    AddAgent {
        chat_id: Id,
        project_id: Id,
    },
    /// `DELETE /chats/{id}/agents/{project_id}` (owner of that project).
    RemoveAgent {
        chat_id: Id,
        project_id: Id,
    },
    /// `DELETE /chats/{id}/members/{user_id}` (chat owner).
    RemoveMember {
        chat_id: Id,
        user_id: String,
    },
    /// `POST /chats/{id}/leave`.
    LeaveChat {
        chat_id: Id,
    },
    /// `POST /blocks {handle}`.
    BlockUser {
        handle: String,
    },
    /// `POST /projects/{id}/members {handle}` (D72 step 3).
    InviteToProject {
        project_id: Id,
        handle: String,
    },
    /// `DELETE /projects/{id}/members/{user_id}`.
    RemoveProjectMember {
        project_id: Id,
        user_id: String,
    },
    /// Fetch an avatar once (`SyncEvent::Avatar`).
    FetchAvatar {
        url: String,
    },
    OpenChat {
        chat_id: Id,
    },
    Send {
        chat_id: Id,
        message_id: Id,
        text: String,
        attachments: Vec<PathBuf>,
    },
    /// Reply to an `ask_user` question (S8). Same shape as `Send`, different outbox kind.
    /// A typed widget answer carries `question_id` and the structured `value`; `text` is
    /// the rendered form shown in the user's bubble.
    Answer {
        chat_id: Id,
        message_id: Id,
        text: String,
        question_id: Option<String>,
        value: Option<serde_json::Value>,
    },
    Stop {
        chat_id: Id,
    },
    /// S18: decide an approval card (`approve` / `reject` commands, `arbos_core::approval`).
    Approve {
        chat_id: Id,
        approval_id: String,
    },
    Reject {
        chat_id: Id,
        approval_id: String,
        reason: Option<String>,
    },
    Retry {
        chat_id: Id,
        message_id: Id,
    },
    NewChat {
        project_id: Id,
        chat_id: Id,
    },
    /// F6: a group chat hosted by `project_id` with the other `participants`; `chat_id` is the
    /// client's placeholder until `ChatCreated` brings the relay's.
    GroupCreate {
        project_id: Id,
        chat_id: Id,
        participants: Vec<Id>,
        title: Option<String>,
    },
    GroupAddParticipant {
        project_id: Id,
        chat_id: Id,
        participant: Id,
    },
    CloseChat {
        chat_id: Id,
    },
    MarkRead {
        chat_id: Id,
    },
    SetModel {
        project_id: Id,
        model: String,
        effort: Effort,
    },
    CreateProject {
        name: String,
    },
    MachineStart,
    MachineStop,
    /// Always-on machines: stop then start (`POST /machine/stop`, `/machine/start`).
    MachineRestart,
    /// Avatar picker: `PATCH /projects/{id} {avatar: slug}` (mirrored into the doc).
    SetAvatar {
        project_id: Id,
        slug: String,
    },
    FetchBlob {
        project_id: Id,
        key: String,
        save: bool,
    },
    WatchBrowser {
        project_id: Id,
        on: bool,
    },
    /// Fetch an image attachment's bytes for the inline thumbnail (F10).
    FetchThumbnail {
        project_id: Id,
        key: String,
    },
    /// Download an attachment to `~/Downloads/<name>` (F10).
    DownloadAttachment {
        project_id: Id,
        key: String,
        name: String,
    },
    /// Settings → Connectors opened or asked for a refresh.
    RefreshConnectors,
    /// Connect a provider: OAuth when `token` is `None`, else store the pasted token.
    ConnectorConnect {
        provider: String,
        token: Option<String>,
    },
    ConnectorDisconnect {
        id: Id,
    },
    /// Settings → Vault opened or asked for a refresh.
    VaultRefresh,
    /// Add (or replace by name) a vault item; the values go to the relay once.
    VaultAddItem {
        item: VaultNewItem,
    },
    VaultDeleteItem {
        id: Id,
    },
    /// Approve or deny a pending vault request.
    VaultDecide {
        request_id: Id,
        approve: bool,
    },
    /// Settings → Routines opened or needs a refresh.
    RoutinesRefresh {
        project_id: Id,
    },
    RoutineCreate {
        project_id: Id,
        name: String,
        prompt: String,
        chat_id: Id,
        trigger: RoutineTrigger,
    },
    /// Pause (`false`) or resume (`true`).
    RoutineSetEnabled {
        project_id: Id,
        routine_id: Id,
        enabled: bool,
    },
    RoutineRunNow {
        project_id: Id,
        routine_id: Id,
    },
    RoutineRuns {
        project_id: Id,
        routine_id: Id,
    },
    RoutineDelete {
        project_id: Id,
        routine_id: Id,
    },
    /// F5: refetch the skills library.
    SkillsRefresh,
    /// F5: create (`id` empty) or update a skill; the source answers with `SyncEvent::Skills`.
    SkillSave {
        skill: Skill,
    },
    SkillDelete {
        id: Id,
    },
    /// Settings → Channels opened or asked for a refresh.
    RefreshChannels,
    /// Mint a pairing code for a project (its pinned chat unless `chat_id`).
    ChannelPairNew {
        project_id: Id,
        chat_id: Option<Id>,
    },
    ChannelUnbind {
        id: Id,
    },
    /// Settings → This Mac opened or needs a refresh.
    LocalRefresh,
    /// Per-device "Allow reads without asking".
    LocalSetAllowReads {
        device_id: Id,
        allow: bool,
    },
    /// Forget a helper (and revoke its device token).
    LocalForget {
        device_id: Id,
    },
    /// F11: a spoken message. `wav` (16 kHz mono PCM) is uploaded to the blob
    /// store and kept on the row as an `attachment` part `{ kind: "audio" }`.
    SendVoice {
        chat_id: Id,
        message_id: Id,
        text: String,
        wav: Arc<Vec<u8>>,
    },
    /// F11: speech-to-text through the relay proxy; answered by `SyncEvent::Transcript`.
    Transcribe {
        project_id: Id,
        request_id: u64,
        wav: Arc<Vec<u8>>,
    },
    /// F11: text-to-speech through the relay proxy; answered by `SyncEvent::Speech`.
    Speak {
        project_id: Id,
        request_id: u64,
        text: String,
    },
}

pub trait SyncSource: Send + 'static {
    /// Short name shown in the settings popover ("fixture", "relay").
    fn name(&self) -> &'static str;

    /// Run until the command channel closes. The future runs on tokio.
    fn run(
        self: Box<Self>,
        events: mpsc::UnboundedSender<SyncEvent>,
        commands: mpsc::UnboundedReceiver<ClientCommand>,
    ) -> BoxFuture<'static, anyhow::Result<()>>;
}

/// The relay everyone talks to unless told otherwise.
pub const DEFAULT_RELAY_URL: &str = "https://relay.arbos.life";

/// Pick the source. Order: `ARBOS_RELAY_URL` (`fixture` runs the seeded demo
/// source), then `defaults read sh.arbos.mac RelayURL` for self-hosters on
/// macOS, then the compiled-in default relay.
pub fn source_from_env() -> Box<dyn SyncSource> {
    let url = match std::env::var("ARBOS_RELAY_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        Ok(_) | Err(_) => {
            relay_url_from_defaults().unwrap_or_else(|| DEFAULT_RELAY_URL.to_string())
        }
    };
    if url.trim().eq_ignore_ascii_case("fixture") {
        return Box::new(fixture::FixtureSource::new());
    }
    Box::new(relay::RelaySource::from_env(url))
}

/// `defaults write sh.arbos.mac RelayURL https://relay.example` (macOS only).
fn relay_url_from_defaults() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let out = std::process::Command::new("defaults")
        .args(["read", "sh.arbos.mac", "RelayURL"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!url.is_empty()).then_some(url)
}
