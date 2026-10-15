//! Message notifications (iOS APNs today; the Mac reads the same `people_message` pushes).
//!
//! The relay never parses Loro rows, so whoever writes a whole message tells it:
//! agentd when an agent reply completes, a device when its person sends. Each announcement is
//! one `chat_events` row (deduplicated by `(chat, message_id)`), which drives unread counts and
//! one APNs alert per recipient device — never the sender's own devices, never a device whose
//! presence says that chat is on screen. Reading a chat anywhere pushes the new badge to the
//! user's other devices.
//!
//! Endpoints: `POST/DELETE /devices/{id}/push`, `GET /me/unread`, `POST /chats/{id}/notify`
//! (people chats), `POST /projects/{project}/chats/{chat}/notify`; `PUT /chats/{id}/read` (in
//! `people.rs`) accepts project chat ids too.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use arbos_core::ids::now_ms;

use crate::apns::{Delivery, Outgoing};
use crate::auth::Principal;
use crate::db::Project;
use crate::db_push::PushToken;
use crate::people;
use crate::signin::api_error;
use crate::state::AppState;

/// APNs category the apps register; the service extension turns it into a communication notification.
pub const CATEGORY: &str = "ARBOS_MESSAGE";
/// Alert body cap (the lock screen shows about this much).
pub const BODY_MAX: usize = 140;
/// A presence frame arrives every 15 s; a focus older than this is stale (the app is gone).
pub const FOCUS_TTL_MS: i64 = 45_000;
/// How long the row-level fallback waits for a proper announcement before it notifies without content.
const FALLBACK_DELAY: Duration = Duration::from_millis(2500);
/// At most one content-less fallback per (chat, recipient) per minute.
const FALLBACK_EVERY_MS: i64 = 60_000;

/// device_id → (chat id on screen, when it said so).
pub type Focus = Arc<Mutex<HashMap<String, (String, i64)>>>;

#[derive(Debug, Clone)]
pub struct ChatRef {
    pub chat_id: String,
    /// `None` for people chats.
    pub project_id: Option<String>,
}

impl ChatRef {
    pub fn people(chat_id: &str) -> Self {
        Self {
            chat_id: chat_id.to_owned(),
            project_id: None,
        }
    }

    pub fn project(project_id: &str, chat_id: &str) -> Self {
        Self {
            chat_id: chat_id.to_owned(),
            project_id: Some(project_id.to_owned()),
        }
    }

    /// What the apps see as `project_id`: `people` for DMs.
    pub fn project_label(&self) -> &str {
        self.project_id.as_deref().unwrap_or(people::PEOPLE_PROJECT)
    }
}

/// Who wrote the message, as the apps render it.
#[derive(Debug, Clone)]
pub struct Sender {
    pub kind: &'static str,
    pub id: String,
    pub name: String,
    pub handle: String,
    pub avatar: Option<String>,
    pub avatar_color: Option<String>,
    pub avatar_url: Option<String>,
    /// The person behind it (the user for a human; the owner for an agent is *not* excluded).
    pub user_id: Option<String>,
}

impl Sender {
    pub fn user(state: &AppState, user_id: &str) -> Self {
        let card = state
            .db
            .user_card(user_id)
            .ok()
            .flatten()
            .unwrap_or_default();
        let handle = card["handle"].as_str().unwrap_or(user_id).to_owned();
        let name = card["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("@{handle}"));
        Self {
            kind: "user",
            id: user_id.to_owned(),
            name,
            handle,
            avatar: None,
            avatar_color: None,
            avatar_url: card["avatar_url"].as_str().map(str::to_owned),
            user_id: Some(user_id.to_owned()),
        }
    }

    pub fn agent(state: &AppState, p: &Project) -> Self {
        Self {
            kind: "agent",
            id: p.id.clone(),
            name: p.name.clone(),
            handle: String::new(),
            avatar: Some(
                p.avatar
                    .clone()
                    .unwrap_or_else(|| crate::avatar::default_for(&p.id, &state.cfg.avatar_slugs)),
            ),
            avatar_color: Some(p.avatar_color.clone().unwrap_or_else(|| {
                crate::avatar::default_color_for(&p.id, &state.cfg.avatar_colors)
            })),
            avatar_url: None,
            user_id: None,
        }
    }

    pub fn json(&self) -> Value {
        json!({
            "kind": self.kind, "id": self.id, "name": self.name, "handle": self.handle,
            "avatar": self.avatar, "avatar_color": self.avatar_color, "avatar_url": self.avatar_url,
        })
    }

    /// Alert title: the agent's name, or `@handle` for a person.
    pub fn title(&self) -> String {
        if self.kind == "agent" || self.handle.is_empty() {
            self.name.clone()
        } else {
            format!("@{}", self.handle)
        }
    }
}

/// First `BODY_MAX` characters of the message, one line, no trailing fragments of whitespace.
pub fn brief(text: &str) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= BODY_MAX {
        return one_line;
    }
    let cut: String = one_line.chars().take(BODY_MAX - 1).collect();
    format!("{}…", cut.trim_end())
}

