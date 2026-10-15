//! The project agent driver: one `pi --mode rpc` child per Project (D65), one pi
//! session per chat, the `route` tool validated here, act limits (D66), streams
//! folded into the chat doc, subagents through the `HarnessAdapter` registry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex, Semaphore};

use arbos_core::approval::ApprovalOutcome;
use arbos_core::doc::{CommandStatus, CommandView, MessageStatus};
use arbos_core::ids::ulid;
use arbos_core::route::{pick_harness, validate, Harness, ValidRoute};
use arbos_core::rpc::{AssistantEvent, Command as RpcCommand, Event};
use arbos_core::widget::Widget;
use arbos_core::{ACT_MAX_WALL_SECS, TOOL_OUTPUT_SUMMARY_MAX};

use crate::config::Config;
use crate::harness::{
    AgentEvent, ClaudeCodeAdapter, HarnessAdapter, PiAdapter, PrimeAdapter, StartRequest,
};
use crate::llm::{extract_return_contract, Llm};
use crate::memory::Memory;
use crate::rpc_child::RpcChild;
use crate::skills::{self, SkillsIndex};
use crate::store::Store;
use crate::sync::{SharedHost, SyncHub};

/// Streams the `reply` string out of the `route` tool's partial JSON arguments.
#[derive(Default)]
pub struct ReplyExtractor {
    buf: String,
    pos: usize,
    in_string: bool,
    done: bool,
    pending_escape: Option<String>,
}

impl ReplyExtractor {
    /// Feed one `toolcall_delta`; returns newly decoded reply text.
    pub fn feed(&mut self, delta: &str) -> String {
        if self.done {
            return String::new();
        }
        self.buf.push_str(delta);
        let mut out = String::new();
        if !self.in_string {
            let Some(k) = self.buf.find("\"reply\"") else {
                return out;
            };
            let after = &self.buf[k + 7..];
            let Some(colon) = after.find(':') else {
                return out;
            };
            let rest = &after[colon + 1..];
            let trimmed = rest.trim_start();
            if trimmed.is_empty() {
                return out;
            }
            if !trimmed.starts_with('"') {
                self.done = true; // null reply
                return out;
            }
            let q = self.buf.len() - trimmed.len();
            self.pos = q + 1;
            self.in_string = true;
        }
        let bytes = self.buf.as_bytes();
        let mut i = self.pos;
        let mut esc = self.pending_escape.take().unwrap_or_default();
        while i < bytes.len() {
            let c = bytes[i] as char;
            if !esc.is_empty() {
                esc.push(c);
                if esc == "\\u" || (esc.starts_with("\\u") && esc.len() < 6) {
                    i += 1;
                    continue;
                }
                match esc.as_str() {
                    "\\n" => out.push('\n'),
                    "\\t" => out.push('\t'),
                    "\\r" => out.push('\r'),
                    "\\\"" => out.push('"'),
                    "\\\\" => out.push('\\'),
                    "\\/" => out.push('/'),
                    s if s.starts_with("\\u") && s.len() == 6 => {
                        if let Some(ch) = u32::from_str_radix(&s[2..], 16)
                            .ok()
                            .and_then(char::from_u32)
                        {
                            out.push(ch);
                        }
                    }
                    other => out.push_str(&other[1..]),
                }
                esc.clear();
                i += 1;
                continue;
            }
            if c == '\\' {
                esc.push('\\');
                i += 1;
                continue;
            }
            if c == '"' {
                self.done = true;
                i += 1;
                break;
            }
            // Multi-byte UTF-8: copy whole chars.
            let ch_len = utf8_len(bytes[i]);
            if i + ch_len > bytes.len() {
                break;
            }
            out.push_str(&self.buf[i..i + ch_len]);
            i += ch_len;
        }
        self.pos = i;
        if !esc.is_empty() {
            self.pending_escape = Some(esc);
        }
        out
    }
}

/// Cheap post-check on main-agent prose as it streams into the doc (demo review v2): drops
/// leading boilerplate lines ("Called 3 tools", "Running the tests…", "Route accepted: …") and
/// whole ```json fences. Text is held back only until a line boundary, so streaming stays live.
#[derive(Default)]
pub struct ProseFilter {
    pending: String,
    in_fence: bool,
    fence_hidden: bool,
    started: bool,
}

impl ProseFilter {
    /// First-line boilerplate: narration about the mechanics, never an answer.
    fn is_boilerplate(line: &str) -> bool {
        let l = line
            .trim()
            .trim_start_matches(['*', '_', '#', '>', '-', ' '])
            .to_ascii_lowercase();
        if l.is_empty() {
            return true;
        }
        let starts = |p: &[&str]| p.iter().any(|x| l.starts_with(x));
        let tool_count = {
            // "called 3 tools", "i ran 2 tool calls.", "(4 tool calls)"
            let t = l
                .trim_start_matches(['(', 'i', ' '])
                .trim_start_matches("i ");
            let t = t
                .trim_start_matches("called ")
                .trim_start_matches("ran ")
                .trim_start_matches("used ")
                .trim_start_matches("made ")
                .trim_start_matches("executed ");
            let mut it = t.splitn(2, ' ');
            matches!((it.next().map(|n| n.parse::<u32>().is_ok()), it.next()), (Some(true), Some(rest)) if rest.starts_with("tool"))
        };
        let mechanics = starts(&[
            "tool call",
            "tool calls",
            "tools called",
            "route accepted",
            "routing:",
            "route:",
        ]);
        // Verb-led narration is short ("Running the tests…"); a real sentence that happens to
        // start with the same word runs long and stays.
        let narration = l.chars().count() <= 90
            && starts(&[
                "running ",
                "executing ",
                "calling ",
                "invoking ",
                "let me check",
                "let me run",
                "let me look",
                "i'll check",
                "i will check",
                "i'll run",
                "i will run",
                "checking ",
                "reading the file",
                "first, i",
                "first i",
                "now i'll",
                "now i will",
            ]);
        tool_count || mechanics || narration
    }

    fn take_line(&mut self, line: &str) -> Option<String> {
        let t = line.trim_start();
        if self.in_fence {
            if t.starts_with("```") {
                self.in_fence = false;
                let hidden = self.fence_hidden;
                self.fence_hidden = false;
                return if hidden { None } else { Some(line.to_owned()) };
            }
            return if self.fence_hidden {
                None
            } else {
                Some(line.to_owned())
            };
        }
        if t.starts_with("```") {
            let lang = t.trim_start_matches('`').trim().to_ascii_lowercase();
            self.in_fence = true;
            self.fence_hidden = lang.starts_with("json");
            return if self.fence_hidden {
                None
            } else {
                Some(line.to_owned())
            };
        }
        if !self.started {
            if Self::is_boilerplate(line) {
                return None;
            }
            self.started = true;
        }
        Some(line.to_owned())
    }

    /// Feed a delta; returns the text that may enter the doc now.
    pub fn feed(&mut self, delta: &str) -> String {
        self.pending.push_str(delta);
        let mut out = String::new();
        while let Some(i) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=i).collect();
            if let Some(l) = self.take_line(&line) {
                out.push_str(&l);
            }
        }
        // A partial line streams at once when it cannot be boilerplate or a fence any more.
        let p = self.pending.trim_start();
        let could_be_fence = p.is_empty() || "```".starts_with(p) || p.starts_with("```");
        if self.started && !self.in_fence && !could_be_fence {
            out.push_str(&std::mem::take(&mut self.pending));
        } else if !self.started && !self.in_fence && self.pending.len() > 160 {
            // Long first line: real prose. Judge it once and let it through.
            if Self::is_boilerplate(&self.pending) {
                self.pending.clear();
            } else {
                self.started = true;
                out.push_str(&std::mem::take(&mut self.pending));
            }
        }
        out
    }

    /// End of a text run (tool call or turn end): judge what is still held back.
    pub fn flush(&mut self) -> String {
        let rest = std::mem::take(&mut self.pending);
        if rest.is_empty() {
            return rest;
        }
        if self.in_fence {
            // An unterminated fence at the end: drop it when hidden, else let it through.
            self.in_fence = false;
            let hidden = self.fence_hidden;
            self.fence_hidden = false;
            return if hidden { String::new() } else { rest };
        }
        self.take_line(&rest).unwrap_or_default()
    }
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

/// Subagents running at once per box; further spawns wait in FIFO order (demo review item 2).
pub const SUBAGENT_MAX_CONCURRENT: usize = 4;

/// Why the main pi session is being prompted.
#[derive(Debug, Clone)]
pub enum TurnKind {
    /// A user message (`send` / `answer`).
    User,
    /// A finished subagent's result, relayed to the user in prose by the main agent.
    SubagentResult { subagent_id: String, name: String },
    /// An approval card was decided after its turn ended (feature `approve`): the main agent
    /// dispatches or acknowledges, in prose.
    ApprovalOutcome { approval_id: String, title: String },
}

/// What one main-agent turn produced, for the caller's follow-ups (memory pass, hand-offs).
pub struct TurnOutcome {
    pub assistant_msg_id: String,
    pub route_name: &'static str,
    pub turn: TurnState,
    pub result: Result<()>,
}

#[derive(Debug, Clone, Default)]
pub struct TurnMetrics {
    pub ttft_ms: Option<i64>,
    pub route_ms: Option<i64>,
    pub tool_calls: i64,
    pub usage: Option<Value>,
    pub route_missing: bool,
    pub route_corrected: bool,
}

pub struct TurnState {
    pub chat_id: String,
    pub user_msg_id: String,
    pub assistant_msg_id: String,
    pub text_part_id: String,
    pub started: Instant,
    pub route: Option<ValidRoute>,
    pub route_attempts: u32,
    pub route_failed: bool,
    pub escalate: bool,
    pub asked: Option<String>,
    pub metrics: TurnMetrics,
    pub streamed_text: String,
    pub act_transcript: Vec<String>,
    /// call id → args from `tool_execution_start`, so the tool part records the real input.
    pub tool_inputs: HashMap<String, Value>,
    pub reply_extractor: ReplyExtractor,
    /// Boilerplate filter on everything the main agent says in this turn.
    pub prose: ProseFilter,
    /// Act turns: prose is held here and only the text after the last tool call reaches the doc;
    /// anything the model said between tool calls is narration and goes to T3 only.
    pub act_buffer: String,
    pub route_call_id: Option<String>,
    pub text_part_dirty: bool,
    /// Set by an out-of-band `stop`: the turn ends as it is and no hand-off follows.
    pub stopped: bool,
    /// The text pi was prompted with; the skills selector matches against it (F5).
    pub prompt_text: String,
    /// Skills injected into this turn's working set (recorded once, in the Details drawer).
    pub skills_used: Vec<String>,
}

pub struct Agent {
    pub cfg: Arc<Config>,
    pub store: Store,
    pub sync: SyncHub,
    pub llm: Arc<Llm>,
    pub memory: Arc<Memory>,
    pi: Mutex<Option<Arc<RpcChild>>>,
    current_chat: Mutex<Option<String>>,
    pub turn: Arc<Mutex<Option<TurnState>>>,
    adapters: HashMap<Harness, Arc<dyn HarnessAdapter>>,
    pub turn_running: AtomicBool,
    /// Subagent turns in flight (keep-alive must cover them too, design §4.6).
    pub subagent_turns: std::sync::atomic::AtomicU32,
    pub pi_ready: AtomicBool,
    pub ext_loaded: AtomicBool,
    /// Set by `shutdown`: the pi supervisor must not respawn a child we are stopping (an orphaned
    /// pi dies with `write EPIPE` at its next reply once agentd is gone).
    pub shutting_down: AtomicBool,
    turn_lock: Mutex<()>,
    /// FIFO gate on concurrent subagent runs in this workspace (`SUBAGENT_MAX_CONCURRENT`).
    subagent_slots: Arc<Semaphore>,
    /// Box-wide gate shared by every workspace (`workspace::BOX_MAX_SUBAGENTS`).
    box_slots: Arc<Semaphore>,
    http: reqwest::Client,
    pub settings: crate::settings::Settings,
    /// Cached skills index (F5): shared + this Project's own, bodies included.
    pub skills: SkillsIndex,
    /// Pending approval cards and short-lived grants (feature `approve`).
    pub approvals: crate::approval::Approvals,
    /// Running subagents → (chat id, progress message id), so a file a subagent delivers lands
    /// on its own card (F10 `deliver_file`).
    pub subagent_msgs: Mutex<HashMap<String, (String, String)>>,
    /// Group-chat bookkeeping (chat kinds from the registry, this box's "Projects ⇄" tab).
    pub groups: crate::groups::GroupState,
    pub people: crate::people::PeopleState,
    /// Image inputs for the next pi prompt (set by `handle_send` from the message's attachments).
    pub turn_images: std::sync::Mutex<Vec<Value>>,
}

