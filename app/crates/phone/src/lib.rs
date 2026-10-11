//! Phone client for the home engine.
//!
//! The frames are the desktop's RPC. `ArbosCall` with `op` `projects` lists
//! the rail. `WatchDocMessages` follows Main. `QueueCommand` sends a prompt.

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use uuid::Uuid;
use zeron_rpc::{RpcClient, methods};

/// One project the home engine is willing to show the phone.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PhoneProject {
    pub name: String,
    pub path: String,
    pub home: String,
    pub main_chat_id: String,
}

/// Dial the engine websocket. `url` is `ws://` or `wss://`, including `/v1/rpc`
/// when the call goes through the home tunnel.
pub async fn connect(url: &str) -> Result<RpcClient, String> {
    zeron_rpc::connect_ws(url)
        .await
        .map_err(|err| format!("the engine did not accept the phone: {err}"))
}

/// Projects on the engine this client is connected to.
pub async fn list_projects(client: &RpcClient) -> Result<Vec<PhoneProject>, String> {
    let value = client
        .call(methods::ARBOS_CALL, json!({ "op": "projects" }))
        .await
        .map_err(|err| format!("the project list was not returned: {err}"))?;
    let rows = value
        .get("projects")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    serde_json::from_value(rows).map_err(|_| "the project list was not readable".to_string())
}

/// Live frames for Main. The first frame is the transcript already on the engine.
pub async fn follow_main(
    client: &RpcClient,
    chat_id: &str,
) -> Result<mpsc::Receiver<Value>, String> {
    client
        .subscribe(
            methods::WATCH_DOC_MESSAGES,
            json!({ "chatId": chat_id }),
        )
        .await
        .map_err(|err| format!("Main could not be followed: {err}"))
}

/// Queue a prompt on Main. Returns the engine's command id.
pub async fn send_prompt(
    client: &RpcClient,
    chat_id: &str,
    prompt: &str,
    cwd: &str,
) -> Result<String, String> {
    let value = client
        .call(
            methods::QUEUE_COMMAND,
            json!({
                "chatId": chat_id,
                "command": {
                    "kind": "run",
                    "messageId": Uuid::new_v4().to_string(),
                    "request": {
                        "prompt": prompt,
                        "harness": "arbos-fx",
                        "model": "glm-5.3",
                        "reasoning": null,
                        "cwd": cwd,
                        "sandbox": "danger-full-access",
                        "autoApprove": true,
                        "resume": null
                    }
                }
            }),
        )
        .await
        .map_err(|err| format!("the prompt was not queued: {err}"))?;
    value
        .get("commandId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "the prompt was not queued".to_string())
}
