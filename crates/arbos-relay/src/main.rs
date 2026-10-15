mod admin;
mod api;
mod apns;
mod auth;
mod avatar;
mod blobs;
mod boxes;
mod browser;
mod channels;
mod connectors;
mod credits;
mod daytona;
mod db;
mod db_accounts;
mod db_people;
mod db_push;
mod groups;
mod images;
mod local;
mod migrate;
mod notify;
mod people;
mod proxy;
mod rooms;
mod routines;
mod signin;
mod skills;
mod smoke;
mod state;
mod upgrade;
mod vault;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use axum::routing::{delete, get, post};
use axum::Router;
use clap::{Parser, Subcommand};
use tokio::sync::{Mutex, RwLock};

use crate::state::{AppState, AuthMode, Config};

#[derive(Parser, Debug)]
#[command(
    name = "arbos-relay",
    version,
    about = "Arbos relay: registry, pairing, sync rooms, blobs, model proxy, Daytona"
)]
struct Args {
    /// Listen address.
    #[arg(long, env = "ARBOS_RELAY_LISTEN", default_value = "127.0.0.1:7700")]
    listen: String,
    /// URL boxes and clients use to reach this relay.
    #[arg(long, env = "ARBOS_RELAY_PUBLIC_URL")]
    public_url: Option<String>,
    /// Data directory (SQLite + blobs + local boxes).
    #[arg(long, env = "ARBOS_RELAY_DATA", default_value = "./.arbos-local/relay")]
    data: PathBuf,
    /// Admin token for `POST /pair/new`; generated and printed when absent.
    #[arg(long, env = "ARBOS_RELAY_ADMIN_TOKEN")]
    admin_token: Option<String>,
    /// Daytona snapshot name for new boxes.
    #[arg(
        long,
        env = "ARBOS_DAYTONA_SNAPSHOT",
        default_value = "arbos-agent-0.1.11"
    )]
    daytona_snapshot: String,
    /// Inline Dockerfile (e.g. image/Dockerfile.dev-inline) built by Daytona when the snapshot does not exist.
    #[arg(long, env = "ARBOS_DAYTONA_DOCKERFILE")]
    daytona_dockerfile: Option<PathBuf>,
    /// Directory served to boxes at /bootstrap/{file} (arbos-agentd, arbos-pi-ext.tar).
    #[arg(long, env = "ARBOS_BOOTSTRAP_DIR")]
    bootstrap_dir: Option<PathBuf>,
    /// Path to the arbos-agentd binary for `process` boxes.
    #[arg(long, env = "ARBOS_AGENTD_BIN", default_value = "arbos-agentd")]
    agentd_bin: PathBuf,
    /// Docker image for `docker` boxes.
    #[arg(long, env = "ARBOS_LOCAL_BOX_IMAGE", default_value = "arbos-agent:dev")]
    local_box_image: String,
    /// OpenAI-compatible upstream (OpenRouter by default).
    #[arg(
        long,
        env = "ARBOS_OPENAI_BASE",
        default_value = "https://openrouter.ai/api/v1"
    )]
    openai_base: String,
    /// Upstream for image generation (needs `OPENAI_API_KEY`; OpenRouter has no images endpoint).
    #[arg(
        long,
        env = "ARBOS_OPENAI_IMAGES_BASE",
        default_value = "https://api.openai.com/v1"
    )]
    openai_images_base: String,
    /// Anthropic-compatible upstream; OpenRouter's `/api/v1` when only an OpenRouter key exists.
    #[arg(long, env = "ARBOS_ANTHROPIC_BASE")]
    anthropic_base: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the server (default).
    Serve,
    /// Print a one-time pairing code (writes to the same SQLite file).
    PairCode,
    /// D25 lifecycle check against the live Daytona API with timings (create → stop → start → archive → start → delete).
    DaytonaSmoke {
        /// Snapshot name; when absent, an inline Dockerfile (node:24 + pi) is built by Daytona.
        #[arg(long)]
        snapshot: Option<String>,
        /// Keep the sandbox instead of deleting it.
        #[arg(long)]
        keep: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,loro_internal=warn,arbos_relay=debug".into()),
        )
        .init();
    let args = Args::parse();
    let db = db::Db::open(&args.data.join("relay.db"))?;

    if let Some(Cmd::PairCode) = args.cmd {
        let code = arbos_core::auth::pairing_code();
        db.new_pairing_code(&code, 10 * 60 * 1000)?;
        println!("{code}");
        return Ok(());
    }

    let openrouter_key = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|s| !s.is_empty());
    let anthropic_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|s| !s.is_empty());
    let openai_key = std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|s| !s.is_empty());
    let daytona_key = std::env::var("DAYTONA_API_KEY")
        .ok()
        .filter(|s| !s.is_empty());
    let anthropic_base = args.anthropic_base.clone().unwrap_or_else(|| {
        if anthropic_key.is_some() {
            "https://api.anthropic.com".to_owned()
        } else {
            "https://openrouter.ai/api".to_owned()
        }
    });
    let admin_token = args.admin_token.clone().or_else(|| {
        let t = arbos_core::auth::mint(arbos_core::auth::TokenKind::Device).replace("adv_", "adm_");
        tracing::info!("no ARBOS_RELAY_ADMIN_TOKEN set; generated admin token: {t}");
        Some(t)
    });
    let public_url = env_nonempty("ARBOS_PUBLIC_URL")
        .or_else(|| args.public_url.clone())
        .unwrap_or_else(|| format!("http://{}", args.listen));
    // Both spellings work: the vault item uses ARBOS_GITHUB_CLIENT_*, the design ARBOS_GITHUB_OAUTH_CLIENT_*.
    let github_client_id = env_nonempty("ARBOS_GITHUB_OAUTH_CLIENT_ID")
        .or_else(|| env_nonempty("ARBOS_GITHUB_CLIENT_ID"));
    let github_client_secret = env_nonempty("ARBOS_GITHUB_OAUTH_CLIENT_SECRET")
        .or_else(|| env_nonempty("ARBOS_GITHUB_CLIENT_SECRET"));
    let auth_mode = match env_nonempty("ARBOS_AUTH_MODE").as_deref() {
        Some("github") => AuthMode::Github,
        Some("paircode") => AuthMode::Paircode,
        Some(other) => anyhow::bail!("ARBOS_AUTH_MODE must be github or paircode, not {other:?}"),
        None if github_client_id.is_some() => AuthMode::Github,
        None => AuthMode::Paircode,
    };
    if auth_mode == AuthMode::Github
        && (github_client_id.is_none() || github_client_secret.is_none())
    {
        anyhow::bail!("ARBOS_AUTH_MODE=github needs ARBOS_GITHUB_CLIENT_ID and ARBOS_GITHUB_CLIENT_SECRET (or the ARBOS_GITHUB_OAUTH_CLIENT_* names)");
    }

    let cfg = Config {
        public_url,
        data_dir: args.data.clone(),
        admin_token,
        openrouter_key,
        anthropic_key,
        openai_key,
        openai_base: args.openai_base.clone(),
        openai_images_base: args.openai_images_base.clone(),
        anthropic_base,
        daytona_snapshot: args.daytona_snapshot.clone(),
        daytona_dockerfile: args.daytona_dockerfile.clone(),
        bootstrap_dir: args.bootstrap_dir.clone(),
        agentd_bin: args.agentd_bin.clone(),
        local_box_image: args.local_box_image.clone(),
        box_idle_grace_ms: 30_000,
        connectors_mock: std::env::var("ARBOS_CONNECTORS_MOCK").is_ok_and(|v| v == "1"),
        channels_mock: std::env::var("ARBOS_CHANNELS_MOCK").is_ok_and(|v| v == "1"),
        image_mock: std::env::var("ARBOS_IMAGE_MOCK").is_ok_and(|v| v == "1"),
        voice_mock: std::env::var("ARBOS_VOICE_MOCK").is_ok_and(|v| v == "1"),
        openai_audio_base: std::env::var("ARBOS_OPENAI_AUDIO_BASE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_owned()),
        vault_mock: std::env::var("ARBOS_VAULT_MOCK").is_ok_and(|v| v == "1"),
        auth_mode,
        github_client_id,
        github_client_secret,
        auth_redirects: env_nonempty("ARBOS_AUTH_REDIRECTS")
            .map(|s| {
                s.split(',')
                    .map(|p| p.trim().to_owned())
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        default_grant_usd: env_f64("ARBOS_DEFAULT_GRANT_USD", 100.0),
        default_daily_cap_usd: env_f64("ARBOS_DEFAULT_DAILY_CAP_USD", 20.0),
        global_daily_cap_usd: env_f64("ARBOS_GLOBAL_DAILY_CAP_USD", 500.0),
        global_machine_daily_cap_usd: env_f64("ARBOS_GLOBAL_MACHINE_DAILY_CAP_USD", 400.0),
        always_on: match env_nonempty("ARBOS_ALWAYS_ON").as_deref() {
            Some("0") | Some("false") | Some("off") => false,
            Some(_) => true,
            None => auth_mode == AuthMode::Github,
        },
        machine_usd_per_hour: env_f64("ARBOS_MACHINE_USD_PER_HOUR", 0.332),
        machine_stopped_usd_per_day: env_f64("ARBOS_MACHINE_STOPPED_USD_PER_DAY", 0.013),
        avatar_slugs: avatar::slugs_from_env(),
        avatar_colors: avatar::colors_from_env(),
        site_url: env_nonempty("ARBOS_SITE_URL").unwrap_or_else(|| "https://arbos.life".to_owned()),
        releases_repo: env_nonempty("ARBOS_RELEASES_REPO")
            .unwrap_or_else(|| "unarbos/arbos".to_owned()),
    };
    let relay_key = Arc::new(connectors::crypto::RelayKey::load(&db)?);
    let jwt_key: [u8; 32] = match env_nonempty("ARBOS_JWT_KEY") {
        Some(k) => connectors::crypto::RelayKey::derive(&k).subkey("jwt"),
        None => relay_key.subkey("jwt"),
    };
    let daytona = daytona_key.map(|k| {
        daytona::Daytona::new(
            k,
            std::env::var("DAYTONA_API_URL").ok(),
            std::env::var("DAYTONA_ORG_ID").ok(),
        )
    });
    if let Some(Cmd::DaytonaSmoke { snapshot, keep }) = args.cmd {
        let dt = daytona.ok_or_else(|| anyhow::anyhow!("DAYTONA_API_KEY is required"))?;
        return smoke::run(&dt, snapshot, keep).await;
    }
    let state = AppState {
        db: db.clone(),
        cfg: Arc::new(cfg),
        http: reqwest::Client::builder()
            .pool_idle_timeout(std::time::Duration::from_secs(300))
            .build()?,
        daytona,
        rooms: Arc::new(RwLock::new(HashMap::new())),
        box_states: Arc::new(RwLock::new(HashMap::new())),
        local_children: Arc::new(Mutex::new(HashMap::new())),
        box_conns: Arc::new(Mutex::new(HashMap::new())),
        wake_started: Arc::new(Mutex::new(HashMap::new())),
        last_client_push: Arc::new(Mutex::new(HashMap::new())),
        stale_bounces: Arc::new(Mutex::new(HashMap::new())),
        conn_seq: Arc::new(AtomicU64::new(0)),
        relay_key,
        local_conns: Arc::new(Mutex::new(HashMap::new())),
        jwt_key: Arc::new(jwt_key),
        download_cache: Arc::new(Mutex::new(None)),
        upgrading: Arc::new(Mutex::new(HashMap::new())),
        push: apns::from_env().unwrap_or_else(|| Arc::new(apns::Pusher::Off)),
        focus: Arc::new(Mutex::new(HashMap::new())),
        fallback_sent: Arc::new(Mutex::new(HashMap::new())),
    };
    tracing::info!(auth_mode = auth_mode.as_str(), public_url = %state.cfg.public_url, "sign-in");
    if state.cfg.connectors_mock {
        tracing::info!("ARBOS_CONNECTORS_MOCK=1: connectors return canned data; sends land in connector_outbox");
    }
    if state.cfg.channels_mock {
        tracing::info!("ARBOS_CHANNELS_MOCK=1: channel sends land in channel_outbox; POST /channels/mock/inbound injects messages");
    }
    if state.cfg.voice_mock {
        tracing::info!(
            "ARBOS_VOICE_MOCK=1: /proxy/v1/audio/* return a mock transcript and a 1 s sine WAV"
        );
    }

    if state.cfg.image_mock {
        tracing::info!("ARBOS_IMAGE_MOCK=1: /proxy/v1/images/generations draws placeholder PNGs");
    } else if state.cfg.openai_key.is_none() {
        tracing::info!("no OPENAI_API_KEY: image generation is unavailable (set ARBOS_IMAGE_MOCK=1 for placeholders)");
    }
    vault::seed_mock(&state)?;

    if auth_mode == AuthMode::Paircode && db.device_count()? == 0 {
        let code = arbos_core::auth::pairing_code();
        db.new_pairing_code(&code, 60 * 60 * 1000)?;
        tracing::info!("no devices paired yet; one-time pairing code (1 h): {code}");
    }

    db.local_devices_reset()?;
    rooms::spawn_sweeper(state.clone()).await;
    boxes::spawn_wake_runner(state.clone()).await;
    routines::spawn_scheduler(state.clone()).await;
    channels::spawn_pollers(state.clone()).await;
    credits::spawn_meter(state.clone()).await;
    boxes::spawn_always_on(state.clone()).await;
    upgrade::spawn_history_sweeper(state.clone()).await;
    boxes::spawn_project_purge(state.clone()).await;

    let app = Router::new()
        .route("/healthz", get(api::healthz))
        .route("/pair/new", post(api::pair_new))
        .route("/pair", post(api::pair))
        .route("/auth/github/start", get(signin::github_start))
        .route("/auth/github/callback", get(signin::github_callback))
        .route("/auth/token", post(signin::token))
        .route("/auth/refresh", post(signin::refresh))
        .route("/auth/logout", post(signin::logout))
        .route("/auth/devices", get(signin::devices_list))
        .route("/auth/devices/{id}", delete(signin::device_revoke))
        .route("/me", get(signin::me))
        .route("/me/pushes", get(people::me_pushes))
        .route("/me/unread", get(notify::me_unread))
        .route(
            "/devices/{id}/push",
            post(notify::register_push).delete(notify::unregister_push),
        )
        .route("/chats/{id}/notify", post(notify::notify_people_chat))
        .route(
            "/projects/{project_id}/chats/{chat_id}/notify",
            post(notify::notify_project_chat),
        )
        .route("/users/search", get(people::users_search))
        .route("/users", get(people::users_directory))
        .route(
            "/chats",
            get(people::chats_list).post(people::chats_create_group),
        )
        .route("/chats/dm", post(people::dm_create))
        .route(
            "/chats/{id}",
            get(people::chat_get).delete(people::chat_delete),
        )
        .route("/chats/{id}/read", axum::routing::put(people::chat_read))
        .route(
            "/chats/{id}/settings",
            axum::routing::put(people::chat_settings),
        )
        .route("/chats/{id}/hide", post(people::chat_hide))
        .route("/chats/{id}/leave", post(people::chat_leave))
        .route("/chats/{id}/agents", post(people::agent_add))
        .route(
            "/chats/{id}/agents/{project_id}",
            delete(people::agent_remove),
        )
        .route(
            "/chats/{id}/agents/{project_id}/settings",
            axum::routing::put(people::agent_settings),
        )
        .route(
            "/chats/{id}/members/{user_id}",
            delete(people::member_remove),
        )
        .route("/box/people/{id}", get(people::box_people_chat))
        .route(
            "/projects/{id}/members",
            get(people::project_members_list).post(people::project_member_add),
        )
        .route(
            "/projects/{id}/members/{user_id}",
            delete(people::project_member_remove),
        )
        .route(
            "/blocks",
            get(people::blocks_list).post(people::block_create),
        )
        .route("/blocks/{user_id}", delete(people::block_delete))
        .route(
            "/invites",
            get(people::invites_list).post(people::invite_create),
        )
        .route("/invites/{code}", delete(people::invite_revoke))
        .route("/i/{code}", get(people::invite_landing))
        .route("/download/mac", get(signin::download_mac))
        .route("/devices", get(api::list_devices))
        .route("/devices/{id}", delete(api::revoke_device))
        .route(
            "/projects",
            get(api::list_projects).post(api::create_project),
        )
        .route(
            "/projects/{id}",
            get(api::get_project)
                .patch(api::patch_project)
                .delete(api::delete_project),
        )
        .route(
            "/projects/{id}/chats/{chat}",
            axum::routing::patch(api::patch_chat).delete(api::delete_chat),
        )
        .route(
            "/projects/{id}/browser/stream",
            get(browser::browser_stream),
        )
        .route("/projects/{id}/history/{chat}", get(api::chat_history))
        .route("/projects/{id}/status", get(api::project_status))
        .route(
            "/projects/{id}/chats",
            get(api::list_chats).post(api::create_chat),
        )
        .route("/projects/{id}/groups", post(groups::create_group))
        .route(
            "/projects/{id}/groups/{chat}",
            get(groups::get_group).patch(groups::patch_group),
        )
        .route(
            "/projects/{id}/groups/{chat}/send",
            post(groups::group_send),
        )
        .route(
            "/projects/{id}/groups/{chat}/reply",
            post(groups::group_reply),
        )
        .route("/projects/{id}/wake", post(api::wake))
        .route("/projects/{id}/start", post(api::start))
        .route("/projects/{id}/stop", post(api::stop))
        .route("/projects/{id}/keepalive", post(api::keepalive))
        .route("/projects/{id}/workspace", post(api::set_workspace_state))
        // One box per user: the box speaks for every project of its user.
        .route("/machine", get(api::machine))
        .route("/machine/start", post(api::machine_start))
        .route("/machine/stop", post(api::machine_stop))
        .route("/machine/migrate", post(api::machine_migrate))
        .route("/machine/restart", post(api::machine_restart))
        .route("/machine/upgrade", post(upgrade::machine_upgrade))
        .route(
            "/admin/users/{id}/box/upgrade",
            post(upgrade::admin_upgrade),
        )
        .route("/admin/users/{id}/box/repair", post(upgrade::admin_repair))
        .route(
            "/admin/users/{id}/box",
            axum::routing::put(api::admin_set_user_box).delete(api::admin_delete_user_box),
        )
        .route("/admin/users", get(admin::list_users))
        .route("/admin/users/test", post(admin::create_test_user))
        .route("/admin/users/{id}", delete(admin::delete_test_user))
        .route(
            "/admin/users/{id}/credits",
            axum::routing::put(admin::set_credits),
        )
        .route(
            "/admin/killswitch",
            get(admin::get_kill_switch).put(admin::set_kill_switch),
        )
        .route("/box/hello", post(api::box_hello))
        .route("/box/projects", get(api::box_projects))
        .route("/box/keepalive", post(api::box_keepalive))
        .route("/projects/{id}/push", post(api::push))
        .route("/projects/{id}/pushes", get(api::take_pushes))
        .route("/projects/{id}/usage", get(api::usage))
        .route("/projects/{id}/timings", post(api::record_timing))
        .route(
            "/projects/{id}/routines",
            get(routines::list).post(routines::create),
        )
        .route(
            "/projects/{id}/routines/{rid}",
            axum::routing::patch(routines::patch).delete(routines::delete),
        )
        .route("/projects/{id}/routines/{rid}/run", post(routines::run_now))
        .route("/projects/{id}/routines/{rid}/runs", get(routines::runs))
        .route(
            "/projects/{id}/routines/{rid}/runs/{run}",
            post(routines::report_run),
        )
        .route("/routines/{rid}/fire", post(routines::webhook_fire))
        .route("/skills", get(skills::list).post(skills::create))
        .route("/skills/search", get(skills::list))
        .route("/skills/index", get(skills::index))
        .route(
            "/skills/{id}",
            get(skills::get).put(skills::update).delete(skills::delete),
        )
        .route(
            "/projects/{id}/audit",
            post(api::record_audit).get(api::list_audit),
        )
        .route("/local/{device}", get(local::ws_local))
        .route("/local/devices", get(local::list))
        .route(
            "/local/devices/{device}",
            axum::routing::patch(local::patch).delete(local::delete),
        )
        .route("/projects/{id}/local/{device}/exec", post(local::exec))
        .route("/room/{project}/{chat}", get(rooms::ws_room))
        .route(
            "/room/{project}/{chat}/checkpoint",
            get(rooms::get_checkpoint).post(rooms::post_checkpoint),
        )
        .route("/room/{project}/{chat}/stats", get(rooms::room_stats))
        .route("/blob/{project}/{key}", get(blobs::get).put(blobs::put))
        .route("/bootstrap/{file}", get(blobs::bootstrap))
        .route("/proxy/v1/models", get(proxy::openai_models))
        .route("/proxy/v1/images/generations", post(images::generations))
        .route(
            "/proxy/v1/audio/transcriptions",
            post(proxy::audio_transcriptions),
        )
        .route("/proxy/v1/audio/speech", post(proxy::audio_speech))
        .route("/proxy/v1/{*tail}", post(proxy::openai))
        .route("/proxy/anthropic/{*tail}", post(proxy::anthropic))
        .route("/connectors", get(connectors::list))
        .route("/connectors/catalog", get(connectors::catalog))
        .route("/connectors/outbox", get(connectors::outbox))
        .route("/connectors/call", post(connectors::call))
        .route(
            "/connectors/oauth/callback",
            get(connectors::oauth_callback),
        )
        .route("/connectors/{provider}/connect", post(connectors::connect))
        .route("/connectors/{id}", delete(connectors::disconnect))
        .route("/channels", get(channels::overview))
        .route("/channels/pair/new", post(channels::pair_new))
        .route("/channels/bindings", get(channels::list_bindings))
        .route("/channels/bindings/{id}", delete(channels::unbind))
        .route("/channels/mock/inbound", post(channels::mock_inbound))
        .route("/channels/mock/outbox", get(channels::mock_outbox))
        .route("/channels/slack/events", post(channels::slack::events))
        .route(
            "/channels/telegram/webhook/{secret}",
            post(channels::telegram::webhook),
        )
        .route(
            "/channels/whatsapp/webhook",
            get(channels::whatsapp::verify).post(channels::whatsapp::webhook),
        )
        .route("/projects/{id}/channels/reply", post(channels::reply))
        .route(
            "/vault/items",
            get(vault::list_items).post(vault::create_item),
        )
        .route(
            "/vault/items/{id}",
            axum::routing::put(vault::update_item).delete(vault::delete_item),
        )
        .route("/vault/requests", get(vault::list_requests))
        .route("/vault/requests/{id}", get(vault::request_status))
        .route("/vault/requests/{id}/decide", post(vault::decide_request))
        .route("/vault/requests/{id}/values", post(vault::request_values))
        .route("/vault/request", post(vault::create_request))
        .route("/vault/mock/echo", axum::routing::any(vault::mock_echo))
        .route("/egress", post(vault::egress))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    tracing::info!(listen = %args.listen, data = %args.data.display(), "arbos-relay listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn env_f64(name: &str, default: f64) -> f64 {
    env_nonempty(name)
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(default)
}
