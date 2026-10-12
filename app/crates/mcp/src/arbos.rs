//! The `arbos` tool catalog. Main sees `agents.*`, `status.step`, and
//! `context.*`. A worker sees `context.*` only, so a worker cannot spawn
//! another worker.

use serde_json::{Value, json};

use crate::tools::ToolDef;
use crate::zeron::Zeron;

const ARBOS_INSTRUCTIONS: &str = "\
This server is the Arbos project. \
agents.spawn starts a worker and returns at once. \
machine is local or the name of a registered install. \
agents.transcript returns a worker's last few turns, never the whole log. \
agents.send steers a running worker, renames it, or changes its model for the next turn. \
goal.show, goal.schedule, goal.pause, goal.resume, goal.complete, and goal.clear manage the project's durable goal. \
preferences.write appends one dated line to the person's preferences, shared by every project. \
status.step sets the live step the person sees: six words at most, starting with a verb. \
subscriptions.add subscribes Main to a GitHub pull request, a CI branch, or a timer; the Listening pill shows the new row. \
context.usage reports how full the context window is. \
context.read and context.write use paths under the project context directory, \
such as internal/note.md or docs/report.md. status.md is read-only; the engine rewrites it after every turn. \
A worker's file under docs/ or internal/ must start with its own arbos agentId frontmatter. \
Workers do not have the agents tools.";

pub fn instructions() -> &'static str {
    ARBOS_INSTRUCTIONS
}

pub fn catalog(profile: &str) -> Vec<ToolDef> {
    let mut tools = Vec::new();
    if profile == "coordinator" {
        tools.extend([
            tool(
                "agents.spawn",
                "Start a worker and return at once. machine is local or the name of a registered install. The worker runs the brief in its own chat. model must be a model the provider offers; an unknown model is refused and the refusal lists the models on offer, so pick from that list.",
                json!({
                    "type": "object",
                    "properties": {
                        "brief": { "type": "string", "description": "What the worker should do." },
                        "name": { "type": "string", "description": "Short worker name. A slug is used when this is omitted." },
                        "model": { "type": "string", "description": "Model id, exactly as the provider lists it. Defaults to glm-5.3. Do not guess a slug; pick one from the list you are given." },
                        "readonly": { "type": "boolean", "description": "Run the worker read-only." },
                        "worktree": { "type": "boolean", "description": "Not available. Must be omitted or false." },
                        "machine": { "type": "string", "description": "local, or the name of a registered install." }
                    },
                    "required": ["brief"]
                }),
            ),
            tool(
                "agents.send",
                "Steer a running worker, or send a new prompt when it is idle. rename changes the worker's display name at once. model applies from the worker's next turn and must be a model the provider offers; an unknown model is refused and the refusal lists the models on offer, so pick from that list. text is optional when rename or model is given.",
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "text": { "type": "string", "description": "The message. Steers a running turn, else becomes the next prompt." },
                        "message": { "type": "string", "description": "Same as text." },
                        "rename": { "type": "string", "description": "New display name for the worker's tab." },
                        "model": { "type": "string", "description": "Model id for the worker's next turn, exactly as the provider lists it. Do not guess a slug; pick one from the list you are given." }
                    },
                    "required": ["name"]
                }),
            ),
            tool(
                "agents.list",
                "List the agents in this project.",
                json!({ "type": "object", "properties": {} }),
            ),
            tool(
                "agents.result",
                "Return a worker's last reply and the path of its agent file.",
                json!({
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                    "required": ["name"]
                }),
            ),
            tool(
                "agents.transcript",
                "Return the last few turns of a worker's conversation: what it was asked, what it replied, which tools it called, and whether the turn ended. Bounded to tail turns, at most 50.",
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "tail": { "type": "integer", "description": "How many of the last turns to return. Defaults to 10. Clamped to 1 through 50." }
                    },
                    "required": ["name"]
                }),
            ),
            tool(
                "goal.show",
                "The project's goal: objective, state (active, paused, complete), elapsed seconds, last wake, and the wake interval. Null when there is no goal.",
                no_args(),
            ),
            tool(
                "goal.schedule",
                "Choose how many seconds to wait before the next goal wake. Clamped to 30 through 86400. Returns the goal.",
                json!({
                    "type": "object",
                    "properties": {
                        "seconds": { "type": "integer", "description": "Seconds until the next wake, and between later wakes." }
                    },
                    "required": ["seconds"]
                }),
            ),
            tool(
                "goal.pause",
                "Pause the goal. Wakes stop until goal.resume. The clock keeps running.",
                no_args(),
            ),
            tool(
                "goal.resume",
                "Resume a paused goal. Wakes start again on the interval.",
                no_args(),
            ),
            tool(
                "goal.complete",
                "Mark the goal complete. Call this only when every step of the goal is checked and you can say why. It is refused while a step is open, and the refusal names the open steps. Wakes stop.",
                json!({
                    "type": "object",
                    "properties": {
                        "reason": { "type": "string", "description": "Why the goal is complete, in at least one sentence." }
                    },
                    "required": ["reason"]
                }),
            ),
            tool(
                "goal.clear",
                "Remove the goal. Wakes stop. The checklist and GOALS.md stay.",
                no_args(),
            ),
            tool(
                "preferences.write",
                "Save one dated line about how the person wants work done, across every project. Use it only when the person states a preference, corrects you, or repeats the same request.",
                json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "One sentence. Newlines are joined." }
                    },
                    "required": ["text"]
                }),
            ),
            tool(
                "status.step",
                "Set the live step the person sees while you work. Six words at most, starting with a verb, such as Checking the gate. It clears when the turn ends.",
                json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "The step, six words at most, starting with a verb." }
                    },
                    "required": ["text"]
                }),
            ),
            tool(
                "subscriptions.add",
                "Subscribe Main to a GitHub pull request, a CI branch, or a timer. The Listening pill shows the new row, and the engine wakes Main when it fires. kind is github_pr with repo owner/name and pr, github_ci with repo and branch, or timer with everySeconds, or hour and minute for a daily time. The same subscription twice is refused.",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "description": "github_pr, github_ci, or timer." },
                        "repo": { "type": "string", "description": "owner/name, for github_pr and github_ci." },
                        "pr": { "type": "integer", "description": "The pull request number, for github_pr." },
                        "branch": { "type": "string", "description": "The branch, for github_ci." },
                        "everySeconds": { "type": "integer", "description": "Seconds between wakes, clamped 30 through 86400. A timer without this uses hour and minute." },
                        "hour": { "type": "integer", "description": "0 through 23, for a daily timer." },
                        "minute": { "type": "integer", "description": "0 through 59, default 0." },
                        "name": { "type": "string", "description": "The row id, slugged from this. A default per kind when omitted." },
                        "prompt": { "type": "string", "description": "One sentence each wake ends with, at most 300 characters." }
                    },
                    "required": ["kind"]
                }),
            ),
            tool(
                "context.usage",
                "How full your context window is: percent used, tokens and percent of the window per bucket (system prompt, tools, rules, skills, summarized conversation, conversation), whether the buckets are fx's own measurement, and the tokens of recent turns.",
                no_args(),
            ),
        ]);
    }
    tools.extend([
        tool(
            "context.read",
            "Read one file under the project context directory.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to context, such as status.md or research/note.md." }
                },
                "required": ["path"]
            }),
        ),
        tool(
            "context.write",
            "Write one file under the project context directory. The engine writes status.md and archived.md itself after every turn.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to context." },
                    "text": { "type": "string", "description": "The full file text." }
                },
                "required": ["path", "text"]
            }),
        ),
    ]);
    tools
}