/// People who should hear about a message in this chat (the sender included; callers skip them).
pub fn recipients(state: &AppState, chat: &ChatRef) -> Vec<String> {
    let mut out = match &chat.project_id {
        None => state
            .db
            .members_of(&chat.chat_id)
            .unwrap_or_default()
            .iter()
            .filter_map(|m| m.user_id().map(str::to_owned))
            .collect(),
        Some(pid) => {
            let mut v: Vec<String> = state.user_of(pid).into_iter().collect();
            v.extend(
                state
                    .db
                    .project_members(pid)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(u, _, _)| u),
            );
            v
        }
    };
    out.sort();
    out.dedup();
    out
}

/// Is this device looking at this chat right now?
pub async fn device_focused(state: &AppState, device_id: &str, chat_id: &str) -> bool {
    let focus = state.focus.lock().await;
    match focus.get(device_id) {
        Some((c, at)) => c == chat_id && now_ms() - at < FOCUS_TTL_MS,
        None => false,
    }
}

/// Presence from a room socket: remember (or forget) what the device looks at.
pub async fn set_focus(state: &AppState, device_id: &str, focused_chat: Option<&str>) {
    let mut focus = state.focus.lock().await;
    match focused_chat {
        Some(c) => {
            focus.insert(device_id.to_owned(), (c.to_owned(), now_ms()));
        }
        None => {
            focus.remove(device_id);
        }
    }
}

/// The socket to `chat_id` closed: that device is no longer looking at it.
pub async fn clear_focus(state: &AppState, device_id: &str, chat_id: &str) {
    let mut focus = state.focus.lock().await;
    if focus.get(device_id).is_some_and(|(c, _)| c == chat_id) {
        focus.remove(device_id);
    }
}

fn alert_payload(
    chat: &ChatRef,
    sender: &Sender,
    message_id: &str,
    body: &str,
    badge: i64,
    chat_title: Option<&str>,
) -> Value {
    let mut v = json!({
        "aps": {
            "alert": { "title": sender.title(), "body": body },
            "badge": badge,
            "sound": "default",
            "thread-id": chat.chat_id,
            "category": CATEGORY,
            "mutable-content": 1,
        },
        "chat_id": chat.chat_id,
        "project_id": chat.project_label(),
        "message_id": message_id,
        "sender": sender.json(),
    });
    if let Some(t) = chat_title {
        v["chat_title"] = json!(t);
    }
    v
}

/// Group-style chats carry the room's name so the banner reads "sender in <chat>": a shared
/// project (owner plus guests) uses the project name; DMs carry none.
fn chat_title(state: &AppState, chat: &ChatRef) -> Option<String> {
    let pid = chat.project_id.as_deref()?;
    let shared = state
        .db
        .project_members(pid)
        .map(|m| !m.is_empty())
        .unwrap_or(false);
    if !shared {
        return None;
    }
    state.db.project(pid).ok().flatten().map(|p| p.name)
}

async fn deliver(state: &AppState, token: &PushToken, out: Outgoing) {
    match state.push.send(out).await {
        Delivery::Sent => {}
        Delivery::TokenGone => {
            tracing::info!(device = %token.device_id, "APNs token gone; forgotten");
            let _ = state.db.delete_push_token_value(&token.token);
        }
        Delivery::Failed(e) => {
            tracing::warn!(device = %token.device_id, error = %e, "APNs send failed");
        }
    }
}