impl Agent {
    pub async fn pi_child(&self) -> Result<Arc<RpcChild>> {
        self.pi().await
    }

    pub fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn new(
        cfg: Arc<Config>,
        store: Store,
        sync: SyncHub,
        box_slots: Arc<Semaphore>,
    ) -> Arc<Self> {
        let cfg_for_settings = cfg.clone();
        let llm = Arc::new(Llm::new(&cfg));
        let memory = Arc::new(Memory::new(cfg.clone(), store.clone(), llm.clone()));
        let mut adapters: HashMap<Harness, Arc<dyn HarnessAdapter>> = HashMap::new();
        adapters.insert(
            Harness::Prime,
            Arc::new(PrimeAdapter::new(cfg.clone(), store.clone())),
        );
        adapters.insert(
            Harness::Pi,
            Arc::new(PiAdapter::new(cfg.clone(), store.clone())),
        );
        adapters.insert(
            Harness::ClaudeCode,
            Arc::new(ClaudeCodeAdapter::new(cfg.clone(), store.clone())),
        );
        Arc::new(Self {
            cfg,
            store,
            sync,
            llm,
            memory,
            pi: Mutex::new(None),
            current_chat: Mutex::new(None),
            turn: Arc::new(Mutex::new(None)),
            adapters,
            turn_running: AtomicBool::new(false),
            subagent_turns: std::sync::atomic::AtomicU32::new(0),
            pi_ready: AtomicBool::new(false),
            ext_loaded: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            turn_lock: Mutex::new(()),
            subagent_slots: Arc::new(Semaphore::new(SUBAGENT_MAX_CONCURRENT)),
            box_slots,
            http: reqwest::Client::new(),
            settings: crate::settings::Settings::new(&cfg_for_settings),
            skills: SkillsIndex::new(),
            approvals: crate::approval::Approvals::default(),
            subagent_msgs: Mutex::new(HashMap::new()),
            groups: crate::groups::GroupState::default(),
            people: crate::people::PeopleState::default(),
            turn_images: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// Refetch the skills index from the relay (60 s timer, `skills_rev` change, after a save).
    pub async fn refresh_skills(&self) {
        match self
            .skills
            .refresh(
                &self.http,
                &self.cfg.relay_url,
                &self.cfg.box_token,
                &self.cfg.project_id,
            )
            .await
        {
            Ok(n) => {
                tracing::debug!(skills = n, rev = %self.skills.rev(), "skills index refreshed")
            }
            Err(e) => tracing::debug!(error = %e, "skills index refresh"),
        }
    }

    /// Live model slots (registry/doc writes land here; `cfg` only holds the boot values).
    async fn live_models(&self) -> (String, String) {
        let m = self.settings.models.read().await;
        (m.talking.id.clone(), m.subagent.id.clone())
    }

    pub fn adapter(&self, h: Harness) -> Arc<dyn HarnessAdapter> {
        self.adapters.get(&h).cloned().expect("registered adapter")
    }

    // ---- pi child -----------------------------------------------------------------------------

    fn system_prompt_path(&self) -> std::path::PathBuf {
        self.cfg.state_dir.join("pi").join("system-prompt.md")
    }

    /// Router prompt (byte-stable, first) + project instructions + T0/T1 (design §6.9 layout).
    pub fn write_system_prompt(&self) -> Result<()> {
        let mut s = String::from(include_str!("../prompts/router.md"));
        // One box, many workspaces (D70): the project's files live in its own folder, not /work.
        s.push_str(&format!(
            "\n\n## This project\n\nProject \"{}\". Its folder is `{}`: every file you create or read for this project goes there (relative paths resolve there; do not use `/work` itself, other folders under `/work` belong to other projects).\n",
            self.cfg.project_name,
            self.cfg.work_dir.display()
        ));
        s.push('\n');
        s.push_str(&crate::media::prompt_section(&self.cfg));
        if !self.cfg.instructions.trim().is_empty() {
            s.push_str("\n\n## Project instructions\n\n");
            s.push_str(self.cfg.instructions.trim());
            s.push('\n');
        }
        let core = self.memory.core_memory_text();
        if !core.trim().is_empty() {
            s.push_str("\n\n## Project memory\n\n");
            s.push_str(&core);
        }
        std::fs::create_dir_all(self.system_prompt_path().parent().unwrap())?;
        std::fs::write(self.system_prompt_path(), s)?;
        Ok(())
    }

    /// Deterministic pi session file for a chat: `<sessions>/<ts>_<chatId>.jsonl` (one session per chat).
    fn session_file_for(&self, chat_id: &str) -> Result<std::path::PathBuf> {
        if let Some(p) = self.store.chat(chat_id)?.and_then(|c| c.pi_session_path) {
            if std::path::Path::new(&p).exists() {
                return Ok(p.into());
            }
        }
        let dir = self.cfg.pi_sessions_dir();
        std::fs::create_dir_all(&dir)?;
        if let Some(existing) = crate::harness::find_session_file(&dir, chat_id) {
            self.store
                .set_chat_session_path(chat_id, &existing.display().to_string())?;
            return Ok(existing);
        }
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S-%3fZ");
        let path = dir.join(format!("{ts}_{chat_id}.jsonl"));
        let header = json!({ "type": "session", "version": 3, "id": chat_id, "timestamp": chrono::Utc::now().to_rfc3339(), "cwd": self.cfg.work_dir });
        std::fs::write(&path, format!("{header}\n"))?;
        // pi appends to this file as the workspace user; agentd (root) created it.
        if let Some(ra) = &self.cfg.run_as {
            let _ = std::os::unix::fs::chown(&path, Some(ra.uid), Some(ra.gid));
        }
        self.store
            .set_chat_session_path(chat_id, &path.display().to_string())?;
        Ok(path)
    }

    pub async fn start_pi(self: &Arc<Self>, initial_chat: &str) -> Result<()> {
        self.write_system_prompt()?;
        let session = self.session_file_for(initial_chat)?;
        let (talking_model, _) = self.live_models().await;
        let mut args = PiAdapter::common_args(
            &self.cfg,
            &self.cfg.pi_sessions_dir(),
            &talking_model,
            &self.system_prompt_path(),
        );
        args.extend(["--session".into(), session.display().to_string()]);
        let log = self.cfg.state_dir.join("logs").join("pi-main.log");
        std::fs::create_dir_all(log.parent().unwrap())?;
        std::fs::create_dir_all(&self.cfg.work_dir)?;
        let t0 = Instant::now();
        let child = RpcChild::spawn(
            "pi-main",
            &self.cfg.pi_bin,
            &args,
            &PiAdapter::envs(&self.cfg, initial_chat, "main"),
            &self.cfg.work_dir,
            &log,
            self.cfg.run_as.as_ref(),
        )
        .await?;
        let state = child
            .call_timeout(
                RpcCommand::GetState {
                    id: "boot-state".into(),
                },
                Duration::from_secs(30),
            )
            .await?;
        if !state.success {
            return Err(anyhow!("pi get_state failed: {:?}", state.error));
        }
        tracing::info!(ms = t0.elapsed().as_millis() as i64, "pi main agent ready");
        *self.pi.lock().await = Some(child);
        *self.current_chat.lock().await = Some(initial_chat.to_owned());
        self.pi_ready.store(true, Ordering::SeqCst);
        self.store.upsert_session(
            initial_chat,
            None,
            Some(&session.display().to_string()),
            &self.cfg.work_dir.display().to_string(),
            &talking_model,
            "pi",
            "main",
        )?;
        let _ = self
            .post_timing("pi_boot_ms", t0.elapsed().as_millis() as i64, None)
            .await;
        // The extension writes an `extension_loaded` audit line on load.
        if let Ok(s) = std::fs::read_to_string(self.cfg.audit_path()) {
            if s.contains("\"extension_loaded\"") {
                self.ext_loaded.store(true, Ordering::SeqCst);
            }
        }
        // Supervise: respawn on exit (sandbox-image.md §3.3).
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let alive = me
                    .pi
                    .lock()
                    .await
                    .as_ref()
                    .map(|c| c.is_alive())
                    .unwrap_or(false);
                if me.shutting_down.load(Ordering::SeqCst) {
                    return;
                }
                if !alive && !me.turn_running.load(Ordering::SeqCst) {
                    me.pi_ready.store(false, Ordering::SeqCst);
                    let chat = me.current_chat.lock().await.clone().unwrap_or_default();
                    tracing::warn!("pi main child is gone; respawning");
                    if let Err(e) = me.respawn_pi(&chat).await {
                        tracing::error!(error = %e, "pi respawn failed");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });
        Ok(())
    }

    async fn respawn_pi(self: &Arc<Self>, chat_id: &str) -> Result<()> {
        if let Some(old) = self.pi.lock().await.take() {
            old.shutdown().await;
        }
        let session = self.session_file_for(chat_id)?;
        let (talking_model, _) = self.live_models().await;
        let mut args = PiAdapter::common_args(
            &self.cfg,
            &self.cfg.pi_sessions_dir(),
            &talking_model,
            &self.system_prompt_path(),
        );
        args.extend(["--session".into(), session.display().to_string()]);
        let log = self.cfg.state_dir.join("logs").join("pi-main.log");
        let child = RpcChild::spawn(
            "pi-main",
            &self.cfg.pi_bin,
            &args,
            &PiAdapter::envs(&self.cfg, chat_id, "main"),
            &self.cfg.work_dir,
            &log,
            self.cfg.run_as.as_ref(),
        )
        .await?;
        child
            .call_timeout(
                RpcCommand::GetState {
                    id: "boot-state".into(),
                },
                Duration::from_secs(30),
            )
            .await?;
        *self.pi.lock().await = Some(child);
        *self.current_chat.lock().await = Some(chat_id.to_owned());
        self.pi_ready.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn pi(&self) -> Result<Arc<RpcChild>> {
        self.pi
            .lock()
            .await
            .clone()
            .ok_or_else(|| anyhow!("pi main agent not running"))
    }

    async fn ensure_chat_session(&self, chat_id: &str) -> Result<()> {
        if self.current_chat.lock().await.as_deref() == Some(chat_id) {
            return Ok(());
        }
        let path = self.session_file_for(chat_id)?;
        let pi = self.pi().await?;
        let t0 = Instant::now();
        pi.call_ok(RpcCommand::SwitchSession {
            id: pi.next_id("sw"),
            session_path: path.display().to_string(),
        })
        .await?;
        tracing::info!(
            chat_id,
            ms = t0.elapsed().as_millis() as i64,
            "switched pi session"
        );
        *self.current_chat.lock().await = Some(chat_id.to_owned());
        let (talking_model, _) = self.live_models().await;
        self.store.upsert_session(
            chat_id,
            None,
            Some(&path.display().to_string()),
            &self.cfg.work_dir.display().to_string(),
            &talking_model,
            "pi",
            "main",
        )?;
        Ok(())
    }

    // ---- commands -----------------------------------------------------------------------------

    /// Mark-before-execute, then `done`/`failed` in the doc. The result is also returned for
    /// callers that report it elsewhere (routine runs, channel replies); the command loop ignores it.
    pub async fn handle_command(self: &Arc<Self>, chat_id: &str, cmd: CommandView) -> Result<()> {
        let Some(host) = self.sync.host(chat_id).await else {
            return Err(anyhow!("no room for chat {chat_id}"));
        };
        // People chats route by mention: a message not for this agent is left pending for
        // whichever agent (if any) it is for; the humans' own `send` commands are theirs.
        if cmd.kind == "send" && self.people.is_people_chat(chat_id) {
            let msg_id = cmd
                .payload
                .get("messageId")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !self.people_wants(chat_id, &host, msg_id).await {
                return Ok(());
            }
        }
        {
            host.lock()
                .await
                .doc
                .set_command_status(&cmd.id, CommandStatus::Taken, None);
        }
        let res = match cmd.kind.as_str() {
            "send" => self.handle_send(chat_id, &host, &cmd).await,
            "stop" => self.handle_stop(chat_id).await,
            "answer" => self.handle_send(chat_id, &host, &cmd).await,
            "retitle" => {
                let title = cmd
                    .payload
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                self.retitle(chat_id, &title).await
            }
            "retry" => Ok(()),
            "approve" | "reject" => {
                let approval_id = cmd
                    .payload
                    .get("approvalId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let outcome = if cmd.kind == "approve" {
                    ApprovalOutcome::Approved
                } else {
                    ApprovalOutcome::Rejected {
                        reason: cmd
                            .payload
                            .get("reason")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|r| !r.is_empty())
                            .map(str::to_owned),
                    }
                };
                self.decide_approval(&approval_id, outcome, &cmd.issued_by)
                    .await
            }
            other => Err(anyhow!("unknown command kind {other}")),
        };
        let h = host.lock().await;
        match &res {
            Ok(()) => h.doc.set_command_status(&cmd.id, CommandStatus::Done, None),
            Err(e) => {
                tracing::warn!(chat_id, cmd = %cmd.kind, error = %e, "command failed");
                h.doc.set_command_status(
                    &cmd.id,
                    CommandStatus::Failed,
                    Some(json!({ "error": e.to_string() })),
                );
            }
        }
        res
    }

    /// Abort the turn running in `chat_id` (people chat removal).
    pub(crate) async fn stop_turn(&self, chat_id: &str) -> Result<()> {
        self.handle_stop(chat_id).await
    }

    /// `stop` runs out of band (its own task, no turn lock): abort the pi turn now, mark the turn
    /// stopped so no hand-off follows, and tell the chat.
    async fn handle_stop(&self, chat_id: &str) -> Result<()> {
        let target = {
            let mut tl = self.turn.lock().await;
            match tl.as_mut() {
                Some(t) if t.chat_id == chat_id => {
                    t.stopped = true;
                    t.escalate = false;
                    Some(t.assistant_msg_id.clone())
                }
                _ => None,
            }
        };
        let Some(msg_id) = target else {
            tracing::info!(chat_id, "stop: no turn running for this chat");
            return Ok(());
        };
        if let Ok(pi) = self.pi().await {
            let _ = pi
                .call_timeout(
                    RpcCommand::Abort {
                        id: pi.next_id("ab"),
                    },
                    Duration::from_secs(5),
                )
                .await;
        }
        if let Some(host) = self.sync.host(chat_id).await {
            let mut h = host.lock().await;
            h.doc
                .add_status_part(&msg_id, "stopped", "Stopped by the user");
        }
        tracing::info!(chat_id, "stop: pi turn aborted");
        Ok(())
    }

    /// For an `answer` command with `{ questionId, value }`: `(question, rendered text)`. The
    /// widget comes from the ask row (falling back to the doc part); the value is rendered once,
    /// here, so the model, T3 and the bubble all read the same words. Also flips the part to
    /// `answered` and the ask row to answered.
    async fn resolve_structured_answer(
        &self,
        host: &SharedHost,
        cmd: &CommandView,
    ) -> Option<(String, String)> {
        if cmd.kind != "answer" {
            return None;
        }
        let question_id = cmd.payload.get("questionId").and_then(Value::as_str)?;
        let value = cmd.payload.get("value").cloned().unwrap_or(Value::Null);
        let (question, widget_json) = match self.store.ask(question_id) {
            Ok(Some((q, w, _))) => (q, w),
            Ok(None) | Err(_) => {
                let h = host.lock().await;
                let (_, q, w) = h
                    .doc
                    .open_question()
                    .filter(|(id, _, _)| id == question_id)?;
                (q, w)
            }
        };
        let rendered = match Widget::parse(widget_json.as_ref()) {
            Ok(Some(w)) => w.answer_text(&value),
            Ok(None) | Err(_) => {
                let text = cmd
                    .payload
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or(value.to_string())
                    });
                format!("Answer: {text}")
            }
        };
        let _ = self.store.answer_ask(question_id);
        {
            let mut h = host.lock().await;
            h.doc.answer_question_part(question_id, &value);
        }
        Some((question, rendered))
    }

    async fn handle_send(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        cmd: &CommandView,
    ) -> Result<()> {
        let _guard = self.turn_lock.lock().await;
        let user_msg_id = cmd
            .payload
            .get("messageId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        // A structured answer (`answer { questionId, value }`): render the value to text with the
        // widget the question was asked with, and mark the part answered before the turn runs.
        let structured = self.resolve_structured_answer(host, cmd).await;
        let (mut user_text, channel_note, message_view) = {
            let h = host.lock().await;
            let m = h.doc.message(&user_msg_id);
            let text = m.as_ref().map(|m| m.text()).unwrap_or_default();
            (text, m.as_ref().and_then(crate::channels::prompt_note), m)
        };
        if user_text.trim().is_empty() {
            if let Some((_, rendered)) = &structured {
                user_text = rendered.clone();
            }
        }
        // Attachments (images, voice notes, files): fetched into the workspace; images ride
        // along as vision input, a voice note's transcript becomes the text when none was typed.
        let attachments = match &message_view {
            Some(m) => self.turn_attachments(m).await,
            None => crate::attachments::TurnAttachments::default(),
        };
        if user_text.trim().is_empty() {
            if let Some(t) = &attachments.transcript {
                user_text = t.clone();
            }
        }
        if user_text.trim().is_empty() && !attachments.notes.is_empty() {
            user_text = "(attachment)".to_owned();
        }
        if user_text.trim().is_empty() {
            return Err(anyhow!("send: message {user_msg_id} not found or empty"));
        }
        let _ = self.store.touch_chat(chat_id);
        let _ = self.store.t3_append(
            chat_id,
            None,
            Some(chat_id),
            "user",
            Some("user"),
            &user_text,
            Some(&json!({ "message_id": user_msg_id })),
        );
        // Title from the first message (design §3.4) when the chat still has the default title.
        if let Ok(Some(c)) = self.store.chat(chat_id) {
            if c.title.is_empty() || c.title == "New chat" {
                let title: String = user_text
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(48)
                    .collect();
                let _ = self
                    .retitle(chat_id, title.trim_end_matches(['.', ',', ':', ';']).trim())
                    .await;
            }
        }
        // Answering an open question (S8): mark it answered; pi receives the text as the next prompt.
        let mut prompt_text = user_text.clone();
        if let Some((question, rendered)) = &structured {
            prompt_text = format!("Answer to your question \"{question}\":\n{rendered}");
        } else if let Ok(Some((ask_id, _, question))) = self.store.open_ask(chat_id) {
            let _ = self.store.answer_ask(&ask_id);
            prompt_text = format!("Answer to your question \"{question}\": {user_text}");
        }
        // A message that arrived through a channel (F7): the model learns where it came from and
        // that the reply is read as plain chat text.
        if let Some(note) = channel_note {
            prompt_text = format!("{note}\n\n{prompt_text}");
        }
        // Group chats (F6): the agent is told which room it speaks in; a user message in a
        // hosted group also goes out to the other participants.
        if let Some(gt) = self.group_turn_context(chat_id, host, &user_msg_id).await {
            if let Some(text) = &gt.fan_out_user_text {
                self.spawn_group_fan_out(&gt, text);
            }
            prompt_text = format!("{}{prompt_text}", gt.prelude);
        }
        // People chats (D72): who is asking, in what role; the reply is attributed to them.
        if let Some(prelude) = self.people_turn_context(chat_id, host, &user_msg_id).await {
            prompt_text = format!("{prelude}{prompt_text}");
        } else if let Some(note) = self
            .shared_project_guest_note(chat_id, host, &user_msg_id)
            .await
        {
            prompt_text = format!("{note}{prompt_text}");
        }
        if !attachments.notes.is_empty() {
            let mut extra = attachments.notes.join("\n");
            if let Some(t) = &attachments.transcript {
                if user_text.trim() != t.trim() {
                    extra = format!("{extra}\n[Voice note transcript]: {t}");
                }
            }
            prompt_text = format!("{extra}\n\n{prompt_text}");
        }
        *self.turn_images.lock().expect("turn images") = attachments.images;

        let outcome = self
            .drive_turn(chat_id, host, &user_msg_id, &prompt_text, TurnKind::User)
            .await;
        self.people.set_current(None);
        let outcome = outcome?;
        let TurnOutcome {
            assistant_msg_id,
            route_name: _,
            turn,
            result,
        } = outcome;

        // The model acknowledges a hand-off in one sentence (demo review item 2); only when it
        // wrote nothing at all does agentd fill the gap so the chat never shows a bare route.
        if matches!(
            turn.route,
            Some(ValidRoute::New { .. }) | Some(ValidRoute::Wake { .. })
        ) && turn.streamed_text.trim().is_empty()
        {
            let mut h = host.lock().await;
            if let Some(p) = h.doc.add_text_part(&assistant_msg_id, "text") {
                h.doc.append_text(
                    &assistant_msg_id,
                    &p,
                    "Handing this to a subagent; I will post its result here.",
                );
            }
            tracing::info!(chat_id, "ack_fallback: model wrote no sentence after route");
        }

        self.spawn_memory_pass(chat_id, &assistant_msg_id, &user_text, &turn.streamed_text);

        // Hand-offs after the main turn: new / wake / escalation. Never on the chat's critical path.
        if turn.stopped {
            return result;
        }
        let followup = match (&turn.route, turn.escalate) {
            (
                Some(ValidRoute::New {
                    name,
                    purpose,
                    brief,
                    harness,
                    isolate: _,
                    data_heavy,
                }),
                _,
            ) => Some((
                None,
                name.clone(),
                purpose.clone(),
                brief.clone(),
                *harness,
                *data_heavy,
                false,
            )),
            (Some(ValidRoute::Wake { subagent_id, brief }), _) => Some((
                Some(subagent_id.clone()),
                String::new(),
                String::new(),
                brief.clone(),
                None,
                false,
                false,
            )),
            (Some(ValidRoute::Act { plan }), true) => {
                let brief = format!(
                    "Continue this task that the project agent started and could not finish within its limits.\n\nUser request: {user_text}\n\nPlan: {plan}\n\nAct so far:\n{}\n\nFinish the task.",
                    turn.act_transcript.join("\n").chars().take(2400).collect::<String>()
                );
                Some((
                    None,
                    "continue-act".into(),
                    "Finish a task the project agent started".into(),
                    brief,
                    None,
                    false,
                    true,
                ))
            }
            _ => None,
        };
        if let Some((existing, name, purpose, brief, harness, data_heavy, escalated)) = followup {
            self.spawn_subagent_task(
                chat_id,
                host,
                &user_msg_id,
                existing,
                name,
                purpose,
                brief,
                harness,
                data_heavy,
                escalated,
            );
        }
        result
    }

    /// Run the subagent in the background and report a start failure into the chat.
    #[allow(clippy::too_many_arguments)]
    fn spawn_subagent_task(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        user_msg_id: &str,
        existing: Option<String>,
        name: String,
        purpose: String,
        brief: String,
        harness: Option<Harness>,
        data_heavy: bool,
        escalated: bool,
    ) {
        let me = self.clone();
        let chat = chat_id.to_owned();
        let host2 = host.clone();
        let user_msg = user_msg_id.to_owned();
        tokio::spawn(async move {
            if let Err(e) = me
                .run_subagent(
                    &chat,
                    &host2,
                    &user_msg,
                    existing.clone(),
                    &name,
                    &purpose,
                    &brief,
                    harness,
                    data_heavy,
                    escalated,
                )
                .await
            {
                tracing::warn!(error = %e, "subagent run failed");
                let label = if name.is_empty() {
                    existing.clone().unwrap_or_else(|| "subagent".into())
                } else {
                    name.clone()
                };
                {
                    let mut h = host2.lock().await;
                    let id = ulid();
                    h.doc
                        .start_assistant_message(&id, &user_msg, &me.cfg.device_id);
                    h.doc.add_status_part(&id, "failed", "Failed");
                    h.doc.add_error_part(&id, "subagent_failed", &e.to_string());
                    h.doc.finish_message(&id, MessageStatus::Error);
                }
                // The main agent must learn about the failure too, or its next turn still believes
                // the hand-off is running; it also tells the user in prose.
                if let Err(e2) = me
                    .run_result_turn(
                        &chat,
                        &host2,
                        &user_msg,
                        existing.as_deref().unwrap_or(""),
                        &label,
                        "failed",
                        0,
                        "(the subagent did not start)",
                        Some(&e.to_string()),
                    )
                    .await
                {
                    tracing::warn!(error = %e2, "result turn after a failed start");
                }
            }
        });
    }

    /// Memory write pass + compaction, off the reply path (design §6.9).
    fn spawn_memory_pass(self: &Arc<Self>, chat_id: &str, msg_id: &str, ask: &str, reply: &str) {
        let me = self.clone();
        let chat = chat_id.to_owned();
        let text = format!("User: {ask}\nAgent: {reply}");
        let msg = msg_id.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            if let Err(e) = me
                .memory
                .write_pass(&msg, &chat, "assistant", &text, &[])
                .await
            {
                tracing::debug!(error = %e, "memory write pass");
            }
            if let Err(e) = me.memory.maybe_compact(&chat).await {
                tracing::debug!(error = %e, "compaction");
            }
            let _ = me.write_system_prompt();
        });
    }

