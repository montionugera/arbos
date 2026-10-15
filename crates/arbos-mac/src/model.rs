//! Plain Rust view of the Project doc and the Chat docs (`arbos-design.md` §5,
//! `crdt-sync.md` §6). No GPUI, no Loro: the sync layer fills these in, the
//! views only read them.

use serde::{Deserialize, Serialize};

pub use arbos_core::widget::{ChoiceOption, FieldKind, FormField, QuestionState, Widget};

pub type Id = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoxState {
    Awake,
    Waking,
    HibernatedLight,
    HibernatedDeep,
    StoppedByUser,
    Error,
}

impl BoxState {
    pub fn is_asleep(self) -> bool {
        match self {
            BoxState::HibernatedLight | BoxState::HibernatedDeep | BoxState::StoppedByUser => true,
            BoxState::Awake | BoxState::Waking | BoxState::Error => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: Id,
    pub name: String,
    /// One glyph, drawn in the rail tile and the pinned tab.
    pub icon: String,
    /// Hex `#rrggbb` accent for the selected tile border.
    pub color: String,
    pub state: BoxState,
    /// Human text for the S12/S13 banner, e.g. "Restoring from archive…".
    pub state_detail: Option<String>,
    /// 0.0–1.0 while `state == Waking`.
    pub wake_progress: Option<f32>,
    pub model: String,
    pub effort: Effort,
    pub spend_today_usd: f64,
    pub spend_cap_usd: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    pub const ALL: [Effort; 3] = [Effort::Low, Effort::Medium, Effort::High];

    pub fn label(self) -> &'static str {
        match self {
            Effort::Low => "Low",
            Effort::Medium => "Medium",
            Effort::High => "High",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chat {
    pub id: Id,
    pub project_id: Id,
    pub title: String,
    pub is_pinned: bool,
    /// S15 system tab "Projects ⇄".
    pub is_system: bool,
    pub unread: bool,
    pub last_message_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
    Project,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Queued,
    Streaming,
    Done,
    Error,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnKind {
    Reply,
    Act,
    /// Hand-off ack: a new subagent was started.
    New,
    /// Progress message of a subagent (status events only).
    Spawn,
    Wake,
    /// Prose written by the main agent about a finished subagent.
    SubagentResult,
    #[serde(other)]
    Other,
}

/// `turn` is written by the box as `{ "kind": "reply" }` and by older
/// fixtures as a bare string; accept both.
fn de_turn<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<TurnKind>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(match v {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => {
            serde_json::from_value(serde_json::Value::String(s)).ok()
        }
        Some(serde_json::Value::Object(map)) => map
            .get("kind")
            .cloned()
            .and_then(|k| serde_json::from_value(k).ok()),
        Some(_) => None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
}

/// `call` object of a tool part, as `arbos_core::doc::ChatDoc::add_tool_part` writes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: Id,
    pub name: String,
    #[serde(default)]
    pub input: serde_json::Value,
}

impl ToolCall {
    /// `input_line` cut to ~60 chars for dense lists.
    pub fn call_input_short(&self) -> String {
        let line = self.input_line();
        let mut out: String = line.chars().take(60).collect();
        if line.chars().count() > 60 {
            out.push('…');
        }
        out
    }

    /// One-line input for the card.
    pub fn input_line(&self) -> String {
        match &self.input {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => String::new(),
            serde_json::Value::Object(map) => map
                .iter()
                .map(|(k, v)| match v {
                    serde_json::Value::String(s) => format!("{k}={s}"),
                    other => format!("{k}={other}"),
                })
                .collect::<Vec<_>>()
                .join(" "),
            other => other.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    #[serde(rename = "ref", default)]
    pub blob_ref: Option<String>,
}

/// One row of `messages[].parts` (`crdt-sync.md` §6.2). Unknown kinds decode
/// to [`Part::Unknown`] and render as nothing, per the "additive only" rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Part {
    Text {
        id: Id,
        #[serde(default)]
        text: String,
    },
    Reasoning {
        id: Id,
        #[serde(default)]
        reasoning: String,
    },
    Tool {
        id: Id,
        call: ToolCall,
        /// ≤ 160 chars (`crdt-sync.md` §6.2).
        #[serde(default)]
        output_summary: String,
        #[serde(default)]
        output_ref: Option<String>,
        #[serde(default)]
        is_error: bool,
        /// Client-visible progress (S17). `None` means finished.
        #[serde(default)]
        status: Option<ToolStatus>,
        #[serde(default)]
        duration_ms: Option<u64>,
        /// Set when this call spawned a subagent; such a call never folds (S7).
        #[serde(default)]
        subagent_name: Option<String>,
    },
    Attachment {
        id: Id,
        attachment: Attachment,
    },
    Error {
        id: Id,
        code: String,
        message: String,
    },
    /// S4 "Arbos is deciding…", S6 "Working in sandbox…", S8 question, S12 wake note.
    Status {
        id: Id,
        code: StatusKind,
        #[serde(default)]
        message: String,
    },
    /// Collapsed diff card.
    Diff {
        id: Id,
        path: String,
        added: u32,
        removed: u32,
        blob_ref: Option<String>,
    },
    /// Collapsed file card.
    File {
        id: Id,
        path: String,
        lines: u32,
        blob_ref: Option<String>,
    },
    /// Raw subagent output; always `hidden: true` (Details drawer only).
    SubagentResult {
        id: Id,
        #[serde(default)]
        text: String,
        #[serde(default)]
        subagent_id: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        return_contract: Option<serde_json::Value>,
        #[serde(default)]
        duration_ms: Option<u64>,
    },
    /// S16 browser card.
    Browser {
        id: Id,
        title: String,
        url: String,
        last_action: String,
        live: bool,
    },
    /// `ask_user` with an optional typed widget (design §10). `widget == None`
    /// is a plain question answered from the composer (S8).
    Question {
        id: Id,
        question_id: String,
        #[serde(default)]
        question: String,
        #[serde(default, deserialize_with = "de_widget")]
        widget: Option<Widget>,
        #[serde(default)]
        answer: Option<serde_json::Value>,
        #[serde(default = "open_state")]
        state: QuestionState,
        #[serde(default)]
        answered_at: Option<u64>,
    },
    /// F5: the "Relevant skills" section agentd injected into this turn; always `hidden: true`.
    Skills {
        id: Id,
        #[serde(default)]
        text: String,
        #[serde(default)]
        slugs: Vec<String>,
    },
    /// S18 draft-and-approve card (feature `approve`; shape owned by `arbos_core::approval`).
    Approval {
        id: Id,
        approval_id: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        summary: String,
        #[serde(default)]
        risk: ApprovalRisk,
        #[serde(default)]
        payload: serde_json::Value,
        #[serde(default)]
        state: ApprovalState,
        #[serde(default)]
        expires_at: Option<u64>,
        #[serde(default)]
        decided_at: Option<u64>,
        #[serde(default)]
        decided_by: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

fn open_state() -> QuestionState {
    QuestionState::Open
}

/// A widget this client cannot draw (newer type, malformed) degrades to a
/// plain question rather than failing the whole message.
fn de_widget<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Widget>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value::<Widget>(v).ok()))
}

/// One skill of the library (F5); the relay's `Skill` shape, reused as the client view.
pub type Skill = arbos_core::skill::Skill;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRisk {
    Send,
    Destructive,
    Money,
    Credential,
    Computer,
    #[default]
    #[serde(other)]
    Other,
}

impl ApprovalRisk {
    pub fn label(self) -> &'static str {
        match self {
            ApprovalRisk::Send => "send",
            ApprovalRisk::Destructive => "destructive",
            ApprovalRisk::Money => "money",
            ApprovalRisk::Credential => "credential",
            ApprovalRisk::Computer => "computer",
            ApprovalRisk::Other => "other",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    #[default]
    Pending,
    Approved,
    Rejected,
    Expired,
}

impl ApprovalState {
    pub fn label(self) -> &'static str {
        match self {
            ApprovalState::Pending => "Pending",
            ApprovalState::Approved => "Approved",
            ApprovalState::Rejected => "Rejected",
            ApprovalState::Expired => "Expired",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    /// Start of every user turn.
    Deciding,
    /// Act accepted / subagent slot acquired / result turn starting.
    Working,
    /// One tool call; the message is the short label for the working line.
    Step,
    /// Subagent waits for a slot.
    Queued,
    /// Subagent finished (collapses).
    Done,
    /// Subagent failed.
    Failed,
    /// User pressed stop mid-turn.
    Stopped,
    #[serde(alias = "waiting_user")]
    AskUser,
    #[serde(alias = "handed_off")]
    Handoff,
    /// S18: an approval card is pending; the composer shows the yellow waiting state.
    WaitingApproval,
    /// Fixture-only thin note.
    Note,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: Id,
    #[serde(default)]
    pub chat_id: Id,
    pub role: Role,
    #[serde(default)]
    pub sender: String,
    pub status: MessageStatus,
    #[serde(default, deserialize_with = "de_turn")]
    pub turn: Option<TurnKind>,
    /// Routing outcome on the assistant message: reply | act | spawn | wake.
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub subagent_id: Option<String>,
    #[serde(default)]
    pub subagent_name: Option<String>,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    /// Set on user messages a routine injected (`sender: routine:<id>`, F3).
    #[serde(default)]
    pub routine_name: Option<String>,
    /// Token usage as the box reports it (shape varies by harness).
    #[serde(default)]
    pub usage: Option<serde_json::Value>,
    #[serde(default)]
    pub return_contract: Option<serde_json::Value>,
    #[serde(default)]
    pub waiting_user: bool,
    /// At least one approval card of this message is still pending (S18).
    #[serde(default)]
    pub waiting_approval: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub completed_at: Option<u64>,
    /// True until the relay `ACK`s the client's push (S3 faint echo). Client-only.
    #[serde(default)]
    pub pending: bool,
    #[serde(default)]
    pub parts: Vec<Part>,
    /// Parts the box flagged `hidden: true` (tool details): Details drawer only.
    #[serde(default)]
    pub hidden_parts: Vec<Part>,
}

impl Message {
    /// Every tool call of this turn, visible or hidden.
    pub fn tool_parts(&self) -> impl Iterator<Item = &Part> {
        self.parts
            .iter()
            .chain(self.hidden_parts.iter())
            .filter(|p| matches!(p, Part::Tool { .. }))
    }

    pub fn is_streaming(&self) -> bool {
        self.status == MessageStatus::Streaming
    }

    /// Open `question` parts of this message, newest last.
    pub fn open_questions(&self) -> impl DoubleEndedIterator<Item = &Part> {
        self.parts.iter().filter(|p| {
            matches!(
                p,
                Part::Question {
                    state: QuestionState::Open,
                    ..
                }
            )
        })
    }

    /// Approval cards still waiting for a decision (S18).
    pub fn pending_approvals(&self) -> impl Iterator<Item = &Part> {
        self.parts.iter().filter(|p| {
            matches!(
                p,
                Part::Approval {
                    state: ApprovalState::Pending,
                    ..
                }
            )
        })
    }

    /// True when this message carries any `question` part (typed or plain), so the legacy
    /// `waiting_user` status row is redundant.
    pub fn has_question_part(&self) -> bool {
        self.parts
            .iter()
            .any(|p| matches!(p, Part::Question { .. }))
    }

    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for part in &self.parts {
            if let Part::Text { text, .. } = part {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }

    pub fn text_len(&self) -> usize {
        self.parts
            .iter()
            .map(|p| match p {
                Part::Text { text, .. } => text.len(),
                _ => 0,
            })
            .sum()
    }
}

/// One entry of the project doc `threads` map (T1 index, `crdt-sync.md` §6.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThreadInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub harness: String,
    #[serde(default)]
    pub last_turn_at: Option<i64>,
}

/// The parts of the project doc the client renders.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ProjectDocView {
    pub threads: Vec<ThreadInfo>,
    pub status: serde_json::Value,
    pub settings: serde_json::Value,
    pub memory_t1: String,
    /// `settings.modelNote`: set when a model id was replaced by a default.
    pub model_note: String,
}

/// `connectors.status` as the relay reports it (F1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorStatus {
    Connected,
    NeedsAuth,
    Error,
    #[serde(other)]
    Other,
}

impl ConnectorStatus {
    pub fn label(self) -> &'static str {
        match self {
            ConnectorStatus::Connected => "Connected",
            ConnectorStatus::NeedsAuth => "Needs sign-in",
            ConnectorStatus::Error => "Error",
            ConnectorStatus::Other => "Unknown",
        }
    }
}

/// One connected account (`GET /connectors` item). Never carries a token.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Connector {
    pub id: Id,
    pub provider: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub status: ConnectorStatus,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub mock: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConnectorAction {
    pub name: String,
    pub kind: String,
}

/// One catalog entry (`GET /connectors/catalog`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConnectorProvider {
    pub provider: String,
    pub label: String,
    /// `oauth | oauth_or_token | token`.
    #[serde(default)]
    pub connect: String,
    #[serde(default)]
    pub oauth_configured: bool,
    #[serde(default)]
    pub actions: Vec<ConnectorAction>,
}

impl ConnectorProvider {
    pub fn accepts_token(&self) -> bool {
        self.connect == "token" || self.connect == "oauth_or_token"
    }

    pub fn has_oauth(&self) -> bool {
        self.connect == "oauth" || self.connect == "oauth_or_token"
    }
}

/// Settings → Connectors state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConnectorsView {
    pub mock: bool,
    pub providers: Vec<ConnectorProvider>,
    pub items: Vec<Connector>,
    pub loaded: bool,
}

impl ConnectorsView {
    pub fn connector_for(&self, provider: &str) -> Option<&Connector> {
        self.items.iter().find(|c| c.provider == provider)
    }
}

/// One `arbos-local` helper the relay knows (`GET /local/devices` item, F9).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocalDevice {
    pub device_id: Id,
    pub name: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub home: String,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub last_seen: Option<i64>,
    /// `read_file` / `list_dir` skip the approval card (still audited) when on.
    #[serde(default)]
    pub allow_reads: bool,
}

impl LocalDevice {
    pub fn os_label(&self) -> &'static str {
        match self.os.as_str() {
            "macos" => "macOS",
            "linux" => "Linux",
            "windows" => "Windows",
            _ => "Computer",
        }
    }
}