/// Announce one whole message: record it, then alert every recipient device except the
/// sender's own and those focused on the chat. Returns the event id, `None` when already seen.
pub async fn announce(
    state: &AppState,
    chat: &ChatRef,
    sender: &Sender,
    message_id: &str,
    text: &str,
) -> Option<i64> {
    let event_id = state
        .db
        .insert_chat_event(
            &chat.chat_id,
            chat.project_id.as_deref(),
            message_id,
            sender.user_id.as_deref(),
        )
        .ok()
        .flatten()?;
    let body = brief(text);
    let title = chat_title(state, chat);
    for user in recipients(state, chat) {
        if sender.user_id.as_deref() == Some(user.as_str()) {
            continue;
        }
        if !state.push.is_on() {
            continue;
        }
        let tokens = state.db.push_tokens_of_user(&user).unwrap_or_default();
        if tokens.is_empty() {
            continue;
        }
        let badge = state.db.unread_total(&user).unwrap_or(0);
        for t in tokens {
            if device_focused(state, &t.device_id, &chat.chat_id).await {
                continue;
            }
            let out = Outgoing {
                device_token: t.token.clone(),
                environment: t.environment.clone(),
                push_type: "alert",
                collapse_id: Some(format!("msg:{}", message_id)),
                payload: alert_payload(chat, sender, message_id, &body, badge, title.as_deref()),
            };
            deliver(state, &t, out).await;
        }
    }
    Some(event_id)
}

/// The user read something somewhere: every other device gets the new badge count.
pub async fn badge_refresh(state: &AppState, user_id: &str, except_device: &str) {
    if !state.push.is_on() {
        return;
    }
    let total = state.db.unread_total(user_id).unwrap_or(0);
    for t in state.db.push_tokens_of_user(user_id).unwrap_or_default() {
        if t.device_id == except_device {
            continue;
        }
        let out = Outgoing {
            device_token: t.token.clone(),
            environment: t.environment.clone(),
            push_type: "alert",
            collapse_id: Some(format!("badge:{user_id}")),
            payload: json!({ "aps": { "badge": total } }),
        };
        deliver(state, &t, out).await;
    }
}

/// A device pushed a row into a chat without announcing a message (older apps). Wait a moment
/// for the announcement; if none comes, notify the others with the sender's name only, at most
/// once a minute per recipient and chat, and count it as one unread message.
pub fn on_row_from_device(state: &AppState, chat: ChatRef, user_id: &str, seq: i64) {
    if !state.push.is_on() {
        return;
    }
    let state = state.clone();
    let user_id = user_id.to_owned();
    let at = now_ms();
    tokio::spawn(async move {
        tokio::time::sleep(FALLBACK_DELAY).await;
        if state
            .db
            .chat_event_since(&chat.chat_id, &user_id, at - 1_000)
            .unwrap_or(true)
        {
            return;
        }
        let key = (chat.chat_id.clone(), user_id.clone());
        {
            let mut last = state.fallback_sent.lock().await;
            if last.get(&key).is_some_and(|t| at - t < FALLBACK_EVERY_MS) {
                return;
            }
            last.insert(key, at);
        }
        let sender = Sender::user(&state, &user_id);
        announce(&state, &chat, &sender, &format!("row:{seq}"), "New message").await;
    });
}

// ---- handlers -----------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RegisterBody {
    pub platform: String,
    pub token: String,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub bundle_id: Option<String>,
    #[serde(default)]
    pub app_version: Option<String>,
}

fn own_device(principal: &Principal, id: &str) -> Result<String, Box<Response>> {
    match principal {
        Principal::Device {
            id: mine, user_id, ..
        } if mine == id => Ok(user_id.clone()),
        Principal::Device { .. } => Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "a device registers only its own token",
        ))),
        Principal::Box { .. } | Principal::Admin => Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "devices only",
        ))),
    }
}

