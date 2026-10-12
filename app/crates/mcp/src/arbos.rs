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
                "Start a worker and return at once. machine is local or the name of a registered install. The worker runs the brief in its own chat.",
                json!({
                    "type": "object",
                    "properties": {
                        "brief": { "type": "string", "description": "What the worker should do." },
                        "name": { "type": "string", "description": "Short worker name. A slug is used when this is omitted." },
                        "model": { "type": "string", "description": "Model id. Defaults to glm-5.3." },
                        "readonly": { "type": "boolean", "description": "Run the worker read-only." },
                        "worktree": { "type": "boolean", "description": "Not available. Must be omitted or false." },
                        "machine": { "type": "string", "description": "local, or the name of a registered install." }
                    },
                    "required": ["brief"]
                }),
            ),
            tool(
                "agents.send",
                "Steer a running worker, or send a new prompt when it is idle. rename changes the worker's display name at once. model applies from the worker's next turn. text is optional when rename or model is given.",
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "text": { "type": "string", "description": "The message. Steers a running turn, else becomes the next prompt." },
                        "rename": { "type": "string", "description": "New display name for the worker's tab." },
                        "model": { "type": "string", "description": "Model id for the worker's next turn." }
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
                "Mark the goal complete. Call this only when the objective is proven. Wakes stop.",
                no_args(),
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
                "context.usage",
                "How full your context window is: percent used, tokens per bucket (system prompt, tools, rules, skills, summarized conversation, conversation), and the input tokens of recent turns.",
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
}