    /// One prompt to the main pi session, folded into a new assistant message. The caller holds
    /// `turn_lock`. Finalizes the message (decision, usage, status), T3, and turn metrics.
    async fn drive_turn(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        user_msg_id: &str,
        prompt_text: &str,
        kind: TurnKind,
    ) -> Result<TurnOutcome> {
        self.ensure_chat_session(chat_id).await?;

        let assistant_msg_id = ulid();
        let text_part_id = {
            let mut h = host.lock().await;
            let pid =
                h.doc
                    .start_assistant_message(&assistant_msg_id, user_msg_id, &self.cfg.device_id);
            match &kind {
                TurnKind::User => {
                    h.doc
                        .add_status_part(&assistant_msg_id, "working", "Working…");
                    if let Some(attr) = self.people_reply_fields(chat_id) {
                        h.doc.set_message_fields(&assistant_msg_id, &attr.fields);
                        if let Some(q) = attr.quote {
                            h.doc.append_text(&assistant_msg_id, &pid, &q);
                        }
                    }
                }
                TurnKind::SubagentResult { subagent_id, name } => {
                    h.doc
                        .add_status_part(&assistant_msg_id, "working", "Working…");
                    h.doc.set_message_fields(
                        &assistant_msg_id,
                        &[
                            ("subagentId", json!(subagent_id)),
                            ("subagentName", json!(name)),
                        ],
                    );
                }
                TurnKind::ApprovalOutcome { approval_id, title } => {
                    h.doc.add_status_part(
                        &assistant_msg_id,
                        "working",
                        &format!("Following up on “{title}”…"),
                    );
                    h.doc.set_message_fields(
                        &assistant_msg_id,
                        &[("approvalId", json!(approval_id))],
                    );
                }
            }
            pid
        };

        *self.turn.lock().await = Some(TurnState {
            chat_id: chat_id.to_owned(),
            user_msg_id: user_msg_id.to_owned(),
            assistant_msg_id: assistant_msg_id.clone(),
            text_part_id: text_part_id.clone(),
            started: Instant::now(),
            route: None,
            route_attempts: 0,
            route_failed: false,
            escalate: false,
            asked: None,
            metrics: TurnMetrics::default(),
            streamed_text: String::new(),
            act_transcript: Vec::new(),
            tool_inputs: HashMap::new(),
            reply_extractor: ReplyExtractor::default(),
            prose: ProseFilter::default(),
            act_buffer: String::new(),
            route_call_id: None,
            text_part_dirty: false,
            stopped: false,
            prompt_text: match &kind {
                TurnKind::User => prompt_text.to_owned(),
                TurnKind::SubagentResult { .. } | TurnKind::ApprovalOutcome { .. } => String::new(),
            },
            skills_used: Vec::new(),
        });
        self.turn_running.store(true, Ordering::SeqCst);
        let keepalive = self.spawn_keepalive();
        let result = self.run_main_turn(chat_id, host, prompt_text).await;
        self.turn_running.store(false, Ordering::SeqCst);
        keepalive.abort();

        // Whatever the filter still holds back at the end of the turn is judged and written.
        {
            let mut tl = self.turn.lock().await;
            if let Some(t) = tl.as_mut() {
                let held = t.prose.flush();
                if !held.is_empty() {
                    self.stage_text(host, t, &held).await;
                }
                let final_text = std::mem::take(&mut t.act_buffer);
                if !final_text.trim().is_empty() {
                    self.write_text(host, t, final_text.trim_start()).await;
                }
            }
        }
        let turn = self.turn.lock().await.take();
        let Some(turn) = turn else {
            return Err(anyhow!("turn state vanished mid-turn"));
        };
        let total_ms = turn.started.elapsed().as_millis() as i64;
        let route_name = turn
            .route
            .as_ref()
            .map(route_name)
            .unwrap_or(if turn.route_failed { "reply" } else { "none" });
        let turn_kind = match &kind {
            TurnKind::User => route_name,
            TurnKind::SubagentResult { .. } => "subagent_result",
            TurnKind::ApprovalOutcome { .. } => "approval_outcome",
        };
        let (final_status, err_text) = match &result {
            Ok(()) => (MessageStatus::Done, None),
            Err(e) => (MessageStatus::Error, Some(e.to_string())),
        };
        let (talking_model, _) = self.live_models().await;
        let room_name;
        {
            let mut h = host.lock().await;
            if let Some(e) = &err_text {
                h.doc.add_error_part(&assistant_msg_id, "turn_failed", e);
            }
            h.doc.set_message_fields(
                &assistant_msg_id,
                &[
                    ("decision", json!(route_name)),
                    ("turn", json!({ "kind": turn_kind })),
                    ("model", json!(talking_model)),
                    ("usage", turn.metrics.usage.clone().unwrap_or(Value::Null)),
                ],
            );
            h.doc.finish_message(&assistant_msg_id, final_status);
            room_name = h.room.clone();
        }
        if matches!(final_status, MessageStatus::Done) && !turn.streamed_text.trim().is_empty() {
            self.announce_reply(&room_name, &assistant_msg_id, &turn.streamed_text);
        }
        if !turn.streamed_text.trim().is_empty() {
            let _ = self.store.t3_append(
                chat_id,
                None,
                Some(chat_id),
                "assistant",
                Some("assistant"),
                &turn.streamed_text,
                Some(&json!({ "message_id": assistant_msg_id, "route": route_name, "turn": turn_kind })),
            );
        }
        let mut usage_rec = turn.metrics.usage.clone().unwrap_or(json!({}));
        if turn.metrics.route_missing {
            usage_rec["route_missing"] = json!(true);
        }
        if turn.metrics.route_corrected {
            usage_rec["route_corrected"] = json!(true);
        }
        let _ = self.store.record_turn(
            chat_id,
            user_msg_id,
            Some(turn_kind),
            turn.metrics.ttft_ms,
            turn.metrics.route_ms,
            total_ms,
            turn.metrics.tool_calls,
            turn.escalate,
            Some(&usage_rec),
        );
        if let Some(ttft) = turn.metrics.ttft_ms {
            let _ = self
                .post_timing("main_turn_ttft_ms", ttft, Some(turn_kind))
                .await;
        }
        let _ = self
            .post_timing("main_turn_total_ms", total_ms, Some(turn_kind))
            .await;
        tracing::info!(chat_id, route = route_name, turn = turn_kind, ttft_ms = ?turn.metrics.ttft_ms, route_ms = ?turn.metrics.route_ms, total_ms, tool_calls = turn.metrics.tool_calls, "turn done");
        Ok(TurnOutcome {
            assistant_msg_id,
            route_name,
            turn,
            result,
        })
    }

