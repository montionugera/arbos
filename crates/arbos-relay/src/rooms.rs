//! Sync rooms (crdt-sync §7, §9): `WS /room/{projectId}/{chatId}`. The relay
//! stores and forwards opaque rows; it never parses Loro bytes.

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::broadcast;

use arbos_core::wire::{self, Frame, FrameType, PROJECT_DOC_CHAT_ID};

use crate::auth::Principal;
use crate::boxes;
use crate::people;
use crate::state::{AppState, RoomBroadcast};

const ROWS_BATCH: i64 = 256;

pub async fn ws_room(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    principal: Principal,
    Path((project_id, chat_id)): Path<(String, String)>,
) -> Response {
    // People chats live under the reserved project slot `people`; their ACL is membership.
    if !state.allows(&principal, &acl_key(&project_id, &chat_id)) {
        return (StatusCode::FORBIDDEN, "not your project").into_response();
    }
    if project_id != people::PEOPLE_PROJECT {
        if state.db.project(&project_id).ok().flatten().is_none() {
            return (StatusCode::NOT_FOUND, "no such project").into_response();
        }
        if chat_id != PROJECT_DOC_CHAT_ID
            && !state.db.chat_exists(&project_id, &chat_id).unwrap_or(false)
        {
            return (StatusCode::NOT_FOUND, "no such chat").into_response();
        }
    }
    let room_name = wire::room_name(&project_id, &chat_id);
    ws.max_message_size(wire::ROW_MAX + 8192)
        .on_upgrade(move |socket| run(socket, state, principal, project_id, room_name))
}

/// What `AppState::allows` checks for a room: the project id, or `people:<chat>` for a people chat.
fn acl_key(project_id: &str, chat_id: &str) -> String {
    if project_id == people::PEOPLE_PROJECT {
        format!("{}{chat_id}", people::PEOPLE_ACL_PREFIX)
    } else {
        project_id.to_owned()
    }
}

fn frame(ty: FrameType, header: serde_json::Value, payload: Bytes) -> Message {
    Message::Binary(Frame::new(ty, header, payload).encode())
}

fn error_frame(code: &str, message: &str) -> Message {
    frame(
        FrameType::Error,
        json!({ "code": code, "message": message }),
        Bytes::new(),
    )
}

