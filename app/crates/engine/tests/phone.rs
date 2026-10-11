//! The phone lists projects and queues a prompt on the home engine's RPC.
//!
//! This process has no iPhone simulator. The frames are the same ones the
//! desktop uses: `ArbosCall`, `WatchDocMessages`, and `QueueCommand`.

use std::time::Duration;

use zeron_engine::arbos_layout;
use zeron_engine::arbos_phone;
use zeron_engine::{Engine, EngineConfig, HarnessId};
use zeron_rpc::{connect_ws, methods};

fn config(data_dir: &std::path::Path) -> EngineConfig {
    EngineConfig {
        data_dir: data_dir.to_path_buf(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        ipc_port: 0,
        default_harness: HarnessId::Mock,
        org_id: None,
        workos_client_id: Some("client_test".into()),
    }
}

async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn phone_lists_a_project_and_queues_a_prompt() {
    let config_dir = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let remote_port = free_port().await;
    let ipc_port = free_port().await;
    unsafe {
        std::env::set_var("ARBOS_CONFIG_DIR", config_dir.path());
        std::env::set_var("ARBOS_REMOTE_PORT", remote_port.to_string());
        std::env::set_var("ARBOS_MACHINE_NAME", "local");
        std::env::remove_var("ARBOS_HOME_URL");
        std::env::remove_var("ENGY_API_KEY");
        std::env::set_var("GIT_AUTHOR_NAME", "Arbos");
        std::env::set_var("GIT_AUTHOR_EMAIL", "arbos@localhost");
        std::env::set_var("GIT_COMMITTER_NAME", "Arbos");
        std::env::set_var("GIT_COMMITTER_EMAIL", "arbos@localhost");
    }

    arbos_layout::ensure(project_dir.path(), Some("phone-demo")).expect("project layout");

    let mut engine_config = config(data_dir.path());
    engine_config.ipc_port = ipc_port;
    let daemon = tokio::spawn(Engine::new(engine_config).run());

    let url = format!("ws://127.0.0.1:{remote_port}/v1/rpc");
    let client = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(client) = arbos_phone::connect(&url).await {
                break client;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the phone port did not accept RPC");

    let projects = arbos_phone::list_projects(&client)
        .await
        .expect("project list");
    let project = projects
        .iter()
        .find(|row| row.name == "phone-demo")
        .expect("phone-demo was not listed");
    assert!(!project.main_chat_id.is_empty());
    assert_eq!(project.home, "local");
    println!("listed phone-demo");
    println!("main {}", project.main_chat_id);

    let mut main = arbos_phone::follow_main(&client, &project.main_chat_id)
        .await
        .expect("follow Main");
    let frame = tokio::time::timeout(Duration::from_secs(10), main.recv())
        .await
        .expect("Main did not send a frame")
        .expect("Main closed before a frame");
    let frame_bytes = serde_json::to_vec(&frame).unwrap_or_default();
    println!("main frame bytes {}", frame_bytes.len());

    let command_id = arbos_phone::send_prompt(
        &client,
        &project.main_chat_id,
        "phone-ping",
        &project.path,
    )
    .await
    .expect("queue prompt");
    assert!(!command_id.is_empty());
    println!("queued {command_id}");

    connect_ws(&format!("ws://127.0.0.1:{ipc_port}"))
        .await
        .expect("desktop IPC")
        .call(methods::STOP_ENGINE, serde_json::json!({}))
        .await
        .expect("stop");
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("the engine did not stop")
        .expect("engine task panicked")
        .expect("engine shutdown failed");
}