/// Settings → This Mac state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalView {
    pub devices: Vec<LocalDevice>,
    /// Relay URL the install command points the helper at.
    pub relay_url: String,
    pub loaded: bool,
}

/// How a routine fires (relay `routines.trigger`, F3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoutineTrigger {
    Cron {
        expr: String,
        #[serde(default = "utc")]
        tz: String,
    },
    Webhook {
        #[serde(default)]
        secret: String,
    },
    Event {
        event: String,
    },
}

fn utc() -> String {
    "UTC".into()
}

impl RoutineTrigger {
    /// Short human label for the list ("Weekdays 09:00", "Webhook", "On box awake").
    pub fn label(&self) -> String {
        match self {
            RoutineTrigger::Cron { expr, tz } => {
                let base = match expr.trim() {
                    "* * * * *" => "Every minute".to_owned(),
                    "0 * * * *" => "Hourly".to_owned(),
                    "0 9 * * *" => "Daily 09:00".to_owned(),
                    "0 9 * * 1-5" => "Weekdays 09:00".to_owned(),
                    other => format!("cron {other}"),
                };
                if tz == "UTC" {
                    base
                } else {
                    format!("{base} {tz}")
                }
            }
            RoutineTrigger::Webhook { .. } => "Webhook".into(),
            RoutineTrigger::Event { event } => match event.as_str() {
                "box_awake" => "On box awake".into(),
                "cross_project_message" => "On project message".into(),
                "connector_event" => "On connector event".into(),
                other => format!("On {other}"),
            },
        }
    }
}