fn no_args() -> Value {
    json!({ "type": "object", "properties": {} })
}

/// Tools only Main has. A worker's call is refused before it reaches the engine.
fn main_only(name: &str) -> bool {
    name.starts_with("agents.")
        || name.starts_with("goal.")
        || name.starts_with("preferences.")
        || name.starts_with("status.")
        || name.starts_with("subscriptions.")
        || name == "context.usage"
}

pub async fn call(zeron: &Zeron, name: &str, args: Value) -> Result<Value, String> {
    let profile = std::env::var("ARBOS_MCP").unwrap_or_default();
    if profile != "coordinator" && main_only(name) {
        return Err("workers cannot call agents, goal, preferences, or status tools".into());
    }
    if !catalog(&profile).iter().any(|tool| tool.name == name) {
        return Err(format!("unknown tool: {name}"));
    }
    if forces(&args) {
        return Err("only a person can set force; give a reason instead".into());
    }
    let project = std::env::var("ARBOS_PROJECT").map_err(|_| "ARBOS_PROJECT is missing".to_string())?;
    let origin = std::env::var("ZERON_CHAT_ID").unwrap_or_default();
    let mut params = match args {
        Value::Object(map) => Value::Object(map),
        _ => json!({}),
    };
    if let Some(object) = params.as_object_mut() {
        object.insert("op".into(), json!(op_for(name)));
        object.insert("project".into(), json!(project));
        object.insert("originChatId".into(), json!(origin));
    }
    zeron
        .call(zeron_rpc::methods::ARBOS_CALL, params)
        .await
        .map_err(|err| err.to_string())
}