/// `POST /devices/{id}/push {platform: "ios", token, environment, bundle_id, app_version}`
pub async fn register_push(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<RegisterBody>,
) -> Response {
    let user_id = match own_device(&principal, &id) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    if b.platform != "ios" {
        return api_error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "platform must be \"ios\"",
        );
    }
    let token = b.token.trim().to_ascii_lowercase();
    if token.is_empty() || token.len() > 512 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "token must be the hex APNs device token",
        );
    }
    let environment = match b.environment.as_deref().unwrap_or("production") {
        "development" => "development",
        "production" => "production",
        _ => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "environment must be development or production",
            )
        }
    };
    let t = PushToken {
        device_id: id,
        user_id,
        platform: b.platform,
        token,
        environment: environment.to_owned(),
        bundle_id: b
            .bundle_id
            .unwrap_or_else(|| crate::apns::DEFAULT_TOPIC.to_owned()),
        app_version: b.app_version.unwrap_or_default(),
        updated_at: now_ms(),
    };
    match state.db.upsert_push_token(&t) {
        Ok(()) => Json(json!({
            "device_id": t.device_id, "environment": t.environment, "push": state.push.is_on(),
        }))
        .into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

/// `DELETE /devices/{id}/push` — sign-out; the token is never pushed to again.
pub async fn unregister_push(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = own_device(&principal, &id) {
        return *r;
    }
    match state.db.delete_push_token(&id) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

/// `GET /me/unread` → `{chats: [{chat_id, project_id, unread}], total}`
pub async fn me_unread(State(state): State<AppState>, principal: Principal) -> Response {
    let Principal::Device { user_id, .. } = &principal else {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "devices only");
    };
    match state.db.unread_for_user(user_id) {
        Ok(chats) => {
            let total: i64 = chats.iter().map(|c| c.unread).sum();
            Json(json!({
                "chats": chats.iter().map(|c| json!({
                    "chat_id": c.chat_id,
                    "project_id": c.project_id.clone().unwrap_or_else(|| people::PEOPLE_PROJECT.to_owned()),
                    "unread": c.unread,
                })).collect::<Vec<_>>(),
                "total": total,
            }))
            .into_response()
        }
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct NotifyBody {
    pub message_id: String,
    #[serde(default)]
    pub text: String,
    /// People chats, box callers: which of the box's agents spoke.
    #[serde(default)]
    pub project_id: Option<String>,
}

/// The sender behind a notify call: a device is its person; a box is one of its owner's agents.
fn sender_for(
    state: &AppState,
    principal: &Principal,
    project_id: Option<&str>,
) -> Result<Sender, Box<Response>> {
    match principal {
        Principal::Device { user_id, .. } => Ok(Sender::user(state, user_id)),
        Principal::Box { user_id } => {
            let Some(pid) = project_id else {
                return Err(Box::new(api_error(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "project_id (the agent) is required",
                )));
            };
            match state.db.project(pid) {
                Ok(Some(p)) if &p.user_id == user_id => Ok(Sender::agent(state, &p)),
                Ok(_) => Err(Box::new(api_error(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "not this box's agent",
                ))),
                Err(e) => Err(Box::new(api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "db",
                    e.to_string(),
                ))),
            }
        }
        Principal::Admin => Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "devices and boxes only",
        ))),
    }
}

fn notify_response(event_id: Option<i64>) -> Response {
    Json(json!({ "recorded": event_id.is_some(), "event_id": event_id })).into_response()
}

/// `POST /chats/{id}/notify {message_id, text, project_id?}` — a whole message landed in a
/// people chat (member devices; boxes whose agent is in the chat).
pub async fn notify_people_chat(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<NotifyBody>,
) -> Response {
    if !state.allows(&principal, &format!("{}{id}", people::PEOPLE_ACL_PREFIX)) {
        return api_error(StatusCode::FORBIDDEN, "not_member", "not in this chat");
    }
    let sender = match sender_for(&state, &principal, b.project_id.as_deref()) {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let chat = ChatRef::people(&id);
    notify_response(announce(&state, &chat, &sender, &b.message_id, &b.text).await)
}

/// `POST /projects/{project_id}/chats/{chat_id}/notify {message_id, text}` — a whole message
/// landed in a project chat (owner and guest devices; the owner's box for its agent).
pub async fn notify_project_chat(
    State(state): State<AppState>,
    principal: Principal,
    Path((project_id, chat_id)): Path<(String, String)>,
    Json(b): Json<NotifyBody>,
) -> Response {
    if !state.allows(&principal, &project_id) {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "not your project");
    }
    let Ok(Some(project)) = state.db.project(&project_id) else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "no such project");
    };
    if !state.db.chat_exists(&project.id, &chat_id).unwrap_or(false) {
        return api_error(StatusCode::NOT_FOUND, "not_found", "no such chat");
    }
    let sender = match sender_for(&state, &principal, Some(&project.id)) {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let chat = ChatRef::project(&project.id, &chat_id);
    notify_response(announce(&state, &chat, &sender, &b.message_id, &b.text).await)
}