/// A scheduled or event-triggered prompt (relay `routines` row).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Routine {
    pub id: Id,
    pub project_id: Id,
    pub chat_id: Id,
    pub name: String,
    pub prompt: String,
    pub trigger: RoutineTrigger,
    pub enabled: bool,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub last_run_at: Option<i64>,
    #[serde(default)]
    pub next_run_at: Option<i64>,
    /// Webhook routines only: the URL that fires them (carries the secret).
    #[serde(default)]
    pub fire_url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineRunStatus {
    Queued,
    Running,
    Done,
    Failed,
    Skipped,
    #[serde(other)]
    Other,
}

impl RoutineRunStatus {
    pub fn label(self) -> &'static str {
        match self {
            RoutineRunStatus::Queued => "queued",
            RoutineRunStatus::Running => "running",
            RoutineRunStatus::Done => "done",
            RoutineRunStatus::Failed => "failed",
            RoutineRunStatus::Skipped => "skipped",
            RoutineRunStatus::Other => "—",
        }
    }
}

/// One row of a routine's history (relay `routine_runs`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoutineRun {
    pub id: Id,
    pub routine_id: Id,
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
    pub status: RoutineRunStatus,
    /// The assistant reply; "open" jumps to it.
    #[serde(default)]
    pub message_id: Option<Id>,
    #[serde(default)]
    pub note: Option<String>,
}