    /// A finished subagent never speaks to the user directly: its raw output stays hidden and the
    /// main agent writes the update in prose, as a follow-up turn in the same chat (demo review
    /// item 1). Waits for `turn_lock`, so it lands after whatever the user is doing right now.
    #[allow(clippy::too_many_arguments)]
    async fn run_result_turn(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        user_msg_id: &str,
        subagent_id: &str,
        name: &str,
        status: &str,
        total_ms: i64,
        raw: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let _guard = self.turn_lock.lock().await;
        let ask = {
            let h = host.lock().await;
            h.doc
                .message(user_msg_id)
                .map(|m| m.text())
                .unwrap_or_default()
        };
        let recent = self
            .store
            .t3_thread(chat_id, None, None, 12)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.kind == "user" || r.kind == "assistant")
            .map(|r| {
                format!(
                    "- {}: {}",
                    r.role.unwrap_or_default(),
                    r.text
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(240)
                        .collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let raw_trim: String = raw.chars().take(7000).collect();
        let mut prompt = format!(
            "[Arbos] Subagent \"{name}\" finished · status {status} · {:.0} s.\n\nThe user originally asked: {}\n",
            total_ms as f64 / 1000.0,
            if ask.trim().is_empty() { "(see the brief)" } else { ask.trim() },
        );
        if !recent.is_empty() {
            prompt.push_str(&format!("\nRecent turns in this chat:\n{recent}\n"));
        }
        if let Some(e) = error {
            prompt.push_str(&format!("\nThe run ended with an error: {e}\n"));
        }
        prompt.push_str(&format!(
            "\nSubagent output (for you only; the user has not seen it):\n<<<\n{raw_trim}\n>>>\n\nRoute `reply` and write the update for the user like a colleague in chat: what got done and the outcome, what changed that they care about, and what is open or needs their answer. Speak as if you did the work; do not mention the subagent, its name, the harness, the model, tool calls, commands, scratch paths, or token counts. No JSON, no quoted output, no list of steps. Two to five sentences."
        ));
        let kind = TurnKind::SubagentResult {
            subagent_id: subagent_id.to_owned(),
            name: name.to_owned(),
        };
        let outcome = self
            .drive_turn(chat_id, host, user_msg_id, &prompt, kind)
            .await?;
        // A result turn relays; it does not start further work. Any other route is logged only.
        if !matches!(outcome.turn.route, Some(ValidRoute::Reply { .. }) | None) {
            tracing::warn!(
                route = outcome.route_name,
                "result turn routed away from reply; ignored"
            );
        }
        self.spawn_memory_pass(
            chat_id,
            &outcome.assistant_msg_id,
            &format!("[subagent {name} finished: {status}]"),
            &outcome.turn.streamed_text,
        );
        outcome.result
    }

    /// A `[Arbos]` notice to the main agent as its own turn in `chat_id` (approval outcomes).
    /// Waits for `turn_lock`, so it lands after whatever the user is doing right now.
    pub async fn run_notice_turn(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        in_reply_to: &str,
        prompt: &str,
        kind: TurnKind,
    ) -> Result<()> {
        let _guard = self.turn_lock.lock().await;
        let outcome = self
            .drive_turn(chat_id, host, in_reply_to, prompt, kind)
            .await?;
        self.spawn_memory_pass(
            chat_id,
            &outcome.assistant_msg_id,
            "[approval outcome]",
            &outcome.turn.streamed_text,
        );
        outcome.result
    }

    fn spawn_keepalive(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            let _ = me.keepalive(true).await;
            let mut t = tokio::time::interval(Duration::from_secs(5 * 60));
            t.tick().await;
            loop {
                t.tick().await;
                let _ = me.keepalive(true).await;
            }
        })
    }