#[cfg(test)]
mod tests {
    //! Invariants: no push to the sender's own devices; none to a device focused on the chat;
    //! the badge is the recipient's total unread and drops when they read elsewhere.

    use arbos_core::ids::ulid;

    use super::*;
    use crate::db::Project;
    use crate::people::tests_support::{dm, person, project_of};
    use crate::state::test_support::test_state;
    use crate::state::AuthMode;

    fn device_id(p: &Principal) -> String {
        match p {
            Principal::Device { id, .. } => id.clone(),
            Principal::Box { .. } | Principal::Admin => unreachable!("device principal"),
        }
    }

    fn register(state: &AppState, p: &Principal, user: &str) -> String {
        let tok: String = ulid()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .collect();
        let tok = format!("{tok:0<64}");
        state
            .db
            .upsert_push_token(&PushToken {
                device_id: device_id(p),
                user_id: user.to_owned(),
                platform: "ios".into(),
                token: tok.clone(),
                environment: "development".into(),
                bundle_id: crate::apns::DEFAULT_TOPIC.into(),
                app_version: "1".into(),
                updated_at: now_ms(),
            })
            .expect("token");
        tok
    }

    /// A second signed-in device for the same person.
    fn second_device(state: &AppState, user: &str) -> Principal {
        let id = ulid();
        state
            .db
            .insert_device(&id, "d2", &ulid(), user, "github")
            .expect("device");
        Principal::Device {
            id,
            token: String::new(),
            user_id: user.to_owned(),
        }
    }

    #[tokio::test]
    async fn sender_devices_are_never_pushed_and_focus_suppresses() {
        let state = test_state(AuthMode::Github);
        let (alice, alice_dev) = person(&state, 1);
        let (bob, bob_dev) = person(&state, 2);
        let bob_dev2 = second_device(&state, &bob);
        let chat = dm(&state, &alice, &bob);
        let alice_tok = register(&state, &alice_dev, &alice);
        let bob_tok = register(&state, &bob_dev, &bob);
        let bob_tok2 = register(&state, &bob_dev2, &bob);

        let sender = Sender::user(&state, &alice);
        let ev = announce(&state, &ChatRef::people(&chat), &sender, "m1", "hello bob").await;
        assert!(ev.is_some());
        let sent = state.push.captured().await;
        let tokens: Vec<&str> = sent.iter().map(|o| o.device_token.as_str()).collect();
        assert!(
            !tokens.contains(&alice_tok.as_str()),
            "sender's own device is silent"
        );
        assert!(tokens.contains(&bob_tok.as_str()));
        assert!(tokens.contains(&bob_tok2.as_str()));
        assert_eq!(sent.len(), 2);
        let p = &sent[0].payload;
        assert_eq!(p["aps"]["alert"]["title"], "@user1");
        assert_eq!(p["aps"]["alert"]["body"], "hello bob");
        assert_eq!(p["aps"]["thread-id"], chat);
        assert_eq!(p["aps"]["category"], CATEGORY);
        assert_eq!(p["aps"]["mutable-content"], 1);
        assert_eq!(p["aps"]["badge"], 1);
        assert_eq!(p["project_id"], "people");
        assert_eq!(p["sender"]["kind"], "user");
        assert_eq!(p["sender"]["handle"], "user1");

        // The same message announced again is not a second notification.
        assert!(
            announce(&state, &ChatRef::people(&chat), &sender, "m1", "hello bob")
                .await
                .is_none()
        );
        assert_eq!(state.push.captured().await.len(), 2);

        // Bob's phone is looking at the chat: only his other device hears the next one.
        set_focus(&state, &device_id(&bob_dev), Some(&chat)).await;
        announce(
            &state,
            &ChatRef::people(&chat),
            &sender,
            "m2",
            "still there?",
        )
        .await;
        let sent = state.push.captured().await;
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[2].device_token, bob_tok2);
        assert_eq!(sent[2].payload["aps"]["badge"], 2);