/// One platform row in Settings → Channels (`GET /channels` `platforms[]`, F7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelPlatformState {
    Configured,
    Missing,
    Mock,
    #[serde(other)]
    Other,
}

impl ChannelPlatformState {
    pub fn label(self) -> &'static str {
        match self {
            ChannelPlatformState::Configured => "Configured",
            ChannelPlatformState::Missing => "Missing credentials",
            ChannelPlatformState::Mock => "Mock",
            ChannelPlatformState::Other => "Unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelPlatform {
    pub platform: String,
    pub label: String,
    pub state: ChannelPlatformState,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub inbound: String,
}

/// One paired external chat (`GET /channels` `bindings[]`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelBinding {
    pub id: Id,
    pub platform: String,
    #[serde(rename = "externalChatId")]
    pub external_chat_id: String,
    #[serde(rename = "externalUserId")]
    pub external_user_id: String,
    #[serde(rename = "projectId")]
    pub project_id: Id,
    #[serde(rename = "chatId")]
    pub chat_id: Id,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub bound_at: u64,
}

/// The active pairing code (10 min, one use).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelPairing {
    pub code: String,
    #[serde(rename = "projectId")]
    pub project_id: Id,
    #[serde(rename = "chatId")]
    pub chat_id: Id,
    #[serde(rename = "expiresAt")]
    pub expires_at: u64,
}