async fn run(
    socket: WebSocket,
    state: AppState,
    principal: Principal,
    project_id: String,
    room_name: String,
) {
    let conn_id = state.conn_seq.fetch_add(1, Ordering::Relaxed) + 1;
    let room = state.room(&room_name).await;
    let mut rx = room.tx.subscribe();
    let (mut sink, mut stream) = socket.split();
    let is_box = principal.is_box();
    // Rows from the box carry the box's device id (`box:<sandbox>` / `box:local-<user>`), the same
    // id agentd writes into the doc, so clients see one stable author across restarts.
    let mut device = match (&principal, principal.user_id()) {
        (Principal::Box { .. }, Some(u)) => state
            .db
            .user_box(u)
            .ok()
            .flatten()
            .map(|b| b.device_id)
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| principal.device_name()),
        _ => principal.device_name(),
    };
    // Box sockets count per user box (one box serves every project of the user).
    let box_user = principal.user_id().map(str::to_owned).unwrap_or_default();
    if is_box {
        boxes::box_connected(&state, &box_user).await;
    }
    tracing::debug!(%room_name, %device, conn_id, "room join");

    loop {
        tokio::select! {
            incoming = stream.next() => {
                let Some(Ok(msg)) = incoming else { break };
                let data = match msg {
                    Message::Binary(b) => b,
                    Message::Close(_) => break,
                    _ => continue,
                };
                let f = match Frame::decode(data) {
                    Ok(Some(f)) => f,
                    Ok(None) => continue,
                    Err(e) => {
                        let _ = sink.send(error_frame("bad_frame", &e.to_string())).await;
                        continue;
                    }
                };
                if let Err(e) = handle_frame(&state, &room, &project_id, &room_name, conn_id, &mut device, &principal, f, &mut sink).await {
                    tracing::warn!(%room_name, error = %e, "room frame error");
                    let _ = sink.send(error_frame("internal", &e.to_string())).await;
                }
            }
            out = rx.recv() => {
                match out {
                    Ok(b) => {
                        if b.from_conn != conn_id && sink.send(Message::Binary(b.bytes)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let _ = sink.send(error_frame("lagging", "re-sync with ROWS_REQ")).await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    tracing::debug!(%room_name, %device, conn_id, "room leave");
    if let Principal::Device { id, .. } = &principal {
        if let Some((_, chat_id)) = room_name.rsplit_once('/') {
            crate::notify::clear_focus(&state, id, chat_id).await;
        }
    }
    if is_box {
        boxes::box_disconnected(&state, &box_user).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_frame(
    state: &AppState,
    room: &crate::state::Room,
    project_id: &str,
    room_name: &str,
    conn_id: u64,
    device: &mut String,
    principal: &Principal,
    f: Frame,
    sink: &mut futures::stream::SplitSink<WebSocket, Message>,
) -> anyhow::Result<()> {
    let is_box = principal.is_box();
    match f.ty {
        FrameType::Hello => {
            let h: wire::Hello = serde_json::from_value(f.header)?;
            if h.proto != arbos_core::PROTO_VERSION {
                sink.send(error_frame("proto", "unsupported protocol version"))
                    .await?;
                return Ok(());
            }
            if !h.device.is_empty() {
                *device = h.device.clone();
            }
            send_state(state, project_id, room_name, sink).await?;
        }
        FrameType::RowsReq => {
            let r: wire::RowsReq = serde_json::from_value(f.header)?;
            let exclude = if r.exclude_own {
                Some(device.as_str())
            } else {
                None
            };
            let mut after = r.after;
            loop {
                let rows = state.db.rows_after(room_name, after, exclude, ROWS_BATCH)?;
                let n = rows.len() as i64;
                for row in rows {
                    after = row.seq;
                    sink.send(frame(
                        FrameType::Row,
                        json!({ "seq": row.seq, "device": row.device, "batchId": row.batch_id }),
                        Bytes::from(row.bytes),
                    ))
                    .await?;
                }
                if n < ROWS_BATCH {
                    break;
                }
            }
            let st = state.db.room_state(room_name)?;
            sink.send(frame(
                FrameType::RowsDone,
                json!({ "headSeq": st.head }),
                Bytes::new(),
            ))
            .await?;
        }
        FrameType::Push => {
            let p: wire::PushHeader = serde_json::from_value(f.header)?;
            if f.payload.len() > wire::ROW_MAX {
                sink.send(error_frame("too_large", "row exceeds 1 MiB"))
                    .await?;
                return Ok(());
            }
            let mut wake_owners = Vec::new();
            if let Some(chat_id) = room_name.strip_prefix(&format!("{}/", people::PEOPLE_PROJECT)) {
                match people::on_room_push(state, principal, chat_id) {
                    Ok(owners) => wake_owners = owners,
                    Err(r) => {
                        sink.send(error_frame(r.code(), r.message())).await?;
                        return Ok(());
                    }
                }
            }
            let (seq, dup) = state
                .db
                .append_row(room_name, device, &p.batch_id, &f.payload)?;
            sink.send(frame(
                FrameType::Ack,
                json!({ "batchId": p.batch_id, "seq": seq, "dup": dup }),
                Bytes::new(),
            ))
            .await?;
            if !dup {
                let row = Frame::new(
                    FrameType::Row,
                    json!({ "seq": seq, "device": device, "batchId": p.batch_id }),
                    f.payload,
                )
                .encode();
                let _ = room.tx.send(RoomBroadcast {
                    from_conn: conn_id,
                    bytes: row,
                });
                if !is_box {
                    // A person wrote without announcing a message (older apps): the fallback
                    // notifies the other members by name only.
                    if let Principal::Device { user_id, .. } = principal {
                        let chat_id = room_name
                            .rsplit_once('/')
                            .map(|(_, c)| c.to_owned())
                            .unwrap_or_default();
                        let chat = if project_id == people::PEOPLE_PROJECT {
                            Some(crate::notify::ChatRef::people(&chat_id))
                        } else if chat_id != PROJECT_DOC_CHAT_ID {
                            Some(crate::notify::ChatRef::project(project_id, &chat_id))
                        } else {
                            None
                        };
                        if let Some(chat) = chat {
                            crate::notify::on_row_from_device(state, chat, user_id, seq);
                        }
                    }
                    // Wake obligation: a client row for a sleeping box (crdt-sync §8.4 step 1–2).
                    if let Some(u) = state.user_of(project_id) {
                        state
                            .last_client_push
                            .lock()
                            .await
                            .insert(u, arbos_core::ids::now_ms());
                    }
                    boxes::ensure_awake_project(state, project_id, "send").await;
                    // A people chat with shared agents wakes every agent owner's box.
                    for owner in wake_owners {
                        state
                            .last_client_push
                            .lock()
                            .await
                            .insert(owner.clone(), arbos_core::ids::now_ms());
                        boxes::ensure_awake(state, &owner, "send").await;
                    }
                }
            }
        }
        FrameType::Presence => {
            // `focused_chat` (crdt-sync §7.5): the chat on this device's screen; its message
            // notifications are suppressed while the presence stays fresh.
            if let Principal::Device { id, .. } = principal {
                let focused = f.header.get("focused_chat").and_then(|v| v.as_str());
                crate::notify::set_focus(state, id, focused).await;
            }
            let out = Frame::new(FrameType::Presence, f.header, f.payload).encode();
            let _ = room.tx.send(RoomBroadcast {
                from_conn: conn_id,
                bytes: out,
            });
        }
        FrameType::Probe => {
            let st = state.db.room_state(room_name)?;
            sink.send(frame(
                FrameType::ProbeOk,
                json!({ "headSeq": st.head }),
                Bytes::new(),
            ))
            .await?;
        }
        FrameType::Wake => {
            let w: wire::WakeHeader =
                serde_json::from_value(f.header).unwrap_or(wire::WakeHeader {
                    reason: "open".into(),
                });
            boxes::ensure_awake_project(state, project_id, &w.reason).await;
            let (st, since) = state.project_box_state(project_id).await;
            sink.send(frame(
                FrameType::Status,
                json!({ "box": st, "since": since }),
                Bytes::new(),
            ))
            .await?;
        }
        FrameType::State
        | FrameType::Row
        | FrameType::RowsDone
        | FrameType::Ack
        | FrameType::ProbeOk
        | FrameType::Error
        | FrameType::Status => {
            // Server-to-client types; a client sending them is ignored (never fatal).
        }
    }
    Ok(())
}

async fn send_state(
    state: &AppState,
    project_id: &str,
    room_name: &str,
    sink: &mut futures::stream::SplitSink<WebSocket, Message>,
) -> anyhow::Result<()> {
    let st = state.db.room_state(room_name)?;
    let (box_state, _) = state.project_box_state(project_id).await;
    let header = wire::State {
        head_seq: st.head,
        seq_floor: st.floor,
        checkpoint_seq: st.checkpoint_seq,
        checkpoint_size: st.checkpoint_size,
        row_count: st.row_count,
        row_bytes: st.row_bytes,
        box_state,
    };
    sink.send(frame(
        FrameType::State,
        serde_json::to_value(header)?,
        Bytes::from(st.checkpoint_frontier),
    ))
    .await?;
    Ok(())
}

/// `GET /room/{project}/{chat}/checkpoint` — full checkpoint bytes (clients load it, then rows after `seq`).
pub async fn get_checkpoint(
    State(state): State<AppState>,
    principal: Principal,
    Path((project_id, chat_id)): Path<(String, String)>,
) -> Response {
    if !state.allows(&principal, &acl_key(&project_id, &chat_id)) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let room_name = wire::room_name(&project_id, &chat_id);
    match state.db.checkpoint_bytes(&room_name) {
        Ok(Some((seq, bytes))) => {
            ([("x-arbos-checkpoint-seq", seq.to_string())], bytes).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `POST /room/{project}/{chat}/checkpoint?seqCovered=` — box only; body = snapshot; header `x-arbos-frontier` = base64 VV.
pub async fn post_checkpoint(
    State(state): State<AppState>,
    principal: Principal,
    Path((project_id, chat_id)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    if !state.allows(&principal, &acl_key(&project_id, &chat_id))
        || !matches!(principal, Principal::Box { .. } | Principal::Admin)
    {
        return (StatusCode::FORBIDDEN, "box only").into_response();
    }
    let seq_covered: i64 = q
        .get("seqCovered")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let frontier = headers
        .get("x-arbos-frontier")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| hex::decode(s).ok())
        .unwrap_or_default();
    let room_name = wire::room_name(&project_id, &chat_id);
    match state
        .db
        .put_checkpoint(&room_name, seq_covered, &frontier, &body)
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::CONFLICT, "seqCovered below floor").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `GET /room/{project}/{chat}/stats`.
pub async fn room_stats(
    State(state): State<AppState>,
    principal: Principal,
    Path((project_id, chat_id)): Path<(String, String)>,
) -> Response {
    if !state.allows(&principal, &acl_key(&project_id, &chat_id)) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let room_name = wire::room_name(&project_id, &chat_id);
    match state.db.room_state(&room_name) {
        Ok(st) => axum::Json(json!({
            "headSeq": st.head, "seqFloor": st.floor, "checkpointSeq": st.checkpoint_seq,
            "checkpointSize": st.checkpoint_size, "rowCount": st.row_count, "rowBytes": st.row_bytes,
        }))
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Grace-period sweeper: a box socket that stays gone marks the project asleep (crdt-sync §8.5).
pub async fn spawn_sweeper(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            boxes::sweep_box_liveness(&state).await;
        }
    });
}
