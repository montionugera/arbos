//! Shared relay state: db, config, per-room broadcast channels, box providers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::{broadcast, Mutex, RwLock};

use arbos_core::wire::BoxState;

use crate::daytona::Daytona;
use crate::db::Db;

#[derive(Clone)]
pub struct Config {
    pub public_url: String,
    pub data_dir: PathBuf,
    pub admin_token: Option<String>,
    pub openrouter_key: Option<String>,
    pub anthropic_key: Option<String>,
    pub openai_key: Option<String>,
    pub openai_base: String,
    /// Upstream for `/proxy/v1/images/generations` (OpenAI's own API; OpenRouter has no images endpoint).
    pub openai_images_base: String,
    pub anthropic_base: String,
    pub daytona_snapshot: String,
    /// Inline Dockerfile for Daytona's declarative builder when the snapshot is not registered.
    pub daytona_dockerfile: Option<PathBuf>,
    /// Directory served at `/bootstrap/{file}` to boxes (arbos-agentd binary, arbos-pi-ext.tar).
    pub bootstrap_dir: Option<PathBuf>,
    pub agentd_bin: PathBuf,
    pub local_box_image: String,
    pub box_idle_grace_ms: i64,
    /// `ARBOS_CONNECTORS_MOCK=1`: connectors answer with canned data (F1).
    pub connectors_mock: bool,
    /// `ARBOS_CHANNELS_MOCK=1`: channel sends land in `channel_outbox`; `/channels/mock/inbound` is open (F7).
    pub channels_mock: bool,
    /// `ARBOS_IMAGE_MOCK=1`: image generation answers with a labelled placeholder PNG (F10).
    pub image_mock: bool,
    /// `ARBOS_VOICE_MOCK=1`: `/proxy/v1/audio/*` answer locally (F11).
    pub voice_mock: bool,
    /// OpenAI-compatible base for audio (STT/TTS); api.openai.com by default
    /// because OpenRouter carries no audio endpoints.
    pub openai_audio_base: String,
    /// `ARBOS_VAULT_MOCK=1`: two demo vault items and a local echo target (F8).
    pub vault_mock: bool,
    /// How devices sign in: GitHub OAuth (self-serve) or one-time pairing codes (self-hosters).
    pub auth_mode: AuthMode,
    pub github_client_id: Option<String>,
    pub github_client_secret: Option<String>,
    /// Extra allowed `redirect` prefixes for `/auth/github/start` (`ARBOS_AUTH_REDIRECTS`).
    pub auth_redirects: Vec<String>,
    /// Credits a new GitHub account starts with (`ARBOS_DEFAULT_GRANT_USD`).
    pub default_grant_usd: f64,
    /// Soft per-user daily debit cap for new accounts (`ARBOS_DEFAULT_DAILY_CAP_USD`).
    pub default_daily_cap_usd: f64,
    /// Relay-wide daily inference debit cap; reaching it trips the kill switch (`ARBOS_GLOBAL_DAILY_CAP_USD`).
    pub global_daily_cap_usd: f64,
    /// Relay-wide daily machine cost that only logs a warning (`ARBOS_GLOBAL_MACHINE_DAILY_CAP_USD`).
    pub global_machine_daily_cap_usd: f64,
    /// Machines always on (`ARBOS_ALWAYS_ON`, default on in GitHub mode): no idle auto-stop,
    /// a box per signed-in user from sign-in on, re-woken after a relay restart.
    pub always_on: bool,
    /// Our Daytona cost for a running box (`ARBOS_MACHINE_USD_PER_HOUR`).
    pub machine_usd_per_hour: f64,
    /// Our Daytona cost for a stopped box's disk (`ARBOS_MACHINE_STOPPED_USD_PER_DAY`).
    pub machine_stopped_usd_per_day: f64,
    /// `owner/repo` whose `edge` release feeds `GET /download/mac` (`ARBOS_RELEASES_REPO`).
    pub releases_repo: String,
    /// Tree avatar slugs in manifest order (`ARBOS_AVATAR_MANIFEST` / `ARBOS_AVATAR_SLUGS`).
    pub avatar_slugs: Vec<String>,
    /// Avatar colour palette in manifest order (`colors[]` / `ARBOS_AVATAR_COLORS`).
    pub avatar_colors: Vec<String>,
    /// Public site that fronts invite links (`ARBOS_SITE_URL`, default `https://arbos.life`):
    /// `<site>/i/<code>` is served by the relay's `GET /i/{code}`.
    pub site_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Github,
    Paircode,
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthMode::Github => "github",
            AuthMode::Paircode => "paircode",
        }
    }
}

/// One frame to fan out to the other sockets in a room.
#[derive(Clone, Debug)]
pub struct RoomBroadcast {
    pub from_conn: u64,
    pub bytes: Bytes,
}

