//! `subscriptions.add` from the tool's side: the camelCase JSON the MCP
//! tool sends, the file the engine writes for each kind, and the rows the
//! Listening pill renders — the rows whose id a Stop sends back to
//! `subscriptions_remove`.
//!
//! The unit tests next to `arbos_wakes` pin each field check on a
//! `NewSubscription` struct; this file pins the wiring the three layers
//! share. `op_add` runs the same `serde_json::from_value` the
//! `subscriptions_add` op runs on its params, so a field the tool names
//! differently, or sends as the wrong type, fails here first.

use std::path::Path;

use serde_json::{Value, json};
use zeron_engine::arbos_wakes::{self, NewSubscription, SubscriptionView};

/// A scratch project: `.arbos/subscriptions` and nothing else, so the row
/// order below is only what this file adds.
fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".arbos").join("subscriptions")).unwrap();
    dir
}

/// One `subscriptions_add` call, minus the engine: the same
/// deserialization the op runs on its params, then the engine's own add.
/// The frame's other keys — `op`, `project` — are ignored on the way
/// through, exactly as the dispatcher's params reach the struct.
fn op_add(project: &Path, params: Value) -> Result<SubscriptionView, String> {
    let new: NewSubscription = serde_json::from_value(params)
        .map_err(|_| "the subscription fields were not read".to_string())?;
    arbos_wakes::add_subscription(project, &new)
}

/// Every row's id and label, in the file order the pill shows.
fn rows(project: &Path) -> Vec<(String, String)> {
    arbos_wakes::subscriptions(project)
        .into_iter()
        .map(|row| (row.id, row.label))
        .collect()
}

fn file_text(project: &Path, id: &str) -> String {
    std::fs::read_to_string(
        project
            .join(".arbos")
            .join("subscriptions")
            .join(format!("{id}.toml")),
    )
    .unwrap()
}

#[test]
fn each_kind_the_tool_sends_becomes_a_row_in_file_order() {
    let root = scratch();

    // The whole frame the tool sends, op and project included.
    let pr = op_add(
        root.path(),
        json!({
            "op": "subscriptions_add",
            "project": root.path().display().to_string(),
            "kind": "github_pr",
            "repo": "unarbos/arbos",
            "pr": 12,
            "prompt": "Say what changed in the pull request.",
        }),
    )
    .unwrap();
    assert_eq!(pr.id, "pr-12");
    assert_eq!(pr.kind, "github_pr");
    assert_eq!(pr.label, "unarbos/arbos#12");
    let pr_text = file_text(root.path(), "pr-12");
    assert!(pr_text.contains("kind = \"github_pr\""));
    assert!(pr_text.contains("repo = \"unarbos/arbos\""));
    assert!(pr_text.contains("pr = 12"));
    assert!(pr_text.contains("prompt = \"Say what changed in the pull request.\""));

    let ci = op_add(
        root.path(),
        json!({ "kind": "github_ci", "repo": "unarbos/arbos", "branch": "main" }),
    )
    .unwrap();
    assert_eq!(ci.id, "ci-main");
    assert_eq!(ci.label, "unarbos/arbos main");
    assert!(file_text(root.path(), "ci-main").contains("branch = \"main\""));

    let tick = op_add(root.path(), json!({ "kind": "timer", "everySeconds": 600 })).unwrap();
    assert_eq!(tick.id, "every-600s");
    assert_eq!(tick.label, "Every 600 seconds");
    assert!(file_text(root.path(), "every-600s").contains("every_seconds = 600"));

    let daily = op_add(
        root.path(),
        json!({ "kind": "timer", "hour": 8, "minute": 30 }),
    )
    .unwrap();
    assert_eq!(daily.id, "daily-0830");
    assert_eq!(daily.label, "Daily at 08:30");
    let daily_text = file_text(root.path(), "daily-0830");
    assert!(daily_text.contains("hour = 8"));
    assert!(daily_text.contains("minute = 30"));

    // One row per file, in file order, each carrying the id its Stop sends.
    // The pill prefixes the GitHub labels ("Pull request …", "CI on …") and
    // shows the timer labels as they are.
    assert_eq!(
        rows(root.path()),
        vec![
            ("ci-main".into(), "unarbos/arbos main".into()),
            ("daily-0830".into(), "Daily at 08:30".into()),
            ("every-600s".into(), "Every 600 seconds".into()),
            ("pr-12".into(), "unarbos/arbos#12".into()),
        ]
    );
}

#[test]
fn a_refused_call_writes_nothing_and_names_its_fields() {
    let root = scratch();
    op_add(root.path(), json!({ "kind": "timer", "everySeconds": 600 })).unwrap();

    assert_eq!(
        op_add(root.path(), json!({ "kind": "timer", "everySeconds": 600 })).unwrap_err(),
        "that subscription already exists"
    );
    // The tool names its fields in camelCase; a snake_case key is not read.
    assert_eq!(
        op_add(
            root.path(),
            json!({ "kind": "timer", "every_seconds": 600 })
        )
        .unwrap_err(),
        "timer needs everySeconds or an hour"
    );
    // A field of the wrong type fails in the op, before the engine.
    assert_eq!(
        op_add(
            root.path(),
            json!({ "kind": "github_pr", "repo": "unarbos/arbos", "pr": "12" })
        )
        .unwrap_err(),
        "the subscription fields were not read"
    );
    // The catalog offers three kinds; the engine agrees.
    assert_eq!(
        op_add(root.path(), json!({ "kind": "slack", "channel": "#arbos" })).unwrap_err(),
        "kind must be github_pr, github_ci, or timer"
    );
    assert_eq!(
        rows(root.path()),
        vec![("every-600s".into(), "Every 600 seconds".into())],
        "only the accepted call wrote a file"
    );
}

#[test]
fn stop_removes_only_the_row_it_names() {
    let root = scratch();
    op_add(
        root.path(),
        json!({ "kind": "github_pr", "repo": "unarbos/arbos", "pr": 12 }),
    )
    .unwrap();
    op_add(
        root.path(),
        json!({ "kind": "github_ci", "repo": "unarbos/arbos", "branch": "main" }),
    )
    .unwrap();

    // The pill's Stop sends the row's id to subscriptions_remove.
    arbos_wakes::remove_subscription(root.path(), "pr-12").unwrap();
    assert!(
        !root
            .path()
            .join(".arbos")
            .join("subscriptions")
            .join("pr-12.toml")
            .is_file(),
        "Stop deletes the file subscriptions.add wrote"
    );
    assert_eq!(
        rows(root.path()),
        vec![("ci-main".into(), "unarbos/arbos main".into())]
    );
    assert_eq!(
        arbos_wakes::remove_subscription(root.path(), "pr-12").unwrap_err(),
        "no such subscription"
    );
}

#[test]
fn a_long_prompt_is_clipped_to_three_hundred_characters() {
    let root = scratch();
    op_add(
        root.path(),
        json!({
            "kind": "timer",
            "everySeconds": 60,
            "name": "long prompt",
            "prompt": "p".repeat(350),
        }),
    )
    .unwrap();
    let text = file_text(root.path(), "long-prompt");
    assert!(text.contains(&format!("prompt = \"{}\"", "p".repeat(300))));
    assert!(
        !text.contains(&"p".repeat(301)),
        "the wake prompt is one sentence, not the whole text"
    );
}