    /// Rename a chat: registry (`PATCH /projects/{id}/chats/{chat}`), local index, and the project
    /// doc `chats` list on the next poll.
    pub async fn retitle(&self, chat_id: &str, title: &str) -> Result<()> {
        if title.is_empty() {
            return Err(anyhow!("retitle: empty title"));
        }
        if let Ok(Some(c)) = self.store.chat(chat_id) {
            let _ = self
                .store
                .upsert_chat(chat_id, title, c.is_pinned, c.created_at);
        }
        let r = self
            .http
            .patch(format!(
                "{}/projects/{}/chats/{}",
                self.cfg.relay_url, self.cfg.project_id, chat_id
            ))
            .bearer_auth(&self.cfg.box_token)
            .json(&json!({ "title": title }))
            .send()
            .await?;
        if !r.status().is_success() {
            return Err(anyhow!("retitle: relay HTTP {}", r.status()));
        }
        Ok(())
    }

    pub async fn keepalive(&self, turn_running: bool) -> Result<()> {
        self.http
            .post(format!("{}/box/keepalive", self.cfg.relay_url))
            .bearer_auth(&self.cfg.box_token)
            .json(&json!({ "turn_running": turn_running }))
            .send()
            .await?;
        Ok(())
    }

    /// Tell the relay a whole reply landed, so people who are not looking get a notification
    /// (`POST /chats/{id}/notify` for a people chat, `POST /projects/{p}/chats/{c}/notify`
    /// otherwise). Fire-and-forget: the message is in the doc either way.
    pub fn announce_reply(self: &Arc<Self>, room: &str, message_id: &str, text: &str) {
        let Some((project, chat_id)) = room.split_once('/') else {
            return;
        };
        let url = if project == crate::people::PEOPLE_PROJECT {
            format!("{}/chats/{chat_id}/notify", self.cfg.relay_url)
        } else {
            format!(
                "{}/projects/{}/chats/{chat_id}/notify",
                self.cfg.relay_url, self.cfg.project_id
            )
        };
        let body = json!({
            "message_id": message_id,
            "text": text.chars().take(600).collect::<String>(),
            "project_id": self.cfg.project_id,
        });
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me
                .http
                .post(url)
                .bearer_auth(&me.cfg.box_token)
                .json(&body)
                .send()
                .await
            {
                tracing::debug!(error = %e, "notify relay of reply failed");
            }
        });
    }

    pub async fn post_timing(&self, kind: &str, ms: i64, detail: Option<&str>) -> Result<()> {
        self.http
            .post(format!(
                "{}/projects/{}/timings",
                self.cfg.relay_url, self.cfg.project_id
            ))
            .bearer_auth(&self.cfg.box_token)
            .json(&json!({ "kind": kind, "ms": ms, "detail": detail }))
            .send()
            .await?;
        Ok(())
    }

    /// Upload a blob to the relay mirror, fire-and-forget. Returns the key.
    pub fn upload_blob(self: &Arc<Self>, bytes: Vec<u8>, mime: &str) -> String {
        use sha2::Digest;
        let key = hex::encode(sha2::Sha256::digest(&bytes));
        let me = self.clone();
        let mime = mime.to_owned();
        let k = key.clone();
        tokio::spawn(async move {
            let _ = me
                .http
                .put(format!(
                    "{}/blob/{}/{}",
                    me.cfg.relay_url, me.cfg.project_id, k
                ))
                .bearer_auth(&me.cfg.box_token)
                .header("content-type", mime)
                .body(bytes)
                .send()
                .await;
        });
        key
    }

    // ---- the main turn ------------------------------------------------------------------------

    async fn run_main_turn(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        prompt_text: &str,
    ) -> Result<()> {
        let pi = self.pi().await?;
        let mut events = pi.subscribe();
        let images = std::mem::take(&mut *self.turn_images.lock().expect("turn images"));
        let r = pi
            .call(RpcCommand::Prompt {
                id: pi.next_id("p"),
                message: prompt_text.to_owned(),
                streaming_behavior: None,
                images,
            })
            .await?;
        if !r.success {
            return Err(anyhow!("prompt rejected: {}", r.error.unwrap_or_default()));
        }
        let started = Instant::now();
        let mut act_steered = false;
        let mut act_aborted = false;
        let deadline = Duration::from_secs(15 * 60);
        // Time the turn spends blocked on an approval card is the user's, not the agent's: it
        // counts toward neither the act limit nor the turn deadline.
        let mut approval_paused = Duration::ZERO;
        let mut approval_pause_started: Option<Instant> = None;
        loop {
            let ev = tokio::select! {
                e = events.recv() => e,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    let waiting = self.approval_pending_for(chat_id).await;
                    match (waiting, approval_pause_started) {
                        (true, None) => approval_pause_started = Some(Instant::now()),
                        (false, Some(t0)) => {
                            approval_paused += t0.elapsed();
                            approval_pause_started = None;
                        }
                        (true, Some(_)) | (false, None) => {}
                    }
                    if waiting {
                        continue;
                    }
                    let elapsed = started.elapsed().saturating_sub(approval_paused);
                    // Act wall-clock limit (D66): steer at 60 s, abort 20 s later.
                    let is_act = matches!(self.turn.lock().await.as_ref().and_then(|t| t.route.as_ref()), Some(ValidRoute::Act { .. }));
                    if is_act && elapsed > Duration::from_secs(ACT_MAX_WALL_SECS) && !act_steered {
                        act_steered = true;
                        let _ = pi.call(RpcCommand::Steer { id: pi.next_id("st"), message: "Time limit reached. Stop now and summarize what you did and what is left.".into() }).await;
                        if let Some(t) = self.turn.lock().await.as_mut() { t.escalate = true; }
                    }
                    if is_act && elapsed > Duration::from_secs(ACT_MAX_WALL_SECS + 20) && !act_aborted {
                        act_aborted = true;
                        let _ = pi.call(RpcCommand::Abort { id: pi.next_id("ab") }).await;
                    }
                    if elapsed > deadline {
                        let _ = pi.call(RpcCommand::Abort { id: pi.next_id("ab") }).await;
                        return Err(anyhow!("turn exceeded 15 minutes"));
                    }
                    continue;
                }
            };
            let ev = match ev {
                Ok(e) => e,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(n, "lagged behind pi events");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(anyhow!("pi event stream closed"))
                }
            };
            if self.fold_main_event(chat_id, host, &ev, started).await? {
                break;
            }
            if matches!(ev, Event::Other) && !pi.is_alive() {
                return Err(anyhow!("pi main child exited mid-turn"));
            }
        }
        Ok(())
    }

    /// Fold one pi event into the chat doc and the turn state. Returns true when the turn settled.
    async fn fold_main_event(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        ev: &Event,
        started: Instant,
    ) -> Result<bool> {
        let mut tl = self.turn.lock().await;
        let Some(t) = tl.as_mut() else {
            return Ok(true);
        };
        let elapsed = started.elapsed().as_millis() as i64;
        match ev {
            Event::MessageUpdate {
                assistant_message_event,
                usage,
            } => {
                if let Some(u) = usage {
                    if u.total_tokens > 0 {
                        t.metrics.usage = Some(
                            json!({ "input": u.input, "output": u.output, "cacheRead": u.cache_read, "cacheWrite": u.cache_write, "cost": u.cost }),
                        );
                    }
                }
                match assistant_message_event {
                    AssistantEvent::TextDelta { delta, .. } => {
                        if t.metrics.ttft_ms.is_none() {
                            t.metrics.ttft_ms = Some(elapsed);
                        }
                        self.append_text(host, t, delta).await;
                    }
                    AssistantEvent::ThinkingDelta { .. } => {
                        if t.metrics.ttft_ms.is_none() {
                            t.metrics.ttft_ms = Some(elapsed);
                        }
                    }
                    AssistantEvent::ToolcallStart { id, tool_name, .. } => {
                        if t.metrics.ttft_ms.is_none() {
                            t.metrics.ttft_ms = Some(elapsed);
                        }
                        if tool_name == "route" {
                            t.route_call_id = Some(id.clone());
                            t.reply_extractor = ReplyExtractor::default();
                        }
                    }
                    AssistantEvent::ToolcallDelta {
                        delta,
                        content_index: _,
                    } => {
                        if t.route_call_id.is_some() && t.route.is_none() {
                            let text = t.reply_extractor.feed(delta);
                            if !text.is_empty() {
                                self.append_text(host, t, &text).await;
                            }
                        }
                    }
                    AssistantEvent::ToolcallEnd { .. }
                    | AssistantEvent::Start
                    | AssistantEvent::TextStart { .. }
                    | AssistantEvent::TextEnd { .. }
                    | AssistantEvent::ThinkingStart { .. }
                    | AssistantEvent::ThinkingEnd { .. }
                    | AssistantEvent::Done { .. }
                    | AssistantEvent::Unknown => {}
                    AssistantEvent::Error { error, .. } => {
                        let mut h = host.lock().await;
                        h.doc.add_error_part(
                            &t.assistant_msg_id,
                            "model_error",
                            &error
                                .as_ref()
                                .map(|e| e.to_string())
                                .unwrap_or_else(|| "provider error".into()),
                        );
                    }
                }
            }
            Event::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                if tool_name == "route" {
                    return Ok(false);
                }
                t.tool_inputs.insert(tool_call_id.clone(), args.clone());
                t.metrics.tool_calls += 1;
                let one_line = args.to_string().chars().take(120).collect::<String>();
                t.act_transcript.push(format!("→ {tool_name} {one_line}"));
                // No `step` event in the doc (one working indicator only); the hidden tool part
                // and T3 keep the detail for the drawer.
                let _ = self.store.t3_append(
                    chat_id,
                    None,
                    Some(chat_id),
                    "tool_call",
                    Some("assistant"),
                    &step_label(tool_name, args),
                    Some(&json!({ "call_id": tool_call_id, "input": args })),
                );
                let held = t.prose.flush();
                if !held.is_empty() {
                    self.stage_text(host, t, &held).await;
                }
                let interim = std::mem::take(&mut t.act_buffer);
                if !interim.trim().is_empty() {
                    let _ = self.store.t3_append(
                        chat_id,
                        None,
                        Some(chat_id),
                        "interim",
                        Some("assistant"),
                        interim.trim(),
                        Some(&json!({ "before_call": tool_call_id })),
                    );
                }
                t.text_part_dirty = true;
            }
            Event::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
                ..
            } => {
                if tool_name == "route" {
                    if t.metrics.route_ms.is_none() {
                        t.metrics.route_ms = Some(elapsed);
                    }
                    if t.route.is_none() && !t.route_failed {
                        // A route the hook never saw (no agentd reachable): validate from the result args is impossible; treat as reply.
                    }
                    return Ok(false);
                }
                let output = result
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| c.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                // D19 at write time: the doc copy gets the same redaction as T3.
                let output = crate::store::redact(&output);
                t.act_transcript.push(format!(
                    "← {tool_name}: {}",
                    output.chars().take(300).collect::<String>()
                ));
                let _ = self.store.t3_append(
                    chat_id,
                    None,
                    Some(chat_id),
                    "tool_output",
                    Some("tool"),
                    &format!("{tool_name}: {output}"),
                    Some(&json!({ "call_id": tool_call_id })),
                );
                let summary: String = output.chars().take(TOOL_OUTPUT_SUMMARY_MAX).collect();
                let output_ref = if output.len() > TOOL_OUTPUT_SUMMARY_MAX {
                    Some(self.upload_blob(output.clone().into_bytes(), "text/plain"))
                } else {
                    None
                };
                let input = t.tool_inputs.remove(tool_call_id).unwrap_or(json!({}));
                let mut h = host.lock().await;
                h.doc.add_tool_part(
                    &t.assistant_msg_id,
                    tool_call_id,
                    tool_name,
                    &input,
                    &summary,
                    output_ref.as_deref(),
                    Some(output.len() as i64),
                    *is_error,
                );
                t.text_part_dirty = true;
            }
            Event::AgentSettled { .. } => return Ok(true),
            Event::CompactionEnd { .. } => {
                let _ = self.store.bump_session_compactions(chat_id);
                let _ = self.store.t3_append(
                    chat_id,
                    None,
                    Some(chat_id),
                    "compaction",
                    Some("system"),
                    "pi session compacted",
                    None,
                );
            }
            Event::Other
            | Event::AgentStart
            | Event::AgentEnd { .. }
            | Event::TurnStart
            | Event::TurnEnd { .. }
            | Event::MessageStart { .. }
            | Event::MessageEnd { .. }
            | Event::ToolExecutionUpdate { .. }
            | Event::QueueUpdate { .. }
            | Event::EntryAppended { .. }
            | Event::CompactionStart { .. }
            | Event::ExtensionUiRequest { .. } => {}
        }
        Ok(false)
    }

    /// Main-agent prose enters the doc through the boilerplate filter.
    async fn append_text(&self, host: &SharedHost, t: &mut TurnState, delta: &str) {
        let out = t.prose.feed(delta);
        if !out.is_empty() {
            self.stage_text(host, t, &out).await;
        }
    }

    /// Reply turns stream straight into the doc; act turns hold the text until the turn settles
    /// (only what follows the last tool call is the answer).
    async fn stage_text(&self, host: &SharedHost, t: &mut TurnState, text: &str) {
        if matches!(t.route, Some(ValidRoute::Act { .. })) {
            t.act_buffer.push_str(text);
        } else {
            self.write_text(host, t, text).await;
        }
    }

    async fn write_text(&self, host: &SharedHost, t: &mut TurnState, delta: &str) {
        let mut h = host.lock().await;
        if t.text_part_dirty {
            // A tool part landed since the last text: open a new text part so order is kept.
            if let Some(p) = h.doc.add_text_part(&t.assistant_msg_id, "text") {
                t.text_part_id = p;
            }
            t.text_part_dirty = false;
        }
        h.doc
            .append_text(&t.assistant_msg_id, &t.text_part_id, delta);
        t.streamed_text.push_str(delta);
    }

    // ---- hooks called by arbos-pi-ext ---------------------------------------------------------

    /// `/hook/route`: validate; one retry; a second failure is treated as `reply` (design §6.2).
    pub async fn hook_route(&self, args: &Value) -> Result<(), String> {
        let mut tl = self.turn.lock().await;
        let Some(t) = tl.as_mut() else {
            return Err("no turn is running".into());
        };
        t.route_attempts += 1;
        match validate(args) {
            Ok(r) => {
                tracing::info!(
                    route = route_name(&r),
                    attempt = t.route_attempts,
                    "route accepted"
                );
                let _ = self.store.t3_append(
                    &t.chat_id,
                    None,
                    Some(&t.chat_id),
                    "route",
                    Some("system"),
                    &format!("route {}: {}", route_name(&r), args),
                    None,
                );
                t.route = Some(r);
                Ok(())
            }
            Err(e) => {
                tracing::warn!(error = %e, attempt = t.route_attempts, "route invalid");
                if t.route_attempts >= 2 {
                    t.route_failed = true;
                    t.route = Some(ValidRoute::Reply { reply: None });
                }
                Err(e)
            }
        }
    }

    /// `ask_user(question, widget?)` from any harness seam. Writes the `question` part (with the
    /// typed widget when one was given) plus the legacy `waiting_user` status part, and records
    /// the ask so the `answer` command can render the value for the model. Returns the question id.
    pub async fn hook_ask_user(
        &self,
        question: &str,
        widget: Option<&Value>,
        session_id: &str,
    ) -> Result<String> {
        let widget = match Widget::parse(widget) {
            Ok(w) => w,
            Err(e) => {
                // An invalid widget degrades to a plain question rather than losing the turn.
                tracing::warn!(error = %e, "ask_user widget rejected; asking as plain text");
                None
            }
        };
        let widget_json = widget.as_ref().and_then(|w| serde_json::to_value(w).ok());
        let mut tl = self.turn.lock().await;
        let chat_id = tl
            .as_ref()
            .map(|t| t.chat_id.clone())
            .unwrap_or_else(|| session_id.to_owned());
        let id = ulid();
        self.store.insert_ask(
            &id,
            Some(&chat_id),
            Some(session_id),
            question,
            widget_json.as_ref(),
        )?;
        if let Some(t) = tl.as_mut() {
            t.asked = Some(question.to_owned());
            if let Some(host) = self.sync.host(&t.chat_id).await {
                let mut h = host.lock().await;
                h.doc
                    .add_question_part(&t.assistant_msg_id, &id, question, widget_json.as_ref());
                h.doc
                    .add_status_part(&t.assistant_msg_id, "waiting_user", question);
                h.doc
                    .set_message_fields(&t.assistant_msg_id, &[("waitingUser", json!(true))]);
            }
        }
        let _ = self.store.t3_append(
            &chat_id,
            None,
            Some(session_id),
            "ask_user",
            Some("assistant"),
            question,
            widget_json
                .as_ref()
                .map(|w| json!({ "question_id": id, "widget": w }))
                .as_ref(),
        );
        Ok(id)
    }

    pub async fn hook_act_limit(&self) {
        if let Some(t) = self.turn.lock().await.as_mut() {
            t.escalate = true;
        }
    }

    /// Working-set section (design §6.6 item 2b): running subagents + compaction summary +
    /// handles + notes, ≤ 40 lines.
    pub fn working_set_text(&self, chat_id: &str) -> String {
        let mut lines = Vec::new();
        let running: Vec<_> = self
            .store
            .subagents()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.status == "running" || s.status == "queued")
            .collect();
        if !running.is_empty() {
            lines.push(format!(
                "Subagents running now ({}): their results arrive as later [Arbos] messages; do not start the same work twice.",
                running.len()
            ));
            for s in running.iter().take(SUBAGENT_MAX_CONCURRENT + 2) {
                lines.push(format!(
                    "- {} ({}, {}): {}",
                    s.name,
                    s.id,
                    s.status,
                    if s.last_status.is_empty() {
                        "starting"
                    } else {
                        s.last_status.as_str()
                    }
                ));
            }
        }
        if let Ok(Some(c)) = self.store.chat(chat_id) {
            if let Some(s) = c.compaction_summary {
                lines.push(format!(
                    "Summary of earlier turns in this chat: {}",
                    s.chars().take(6000).collect::<String>()
                ));
            }
        }
        for h in self.store.handles_for_chat(chat_id, 20).unwrap_or_default() {
            lines.push(format!(
                "- {} ({}, {} chars, {} lines): {}",
                h.id, h.kind, h.chars, h.lines, h.note
            ));
        }
        for (k, v) in self.store.scratch_all(chat_id).unwrap_or_default() {
            lines.push(format!(
                "- remember:{k} = {}",
                v.chars().take(120).collect::<String>()
            ));
        }
        lines.into_iter().take(40).collect::<Vec<_>>().join("\n")
    }

    /// "Relevant skills" section for the running turn (F5): selected against the prompt text
    /// within `SECTION_BUDGET_TOKENS`; empty when nothing matches or no turn is running. The
    /// first call per turn records the chosen slugs on the assistant message (Details drawer).
    pub async fn skills_section_text(&self) -> String {
        let (chat_id, msg_id, prompt, first) = {
            let mut tl = self.turn.lock().await;
            let Some(t) = tl.as_mut() else {
                return String::new();
            };
            if t.prompt_text.trim().is_empty() {
                return String::new();
            }
            let first = t.skills_used.is_empty();
            (
                t.chat_id.clone(),
                t.assistant_msg_id.clone(),
                t.prompt_text.clone(),
                first,
            )
        };
        let index = self.skills.snapshot();
        let (selected, text) = skills::section_for(&prompt, &index, skills::SECTION_BUDGET_TOKENS);
        if selected.is_empty() {
            return String::new();
        }
        let slugs: Vec<String> = selected.iter().map(|s| s.slug.clone()).collect();
        if first {
            tracing::info!(
                chat_id,
                skills = ?slugs,
                tokens = arbos_core::skill::estimate_tokens(&text),
                "skills injected into the working set"
            );
            if let Some(t) = self.turn.lock().await.as_mut() {
                t.skills_used = slugs.clone();
            }
            if let Some(host) = self.sync.host(&chat_id).await {
                let mut h = host.lock().await;
                h.doc.add_hidden_part(
                    &msg_id,
                    "skills",
                    &text,
                    &json!({ "slugs": slugs, "scores": selected.iter().map(|s| s.score).collect::<Vec<_>>() }),
                );
            }
            let _ = self.store.t3_append(
                &chat_id,
                None,
                Some(&chat_id),
                "skills",
                Some("system"),
                &format!("skills injected: {}", slugs.join(", ")),
                None,
            );
        }
        text
    }

    // ---- subagents ----------------------------------------------------------------------------

    /// Context pack v0 (design §6.7 sections 1–4, 9, 10).
    fn build_pack(&self, name: &str, purpose: &str, brief: &str, data_heavy: bool) -> String {
        let mut s = String::new();
        s.push_str(&format!("# Task: {name}\n\n{purpose}\n\n{brief}\n\n"));
        s.push_str(&format!(
            "## Project folder\n\n`{}` (your working directory). Keep every file of this project there; other folders under `/work` belong to other projects.\n\n",
            self.cfg.work_dir.display()
        ));
        if !self.cfg.instructions.trim().is_empty() {
            s.push_str(&format!(
                "## Project instructions\n\n{}\n\n",
                self.cfg.instructions.trim()
            ));
        }
        let core = self.memory.core_memory_text();
        if !core.trim().is_empty() {
            s.push_str("## Project memory (T0 + T1)\n\n");
            s.push_str(&core);
            s.push('\n');
        }
        if data_heavy {
            s.push_str("## Data handling\n\nInputs are large. Keep data in variables or files and print slices; never paste whole files into the conversation.\n\n");
        }
        // Section 11 (F5): skills matching the brief, same budget as the main agent's section.
        let (selected, skills_text) = skills::section_for(
            &format!("{name} {purpose} {brief}"),
            &self.skills.snapshot(),
            skills::SECTION_BUDGET_TOKENS,
        );
        if !skills_text.is_empty() {
            tracing::info!(subagent = name, skills = ?selected.iter().map(|x| x.slug.as_str()).collect::<Vec<_>>(), "skills injected into the pack");
            s.push_str(&skills_text);
            s.push_str("\n\n");
        }
        s.push_str("## Tools\n\nMCP server `arbos`: history_search, history_read, read_thread, memory_search, memory_read (you may search and read the entire history of this Project), peek, sub_llm, remember/recall, projects_list, ask_user (optional widget: choice, multi_select, text, date, form), skills_list/skills_read/skills_save (reusable how-tos), draft_message (an outbound email or chat message shown to the user as an approval card; nothing is sent without their click), connectors_list/connector_call (the user's Gmail, Calendar, Slack, GitHub, Harness Comms through the relay; write/send actions need the user's approval), local_devices/local_run/local_read/local_list (the user's own computer through the arbos-local helper; every command and, unless the user allowed reads, every file read needs the user's approval), deliver_file (hand a file you made to the user as a card), generate_image (an image from a prompt, delivered as a card), vault_list/vault_request/egress/browser_fill (the user's credentials: you only ever get a placeholder such as `arbos-secret://name/value` or `$ARBOS_SECRET_NAME`; send it through `egress`, never try to read or print the value; logins are typed by browser_fill).\n\n");
        s.push_str(&crate::media::prompt_section(&self.cfg));
        s.push_str("## Return contract\n\nEnd your final reply with one fenced ```json block:\n{ \"status\": \"done\" | \"blocked\" | \"partial\" | \"needs_input\", \"summary\": \"<= 5 lines\", \"artifacts\": [ { \"kind\": \"file\" | \"commit\" | \"url\" | \"command\", \"ref\": \"…\", \"note\": \"…\" } ], \"acceptance\": [ { \"criterion\": \"…\", \"met\": true, \"evidence\": \"…\" } ], \"open_items\": [], \"proposed_memory\": [ { \"kind\": \"decision\", \"scope\": \"project\", \"text\": \"…\" } ], \"questions\": [] }\n");
        s
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run_subagent(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        in_reply_to: &str,
        existing: Option<String>,
        name: &str,
        purpose: &str,
        brief: &str,
        route_harness: Option<Harness>,
        data_heavy: bool,
        escalated: bool,
    ) -> Result<()> {
        let default_harness = std::env::var("ARBOS_DEFAULT_HARNESS")
            .ok()
            .and_then(|s| match s.as_str() {
                "pi" => Some(Harness::Pi),
                "claude_code" => Some(Harness::ClaudeCode),
                "prime" => Some(Harness::Prime),
                _ => None,
            })
            .unwrap_or(Harness::Prime);
        let (_, subagent_model) = self.live_models().await;
        let (subagent_id, harness, is_new) = match existing {
            Some(id) => {
                let row = match self.store.subagent(&id)? {
                    Some(r) => r,
                    None => self
                        .store
                        .subagent_by_name(&id)?
                        .ok_or_else(|| anyhow!("unknown subagent {id}"))?,
                };
                let h = match row.harness.as_str() {
                    "pi" => Harness::Pi,
                    "claude_code" => Harness::ClaudeCode,
                    _ => Harness::Prime,
                };
                (row.id, h, false)
            }
            None => {
                let h = pick_harness(
                    route_harness,
                    data_heavy,
                    brief,
                    &subagent_model,
                    default_harness,
                );
                let id = ulid();
                self.store
                    .insert_subagent(&id, name, purpose, h.as_str(), chat_id)?;
                (id, h, true)
            }
        };
        let adapter = self.adapter(harness);
        let sub_name = self
            .store
            .subagent(&subagent_id)?
            .map(|s| s.name)
            .unwrap_or_else(|| name.to_owned());

        // The progress message carries status events and hidden parts only; the main agent writes
        // the prose ack before this and the prose result after (demo review items 1–2).
        let msg_id = ulid();
        {
            let mut h = host.lock().await;
            h.doc
                .start_assistant_message(&msg_id, in_reply_to, &self.cfg.device_id);
            h.doc.set_message_fields(
                &msg_id,
                &[
                    ("subagentId", json!(subagent_id)),
                    ("subagentName", json!(sub_name)),
                    (
                        "turn",
                        json!({ "kind": if is_new { "spawn" } else { "wake" } }),
                    ),
                    ("escalated_from_act", json!(escalated)),
                    ("model", json!(subagent_model)),
                ],
            );
        }
        let queued_at = Instant::now();
        if self.subagent_slots.available_permits() == 0 {
            let waiting = self
                .store
                .subagents()
                .unwrap_or_default()
                .iter()
                .filter(|s| s.status == "queued")
                .count();
            let _ = self.store.set_subagent_status(&subagent_id, "queued");
            let _ = self
                .store
                .set_subagent_last_status(&subagent_id, &format!("queued ({waiting} ahead)"));
            let mut h = host.lock().await;
            h.doc
                .add_status_part(&msg_id, "working", "Waiting for a free slot…");
        }
        self.write_threads_index().await;
        let _slot = self
            .subagent_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("subagent slots closed"))?;
        let _box_slot = self
            .box_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("box subagent slots closed"))?;
        let queued_ms = queued_at.elapsed().as_millis() as i64;
        if queued_ms > 500 {
            let _ = self
                .post_timing("subagent_queued_ms", queued_ms, Some(harness.as_str()))
                .await;
        }
        {
            let mut h = host.lock().await;
            h.doc.add_status_part(&msg_id, "working", "Working…");
        }
        self.subagent_msgs
            .lock()
            .await
            .insert(subagent_id.clone(), (chat_id.to_owned(), msg_id.clone()));
        let _ = self.store.set_subagent_last_status(&subagent_id, "working");
        let t0 = Instant::now();
        let pack = self.build_pack(&sub_name, purpose, brief, data_heavy);
        std::fs::create_dir_all(self.cfg.state_dir.join("packs"))?;
        std::fs::write(
            self.cfg
                .state_dir
                .join("packs")
                .join(format!("{subagent_id}.md")),
            &pack,
        )?;
        self.store.set_subagent_status(&subagent_id, "running")?;
        self.write_threads_index().await;
        let _ = self.store.t3_append(
            &subagent_id,
            Some(&subagent_id),
            Some(&subagent_id),
            "user",
            Some("user"),
            brief,
            Some(&json!({ "chat_id": chat_id, "pack": "pack-v0" })),
        );

        let mut resumed_fresh = false;
        if is_new {
            adapter
                .start(StartRequest {
                    session_id: subagent_id.clone(),
                    cwd: self.cfg.work_dir.clone(),
                    system_prompt: pack.clone(),
                    model: subagent_model.clone(),
                    thinking: "medium".into(),
                })
                .await?;
            let _ = self
                .post_timing(
                    "subagent_start_ms",
                    t0.elapsed().as_millis() as i64,
                    Some(harness.as_str()),
                )
                .await;
        } else if !adapter.is_open(&subagent_id) {
            match adapter.resume(&subagent_id).await {
                Ok(()) => {
                    let _ = self
                        .post_timing(
                            "subagent_resume_ms",
                            t0.elapsed().as_millis() as i64,
                            Some(harness.as_str()),
                        )
                        .await;
                }
                Err(e) => {
                    // A session that cannot come back (moved box, corrupt store, harness upgrade)
                    // must not sink the hand-off: start the same subagent fresh with its summary.
                    tracing::warn!(subagent = %subagent_id, error = %e, "resume failed; starting the subagent fresh");
                    resumed_fresh = true;
                    adapter
                        .start(StartRequest {
                            session_id: subagent_id.clone(),
                            cwd: self.cfg.work_dir.clone(),
                            system_prompt: pack.clone(),
                            model: subagent_model.clone(),
                            thinking: "medium".into(),
                        })
                        .await
                        .with_context(|| format!("resume failed ({e}); fresh start failed too"))?;
                }
            }
        }
        let stream = adapter
            .stream(&subagent_id)
            .ok_or_else(|| anyhow!("no stream for {subagent_id}"))?;
        let first_prompt = if is_new {
            brief.to_owned()
        } else if resumed_fresh {
            let summary = self
                .store
                .subagent(&subagent_id)
                .ok()
                .flatten()
                .map(|r| r.summary)
                .unwrap_or_default();
            format!(
                "Your previous session could not be resumed, so this is a fresh session for the same thread. What you had done before: {}\n\nContinue:\n\n{brief}",
                if summary.trim().is_empty() { "(no summary recorded)" } else { summary.trim() }
            )
        } else {
            format!("Context update: continue your thread.\n\n{brief}")
        };
        let mut stream = match adapter.prompt(&subagent_id, &first_prompt).await {
            Ok(()) => stream,
            Err(e) if !is_new && !resumed_fresh => {
                // The resumed child died on first contact (broken pipe): same fallback as above.
                tracing::warn!(subagent = %subagent_id, error = %e, "resumed session failed on prompt; starting fresh");
                let _ = adapter.stop(&subagent_id).await;
                adapter
                    .start(StartRequest {
                        session_id: subagent_id.clone(),
                        cwd: self.cfg.work_dir.clone(),
                        system_prompt: pack.clone(),
                        model: subagent_model.clone(),
                        thinking: "medium".into(),
                    })
                    .await
                    .with_context(|| format!("prompt failed ({e}); fresh start failed too"))?;
                let fresh = adapter
                    .stream(&subagent_id)
                    .ok_or_else(|| anyhow!("no stream for {subagent_id}"))?;
                let summary = self
                    .store
                    .subagent(&subagent_id)
                    .ok()
                    .flatten()
                    .map(|r| r.summary)
                    .unwrap_or_default();
                adapter
                    .prompt(
                        &subagent_id,
                        &format!(
                            "Your previous session could not be resumed, so this is a fresh session for the same thread. What you had done before: {}\n\nContinue:\n\n{brief}",
                            if summary.trim().is_empty() { "(no summary recorded)" } else { summary.trim() }
                        ),
                    )
                    .await?;
                fresh
            }
            Err(e) => return Err(e),
        };

        self.subagent_turns.fetch_add(1, Ordering::SeqCst);
        let keepalive = self.spawn_keepalive();
        let mut text = String::new();
        let mut sub_tool_inputs: HashMap<String, Value> = HashMap::new();
        let mut usage_total = json!({});
        let mut first_delta: Option<i64> = None;
        let mut done_err: Option<String> = None;
        let deadline = Instant::now() + Duration::from_secs(60 * 60);
        loop {
            let ev = tokio::select! {
                e = stream.recv() => e,
                _ = tokio::time::sleep_until(deadline.into()) => { done_err = Some("subagent exceeded 60 minutes".into()); break; }
            };
            let ev = match ev {
                Ok(e) => e,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            };
            match ev {
                AgentEvent::TextDelta(d) => {
                    // Raw subagent prose never renders (item 1): kept for the hidden result part.
                    if first_delta.is_none() {
                        first_delta = Some(t0.elapsed().as_millis() as i64);
                    }
                    text.push_str(&d);
                }
                AgentEvent::ReasoningDelta(_) => {}
                AgentEvent::ToolCall { id, name, input } => {
                    sub_tool_inputs.insert(id, input.clone());
                    let label = step_label(&name, &input);
                    let _ = self
                        .store
                        .set_subagent_last_status(&subagent_id, &format!("step: {label}"));
                    let _ = self.store.t3_append(
                        &subagent_id,
                        Some(&subagent_id),
                        Some(&subagent_id),
                        "tool_call",
                        Some("assistant"),
                        &format!("{name} {input}"),
                        None,
                    );
                }
                AgentEvent::ToolResult {
                    id,
                    name,
                    output,
                    is_error,
                } => {
                    let output = crate::store::redact(&output);
                    let _ = self.store.t3_append(
                        &subagent_id,
                        Some(&subagent_id),
                        Some(&subagent_id),
                        "tool_output",
                        Some("tool"),
                        &format!("{name}: {output}"),
                        None,
                    );
                    let summary: String = output.chars().take(TOOL_OUTPUT_SUMMARY_MAX).collect();
                    let output_ref = if output.len() > TOOL_OUTPUT_SUMMARY_MAX {
                        Some(self.upload_blob(output.clone().into_bytes(), "text/plain"))
                    } else {
                        None
                    };
                    let input = sub_tool_inputs.remove(&id).unwrap_or(json!({}));
                    let mut h = host.lock().await;
                    h.doc.add_tool_part(
                        &msg_id,
                        &id,
                        &name,
                        &input,
                        &summary,
                        output_ref.as_deref(),
                        Some(output.len() as i64),
                        is_error,
                    );
                }
                AgentEvent::Usage {
                    input,
                    output,
                    cache_read,
                    cache_write,
                    cost_usd,
                } => {
                    usage_total = json!({ "input": input, "output": output, "cacheRead": cache_read, "cacheWrite": cache_write, "cost": cost_usd });
                }
                AgentEvent::InputRequested { question } => {
                    let qid = ulid();
                    let _ = self.store.insert_ask(
                        &qid,
                        Some(chat_id),
                        Some(&subagent_id),
                        &question,
                        None,
                    );
                    let mut h = host.lock().await;
                    h.doc.add_question_part(&msg_id, &qid, &question, None);
                    h.doc.add_status_part(&msg_id, "waiting_user", &question);
                }
                AgentEvent::CompactionStarted => {}
                AgentEvent::CompactionEnded { summary } => {
                    let n = self
                        .store
                        .bump_session_compactions(&subagent_id)
                        .unwrap_or(0);
                    let _ = self.store.t3_append(
                        &subagent_id,
                        Some(&subagent_id),
                        Some(&subagent_id),
                        "compaction",
                        Some("system"),
                        &format!(
                            "session compacted ({n}); summary {} chars",
                            summary.as_deref().map(str::len).unwrap_or(0)
                        ),
                        None,
                    );
                }
                AgentEvent::Done { aborted, error } => {
                    if aborted || error.is_some() {
                        done_err = error.or(Some("aborted".into()));
                    }
                    break;
                }
            }
        }
        keepalive.abort();
        self.subagent_turns.fetch_sub(1, Ordering::SeqCst);
        let total_ms = t0.elapsed().as_millis() as i64;
        let contract = extract_return_contract(&text);
        let status = contract
            .as_ref()
            .and_then(|c| c.get("status"))
            .and_then(Value::as_str)
            .unwrap_or(if done_err.is_some() {
                "blocked"
            } else {
                "done"
            })
            .to_owned();
        let failed = done_err.is_some() || status == "blocked";
        {
            let mut h = host.lock().await;
            if let Some(e) = &done_err {
                h.doc.add_error_part(&msg_id, "subagent_error", e);
            }
            // Raw output + contract: Details drawer only (`hidden: true`).
            h.doc.add_hidden_part(
                &msg_id,
                "subagent_result",
                &text,
                &json!({
                    "subagentId": subagent_id,
                    "status": status,
                    "returnContract": contract.clone().unwrap_or(Value::Null),
                    "durationMs": total_ms,
                }),
            );
            h.doc.add_status_part(
                &msg_id,
                if failed { "failed" } else { "done" },
                if failed { "Failed" } else { "Done" },
            );
            h.doc.set_message_fields(
                &msg_id,
                &[
                    ("usage", usage_total.clone()),
                    ("returnContract", contract.clone().unwrap_or(Value::Null)),
                ],
            );
            h.doc.finish_message(
                &msg_id,
                if done_err.is_some() {
                    MessageStatus::Error
                } else {
                    MessageStatus::Done
                },
            );
        }
        self.subagent_msgs.lock().await.remove(&subagent_id);
        let _ = self
            .store
            .set_subagent_last_status(&subagent_id, if failed { "failed" } else { "done" });
        let _ = self.store.t3_append(
            &subagent_id,
            Some(&subagent_id),
            Some(&subagent_id),
            "assistant",
            Some("assistant"),
            &text,
            Some(&json!({ "chat_id": chat_id, "message_id": msg_id })),
        );
        let new_status = match status.as_str() {
            "needs_input" => "waiting_user",
            _ => "idle",
        };
        self.store.set_subagent_status(&subagent_id, new_status)?;
        if let Some(c) = &contract {
            if let Some(s) = c.get("summary").and_then(Value::as_str) {
                let _ = self.store.set_subagent_summary(&subagent_id, s, &json!([]));
            }
            if let Some(qs) = c.get("questions").and_then(Value::as_array) {
                for q in qs.iter().filter_map(Value::as_str) {
                    let _ =
                        self.store
                            .insert_ask(&ulid(), Some(chat_id), Some(&subagent_id), q, None);
                }
            }
        }
        let _ = self
            .post_timing("subagent_turn_total_ms", total_ms, Some(harness.as_str()))
            .await;
        if let Some(fd) = first_delta {
            let _ = self
                .post_timing("subagent_ttft_ms", fd, Some(harness.as_str()))
                .await;
        }
        self.write_threads_index().await;
        // T3 index of the subagent's own session file (history_search source) + write pass, off path.
        {
            let me = self.clone();
            let sid = subagent_id.clone();
            let proposals = contract
                .as_ref()
                .and_then(|c| c.get("proposed_memory"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let summary_text = contract
                .as_ref()
                .and_then(|c| c.get("summary"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let text2 = text.clone();
            let chat = chat_id.to_owned();
            tokio::spawn(async move {
                me.index_session_history(&sid, harness).await;
                let body = if summary_text.is_empty() {
                    text2
                } else {
                    summary_text
                };
                let _ = me
                    .memory
                    .write_pass(
                        &format!("subagent:{sid}"),
                        &sid,
                        "subagent",
                        &body,
                        &proposals,
                    )
                    .await;
                let _ = me.memory.maybe_compact(&chat).await;
                let _ = me.write_system_prompt();
            });
        }
        drop(_box_slot);
        drop(_slot);
        // The user hears about it from the main agent, in prose, in this chat.
        let result_text = match &contract {
            Some(c) => format!("{text}\n\nParsed return contract:\n{c}"),
            None => text.clone(),
        };
        self.run_result_turn(
            chat_id,
            host,
            in_reply_to,
            &subagent_id,
            &sub_name,
            &status,
            total_ms,
            &result_text,
            done_err.as_deref(),
        )
        .await
    }

    /// Tail the subagent's session JSONL into T3 (pre-compaction entries included).
    pub async fn index_session_history(&self, session_id: &str, harness: Harness) {
        let adapter = self.adapter(harness);
        let cursor = self.store.session_cursor(session_id).ok().flatten();
        match adapter.read_history(session_id, cursor).await {
            Ok((entries, next)) => {
                for e in &entries {
                    if let Some((role, text)) = crate::harness::entry_text(e) {
                        let _ = self.store.t3_append(
                            session_id,
                            Some(session_id),
                            Some(session_id),
                            "session_entry",
                            Some(&role),
                            &text,
                            Some(&json!({ "entry_id": e.get("id") })),
                        );
                    }
                }
                if let Some(c) = next {
                    let _ = self.store.set_session_cursor(session_id, &c);
                }
            }
            Err(e) => tracing::debug!(error = %e, session_id, "history index"),
        }
    }

    /// `threads` (T1 index) + `status` in the project doc.
    pub async fn write_threads_index(&self) {
        let Some(h) = self.sync.host(arbos_core::wire::PROJECT_DOC_CHAT_ID).await else {
            return;
        };
        let h = h.lock().await;
        let threads = h.doc.doc.get_map("threads");
        for s in self.store.subagents().unwrap_or_default() {
            let v = json!({ "name": s.name, "purpose": s.purpose, "summary": s.summary, "tags": s.tags, "status": s.status, "lastTurnAt": s.last_turn_at, "harness": s.harness });
            let lv: loro::LoroValue = serde_json::from_value(v).unwrap_or(loro::LoroValue::Null);
            threads.insert(&s.id, lv).ok();
        }
        h.doc.doc.commit();
    }

    /// MCP `spawn_subagent` / `wake_subagent` from inside an act turn (D66 escalation).
    pub async fn spawn_from_tool(
        self: &Arc<Self>,
        existing: Option<String>,
        name: &str,
        purpose: &str,
        brief: &str,
        isolate: bool,
    ) -> Result<String> {
        let _ = isolate;
        let (chat_id, user_msg) = {
            let t = self.turn.lock().await;
            match t.as_ref() {
                Some(t) => (t.chat_id.clone(), t.user_msg_id.clone()),
                None => (
                    self.store
                        .chats()?
                        .first()
                        .map(|c| c.id.clone())
                        .unwrap_or_default(),
                    String::new(),
                ),
            }
        };
        let host = self
            .sync
            .host(&chat_id)
            .await
            .ok_or_else(|| anyhow!("chat room not open"))?;
        if let Some(t) = self.turn.lock().await.as_mut() {
            t.escalate = false;
            let mut h = host.lock().await;
            h.doc
                .add_status_part(&t.assistant_msg_id, "working", "Working…");
        }
        let me = self.clone();
        let (n, p, b) = (name.to_owned(), purpose.to_owned(), brief.to_owned());
        let id_holder = Arc::new(Mutex::new(String::new()));
        let idh = id_holder.clone();
        tokio::spawn(async move {
            let _ = idh;
            if let Err(e) = me
                .run_subagent(
                    &chat_id, &host, &user_msg, existing, &n, &p, &b, None, false, true,
                )
                .await
            {
                tracing::warn!(error = %e, "spawn_from_tool");
            }
        });
        Ok("subagent started; its result will be posted in this chat".to_owned())
    }

    /// Stop every child in order (subagents, then pi) so session files and kernel snapshots land.
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.pi_ready.store(false, Ordering::SeqCst);
        for a in self.adapters.values() {
            a.shutdown_all().await;
        }
        if let Some(pi) = self.pi.lock().await.take() {
            pi.shutdown().await;
        }
    }

    /// True while the main agent or any subagent is mid-turn.
    pub fn any_turn_running(&self) -> bool {
        self.turn_running.load(Ordering::SeqCst) || self.subagent_turns.load(Ordering::SeqCst) > 0
    }

    pub fn is_healthy(&self) -> bool {
        self.pi_ready.load(Ordering::SeqCst)
    }
}

/// Short label for a `step` status event: the command head for `bash`, the path for file tools,
/// otherwise the tool name and its first string argument. One line, ≤ 72 chars.
pub fn step_label(tool_name: &str, args: &Value) -> String {
    let first_str = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| args.get(k).and_then(Value::as_str))
            .or_else(|| {
                args.as_object()
                    .and_then(|o| o.values().find_map(Value::as_str))
            })
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
    };
    let body = match tool_name {
        "bash" | "shell" | "execute" => first_str(&["command", "cmd"]),
        "read" | "write" | "edit" | "read_file" | "write_file" | "edit_file" | "glob" | "grep" => {
            first_str(&["path", "file_path", "pattern"])
        }
        _ => first_str(&["query", "url", "name", "code"]),
    }
    .unwrap_or_default();
    let label = if body.is_empty() {
        tool_name.to_owned()
    } else {
        format!("{tool_name} {body}")
    };
    let mut out: String = label.chars().take(72).collect();
    if out.chars().count() < label.chars().count() {
        out.push('…');
    }
    out
}

pub fn route_name(r: &ValidRoute) -> &'static str {
    match r {
        ValidRoute::Reply { .. } => "reply",
        ValidRoute::Act { .. } => "act",
        ValidRoute::Wake { .. } => "wake",
        ValidRoute::New { .. } => "new",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Routing rule: the inline `reply` streams out of partial JSON exactly as written.
    #[test]
    fn reply_extractor_streams_partial_json() {
        let mut ex = ReplyExtractor::default();
        let mut out = String::new();
        for d in [
            "{\"action\": \"reply",
            "\", \"reply\": \"Hi ",
            "there\\n\\\"you\\\"",
            " — ok\"}",
        ] {
            out.push_str(&ex.feed(d));
        }
        assert_eq!(out, "Hi there\n\"you\" — ok");
        let mut ex2 = ReplyExtractor::default();
        assert_eq!(
            ex2.feed("{\"action\":\"act\",\"plan\":\"x\",\"reply\":null}"),
            ""
        );
    }
}