pub struct Room {
    pub tx: broadcast::Sender<RoomBroadcast>,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub cfg: Arc<Config>,
    pub http: reqwest::Client,
    pub daytona: Option<Daytona>,
    pub rooms: Arc<RwLock<HashMap<String, Arc<Room>>>>,
    /// user_id → live state of the user's box as the relay believes it (STATUS frames, crdt-sync §8).
    pub box_states: Arc<RwLock<HashMap<String, (BoxState, i64)>>>,
    /// user_id → local child process (process provider only).
    pub local_children: Arc<Mutex<HashMap<String, tokio::process::Child>>>,
    /// user_id → (open box sockets, last disconnect ms).
    pub box_conns: Arc<Mutex<HashMap<String, (u32, i64)>>>,
    /// user_id → ms when the current wake started (for wake timing).
    pub wake_started: Arc<Mutex<HashMap<String, i64>>>,
    /// user_id → ms of the last client row; rows pushed while a box was dying must re-wake it.
    pub last_client_push: Arc<Mutex<HashMap<String, i64>>>,
    /// user_id → wakes that produced no hello within the deadline (stale-box detector).
    pub stale_bounces: Arc<Mutex<HashMap<String, u32>>>,
    pub conn_seq: Arc<std::sync::atomic::AtomicU64>,
    /// Encrypts connector tokens and vault items at rest (F1, F8).
    pub relay_key: Arc<crate::connectors::crypto::RelayKey>,
    /// device_id → live `arbos-local` helper socket (F9).
    pub local_conns: crate::local::LocalConns,
    /// HS256 key for access tokens (`ARBOS_JWT_KEY`, else derived from the relay key).
    pub jwt_key: Arc<[u8; 32]>,
    /// `(fetched_at_ms, dmg_url)` for `GET /download/mac`.
    pub download_cache: Arc<Mutex<Option<(i64, String)>>>,
    /// user_id → box upgrade in progress (`POST /machine/upgrade`).
    pub upgrading: Arc<Mutex<HashMap<String, UpgradeStatus>>>,
    /// Message notifications transport (APNs, a capture list in tests, or off).
    pub push: Arc<crate::apns::Pusher>,
    /// device_id → the chat its presence says is on screen (suppresses that chat's pushes).
    pub focus: crate::notify::Focus,
    /// (chat_id, sender user) → when the row-level fallback last notified for it.
    pub fallback_sent: Arc<Mutex<HashMap<(String, String), i64>>>,
}

/// One running box upgrade.
#[derive(Debug, Clone)]
pub struct UpgradeStatus {
    pub phase: String,
    pub since: i64,
    pub from_snapshot: Option<String>,
    pub to_snapshot: String,
}