        // Focus on a different chat does not suppress.
        set_focus(&state, &device_id(&bob_dev), Some("other-chat")).await;
        announce(&state, &ChatRef::people(&chat), &sender, "m3", "?").await;
        assert_eq!(state.push.captured().await.len(), 5);
    }

    #[tokio::test]
    async fn badge_is_total_unread_and_read_elsewhere_pushes_the_new_count() {
        let state = test_state(AuthMode::Github);
        let (alice, _alice_dev) = person(&state, 1);
        let (bob, bob_dev) = person(&state, 2);
        let bob_dev2 = second_device(&state, &bob);
        let dm_chat = dm(&state, &alice, &bob);
        let project_id = project_of(&state, &alice, "Research");
        state
            .db
            .add_project_member(&project_id, &bob, &alice)
            .expect("guest");
        let chat_id = ulid();
        state
            .db
            .insert_chat(&crate::db::Chat {
                id: chat_id.clone(),
                project_id: project_id.clone(),
                title: "Plan".into(),
                is_pinned: false,
                position: 0,
                created_at: now_ms(),
                doc_epoch: 1,
                kind: "chat".into(),
                participants: Vec::new(),
            })
            .expect("chat");
        let bob_tok = register(&state, &bob_dev, &bob);
        let bob_tok2 = register(&state, &bob_dev2, &bob);

        let alice_s = Sender::user(&state, &alice);
        announce(&state, &ChatRef::people(&dm_chat), &alice_s, "d1", "one").await;
        announce(&state, &ChatRef::people(&dm_chat), &alice_s, "d2", "two").await;
        let project: Project = state.db.project(&project_id).expect("p").expect("p");
        let agent = Sender::agent(&state, &project);
        announce(
            &state,
            &ChatRef::project(&project_id, &chat_id),
            &agent,
            "a1",
            "Here is the plan.",
        )
        .await;

        let unread = state.db.unread_for_user(&bob).expect("unread");
        let total: i64 = unread.iter().map(|u| u.unread).sum();
        assert_eq!(total, 3);
        assert_eq!(
            unread
                .iter()
                .find(|u| u.chat_id == dm_chat)
                .map(|u| u.unread),
            Some(2)
        );
        assert_eq!(
            unread
                .iter()
                .find(|u| u.chat_id == chat_id)
                .map(|u| u.unread),
            Some(1)
        );
        // Alice wrote two of them and is the agent's owner: nothing unread for her from the DM,
        // but the agent's reply counts (it is not her message).
        assert_eq!(state.db.unread_total(&alice).expect("alice"), 1);

        let sent = state.push.captured().await;
        let last = sent.last().expect("agent push");
        assert_eq!(last.payload["aps"]["badge"], 3);
        assert_eq!(last.payload["aps"]["alert"]["title"], "Research");
        assert_eq!(last.payload["sender"]["kind"], "agent");
        assert_eq!(last.payload["chat_title"], "Research");
        assert_eq!(last.payload["project_id"], project_id);
        assert!(last.payload["sender"]["avatar"].is_string());
        assert!(last.payload["sender"]["avatar_color"].is_string());

        // Bob reads the DM on his phone: the other device gets badge 1, the phone nothing.
        let before = sent.len();
        state.db.mark_chat_read(&bob, &dm_chat).expect("read");
        badge_refresh(&state, &bob, &device_id(&bob_dev)).await;
        let sent = state.push.captured().await;
        assert_eq!(sent.len(), before + 1);
        let badge = sent.last().expect("badge push");
        assert_eq!(badge.device_token, bob_tok2);
        assert_ne!(badge.device_token, bob_tok);
        assert_eq!(badge.payload, json!({ "aps": { "badge": 1 } }));
        assert_eq!(state.db.unread_total(&bob).expect("total"), 1);
    }

    #[test]
    fn brief_caps_at_140_chars_on_one_line() {
        assert_eq!(brief("  two\nlines  here "), "two lines here");
        let long = "x".repeat(300);
        let b = brief(&long);
        assert_eq!(b.chars().count(), BODY_MAX);
        assert!(b.ends_with('…'));
    }
}