/// `force` is the person's flag from the UI. No agent call carries it.
fn forces(args: &Value) -> bool {
    args.get("force").is_some_and(|force| !force.is_null())
}

fn op_for(name: &str) -> &'static str {
    match name {
        "agents.spawn" => "spawn",
        "agents.send" => "send",
        "agents.list" => "list",
        "agents.result" => "result",
        "agents.transcript" => "transcript",
        "goal.show" => "goal_show",
        "goal.schedule" => "goal_schedule",
        "goal.pause" => "goal_pause",
        "goal.resume" => "goal_resume",
        "goal.complete" => "goal_complete",
        "goal.clear" => "goal_clear",
        "preferences.write" => "preferences_write",
        "status.step" => "step",
        "subscriptions.add" => "subscriptions_add",
        "context.usage" => "context_usage",
        "context.read" => "read",
        "context.write" => "write",
        _ => "",
    }
}

fn tool(name: &'static str, description: &'static str, input_schema: Value) -> ToolDef {
    ToolDef {
        name,
        description,
        input_schema,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_sees_every_tool_once_and_a_worker_sees_context_only() {
        let main: Vec<&str> = catalog("coordinator").iter().map(|tool| tool.name).collect();
        let mut sorted = main.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), main.len(), "a tool is listed twice: {main:?}");
        for name in [
            "agents.spawn",
            "agents.send",
            "agents.list",
            "agents.result",
            "agents.transcript",
            "goal.show",
            "goal.schedule",
            "goal.pause",
            "goal.resume",
            "goal.complete",
            "goal.clear",
            "preferences.write",
            "status.step",
            "subscriptions.add",
            "context.usage",
            "context.read",
            "context.write",
        ] {
            assert!(main.contains(&name), "{name} is missing from Main's catalog");
            assert!(!op_for(name).is_empty(), "{name} has no op");
        }
        let worker: Vec<&str> = catalog("worker").iter().map(|tool| tool.name).collect();
        assert_eq!(worker, vec!["context.read", "context.write"]);
        for name in &main {
            assert_eq!(
                main_only(name),
                !worker.contains(name),
                "{name} is gated wrong"
            );
        }
    }

    #[test]
    fn goal_complete_needs_a_reason_and_never_a_force_flag() {
        let complete = catalog("coordinator")
            .into_iter()
            .find(|tool| tool.name == "goal.complete")
            .unwrap();
        assert_eq!(complete.input_schema["required"], json!(["reason"]));
        assert!(complete.input_schema["properties"].get("force").is_none());
        assert!(complete.description.contains("every step of the goal is checked"));
        assert!(forces(&json!({ "force": true })));
        assert!(forces(&json!({ "force": false })));
        assert!(!forces(&json!({ "reason": "The gate is green and both reports are in." })));
        assert!(!forces(&json!({})));
    }

    #[test]
    fn no_tool_takes_a_secret_so_a_model_is_never_handed_one() {
        for profile in ["coordinator", "worker"] {
            let tools = catalog(profile);
            assert!(!tools.is_empty());
            for tool in tools {
                assert_ne!(tool.name, "byok", "{profile} can save a key");
                assert_ne!(
                    op_for(tool.name),
                    "byok",
                    "{profile}: {} saves a key",
                    tool.name
                );
                let properties = tool.input_schema["properties"]
                    .as_object()
                    .unwrap_or_else(|| panic!("{}: properties is not an object", tool.name));
                for property in ["secret", "token", "apiKey"] {
                    assert!(
                        !properties.contains_key(property),
                        "{}: takes {property}",
                        tool.name
                    );
                }
            }
        }
    }

    #[test]
    fn subscriptions_add_needs_a_kind_and_names_each_field() {
        let add = catalog("coordinator")
            .into_iter()
            .find(|tool| tool.name == "subscriptions.add")
            .unwrap();
        assert_eq!(add.input_schema["required"], json!(["kind"]));
        for field in [
            "repo",
            "pr",
            "branch",
            "everySeconds",
            "hour",
            "minute",
            "name",
            "prompt",
        ] {
            assert!(
                add.input_schema["properties"].get(field).is_some(),
                "{field} is missing"
            );
        }
        assert!(add.input_schema["properties"].get("force").is_none());
        assert_eq!(op_for("subscriptions.add"), "subscriptions_add");
        assert!(
            main_only("subscriptions.add"),
            "a worker cannot subscribe for Main"
        );
    }
}
