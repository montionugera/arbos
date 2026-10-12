//! The `arbos` tool catalog. Main sees `agents.*` and `context.*`.
//! A worker sees `context.*` only, so a worker cannot spawn another worker.

use serde_json::{Value, json};

use crate::tools::ToolDef;
use crate::zeron::Zeron;

const ARBOS_INSTRUCTIONS: &str = "\
This server is the Arbos project. \
agents.spawn starts a worker and returns at once. \
machine is local or the name of a registered install. \
agents.transcript returns a worker's last few turns, never the whole log. \
context.read and context.write use paths under the project context directory, \
such as internal/note.md or status.md. \
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
                "Steer a running worker, or send a new prompt when it is idle.",
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "text": { "type": "string" }
                    },
                    "required": ["name", "text"]
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
            "Write one file under the project context directory. Writing status.md updates the checklist.",
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

pub async fn call(zeron: &Zeron, name: &str, args: Value) -> Result<Value, String> {
    let profile = std::env::var("ARBOS_MCP").unwrap_or_default();
    if profile != "coordinator" && name.starts_with("agents.") {
        return Err("workers cannot call agents tools".into());
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