impl AppState {
    pub async fn room(&self, name: &str) -> Arc<Room> {
        if let Some(r) = self.rooms.read().await.get(name) {
            return r.clone();
        }
        let mut w = self.rooms.write().await;
        w.entry(name.to_owned())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(1024);
                Arc::new(Room { tx })
            })
            .clone()
    }

    /// The user whose box serves a project.
    pub fn user_of(&self, project_id: &str) -> Option<String> {
        self.db
            .project(project_id)
            .ok()
            .flatten()
            .map(|p| p.user_id)
    }

    /// May this principal touch this project (by id or name)? Unknown projects: no.
    pub fn allows(&self, principal: &crate::auth::Principal, project_id: &str) -> bool {
        if let Some(chat_id) = project_id.strip_prefix(crate::people::PEOPLE_ACL_PREFIX) {
            return crate::people::room_access(self, principal, chat_id);
        }
        match self.user_of(project_id) {
            Some(u) if principal.can_access_project(&u) => true,
            // Projects together (D72 step 3): a person the owner invited drives the project too.
            Some(_) => match principal {
                crate::auth::Principal::Device { user_id, .. } => {
                    let id = self
                        .db
                        .project(project_id)
                        .ok()
                        .flatten()
                        .map(|p| p.id)
                        .unwrap_or_else(|| project_id.to_owned());
                    self.db.is_project_member(&id, user_id).unwrap_or(false)
                }
                crate::auth::Principal::Box { .. } | crate::auth::Principal::Admin => false,
            },
            None => false,
        }
    }

    /// Owner-level access: the project's own user (any of their devices, their box) or the admin;
    /// guests of a shared project are not owners.
    pub fn owns(&self, principal: &crate::auth::Principal, project_id: &str) -> bool {
        match self.user_of(project_id) {
            Some(u) => principal.can_access_project(&u),
            None => false,
        }
    }

    pub async fn box_state(&self, user_id: &str) -> (BoxState, i64) {
        self.box_states
            .read()
            .await
            .get(user_id)
            .copied()
            .unwrap_or((BoxState::Asleep, 0))
    }

    /// Box state seen from one project (its user's box).
    pub async fn project_box_state(&self, project_id: &str) -> (BoxState, i64) {
        match self.user_of(project_id) {
            Some(u) => self.box_state(&u).await,
            None => (BoxState::Asleep, 0),
        }
    }

    pub async fn set_box_state(&self, user_id: &str, st: BoxState, detail: Option<&str>) {
        let now = arbos_core::ids::now_ms();
        let changed = {
            let mut w = self.box_states.write().await;
            let prev = w.get(user_id).map(|x| x.0);
            w.insert(user_id.to_owned(), (st, now));
            prev != Some(st)
        };
        if st == BoxState::Awake {
            self.wake_started.lock().await.remove(user_id);
        }
        let arbos = match st {
            BoxState::Awake => "awake",
            BoxState::Waking => "waking",
            BoxState::Asleep => "hibernated_light",
            BoxState::Error => "error",
        };
        // The project state column mirrors the user's box for every project of that user.
        for p in self.db.projects_of_user(user_id).unwrap_or_default() {
            let _ = self.db.set_project_state(&p.id, arbos);
            if st != BoxState::Awake {
                let _ = self.db.set_workspace_state(&p.id, "stopped");
            }
        }
        if changed {
            self.broadcast_status(user_id, st, detail).await;
        }
    }

    /// `STATUS` frame to every room of every project of the user (crdt-sync §7.4, 0x0c).
    pub async fn broadcast_status(&self, user_id: &str, st: BoxState, detail: Option<&str>) {
        let frame = arbos_core::wire::Frame::new(
            arbos_core::wire::FrameType::Status,
            serde_json::to_value(arbos_core::wire::StatusHeader {
                box_state: st,
                since: arbos_core::ids::now_ms(),
                detail: detail.map(str::to_owned),
            })
            .expect("status header"),
            Bytes::new(),
        )
        .encode();
        let prefixes: Vec<String> = self
            .db
            .projects_of_user(user_id)
            .unwrap_or_default()
            .into_iter()
            .map(|p| format!("{}/", p.id))
            .collect();
        let rooms = self.rooms.read().await;
        for (name, room) in rooms.iter() {
            if prefixes.iter().any(|p| name.starts_with(p)) {
                let _ = room.tx.send(RoomBroadcast {
                    from_conn: 0,
                    bytes: frame.clone(),
                });
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! One relay state over a fresh temp SQLite file, for unit tests across modules.

    use std::collections::HashMap;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    use tokio::sync::{Mutex, RwLock};

    use arbos_core::ids::ulid;

    use super::{AppState, AuthMode, Config};
    use crate::connectors::crypto::RelayKey;
    use crate::db::Db;

    pub fn test_state(auth_mode: AuthMode) -> AppState {
        let dir = std::env::temp_dir().join(format!("arbos-credits-test-{}", ulid()));
        let db = Db::open(&dir.join("relay.db")).expect("db");
        let cfg = Config {
            public_url: "http://127.0.0.1:7707".into(),
            data_dir: dir,
            admin_token: Some("adm_test".into()),
            openrouter_key: None,
            anthropic_key: None,
            openai_key: None,
            openai_base: String::new(),
            anthropic_base: String::new(),
            daytona_snapshot: String::new(),
            daytona_dockerfile: None,
            bootstrap_dir: None,
            agentd_bin: "arbos-agentd".into(),
            local_box_image: String::new(),
            box_idle_grace_ms: 0,
            connectors_mock: true,
            channels_mock: true,
            openai_images_base: String::new(),
            image_mock: false,
            voice_mock: true,
            openai_audio_base: String::new(),
            vault_mock: false,
            auth_mode,
            github_client_id: Some("id".into()),
            github_client_secret: Some("secret".into()),
            auth_redirects: Vec::new(),
            default_grant_usd: 100.0,
            default_daily_cap_usd: 20.0,
            global_daily_cap_usd: 500.0,
            global_machine_daily_cap_usd: 400.0,
            always_on: false,
            machine_usd_per_hour: 0.332,
            machine_stopped_usd_per_day: 0.013,
            releases_repo: "unarbos/arbos".into(),
            site_url: "https://arbos.life".into(),
            avatar_slugs: crate::avatar::DEFAULT_SLUGS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            avatar_colors: crate::avatar::DEFAULT_COLORS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        };
        AppState {
            db,
            cfg: Arc::new(cfg),
            http: reqwest::Client::new(),
            daytona: None,
            rooms: Arc::new(RwLock::new(HashMap::new())),
            box_states: Arc::new(RwLock::new(HashMap::new())),
            local_children: Arc::new(Mutex::new(HashMap::new())),
            box_conns: Arc::new(Mutex::new(HashMap::new())),
            wake_started: Arc::new(Mutex::new(HashMap::new())),
            last_client_push: Arc::new(Mutex::new(HashMap::new())),
            conn_seq: Arc::new(AtomicU64::new(0)),
            relay_key: Arc::new(RelayKey::derive("test-key")),
            jwt_key: Arc::new(RelayKey::derive("test-key").subkey("jwt")),
            download_cache: Arc::new(Mutex::new(None)),
            upgrading: Arc::new(Mutex::new(HashMap::new())),
            stale_bounces: Arc::new(Mutex::new(HashMap::new())),
            local_conns: Arc::new(Mutex::new(HashMap::new())),
            push: Arc::new(crate::apns::Pusher::Capture(Mutex::new(Vec::new()))),
            focus: Arc::new(Mutex::new(HashMap::new())),
            fallback_sent: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}