impl ChannelPairing {
    pub fn seconds_left(&self) -> u64 {
        self.expires_at.saturating_sub(now_ms()) / 1000
    }
}

/// Settings → Channels state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChannelsView {
    pub mock: bool,
    pub platforms: Vec<ChannelPlatform>,
    pub bindings: Vec<ChannelBinding>,
    pub pairing: Option<ChannelPairing>,
    pub loaded: bool,
}

/// The user's one Daytona computer. Projects are workspaces on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MachineState {
    #[default]
    Asleep,
    Waking,
    Awake,
    Stopped,
    Error,
    #[serde(other)]
    Unknown,
}

impl MachineState {
    pub fn label(self) -> &'static str {
        match self {
            MachineState::Asleep => "asleep",
            MachineState::Waking => "waking",
            MachineState::Awake => "awake",
            MachineState::Stopped => "stopped",
            MachineState::Error => "error",
            MachineState::Unknown => "unknown",
        }
    }

    pub fn is_down(self) -> bool {
        matches!(
            self,
            MachineState::Asleep
                | MachineState::Stopped
                | MachineState::Error
                | MachineState::Unknown
        )
    }
}

/// `status.box {state, since}` at user level (mirrored into every project
/// doc) plus the hover details.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Machine {
    pub state: MachineState,
    pub since: Option<u64>,
    /// e.g. "2 vCPU · 4 GiB · 10 GiB" (`SandboxBinding.resource_class`).
    pub spec: Option<String>,
    pub spend_today_usd: f64,
    pub spend_cap_usd: f64,
    pub detail: Option<String>,
    /// 0..1 while waking.
    pub progress: Option<f32>,
}

/// Connection to the relay, S14.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Live,
    Reconnecting,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn new_id() -> Id {
    ulid::Ulid::new().to_string()
}
