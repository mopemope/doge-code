//! Translate the seven scoped tools to daemon calls.

use std::collections::HashMap;
use std::time::Duration;

use dgc_client::{Client, ClientError};
use dgc_daemon::paths::DaemonPaths;
use dgc_protocol::event::{AgentEvent, EventKind};
use dgc_protocol::ids::AgentId;
use dgc_protocol::methods::{
    AGENT_ARCHIVE, AGENT_CREATE, AGENT_EVENTS, AGENT_GET, AGENT_INTERRUPT, AGENT_LIST, AGENT_SEND,
    AgentCreateParams, AgentEventsParams, AgentEventsQuery, AgentIdParams, AgentListParams,
    AgentSendParams, MAX_EVENTS_LIMIT, NOTIFY_EVENT, NOTIFY_LAGGED, SubscribeParams,
};
use dgc_protocol::model::{Agent, AgentState, ProviderKind, SendBehavior};
use dgc_protocol::rpc::{INVALID_PARAMS, NOT_FOUND, Notification, RpcError, VERSION_MISMATCH};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout, timeout_at};

use super::rpc;
use crate::commands;
use crate::commands::agent::default_title;

const MAX_AGENT_DEPTH: u32 = 8;
const DEFAULT_WAIT_SECS: u64 = 60;
const MAX_WAIT_SECS: u64 = 300;
const MAX_ANSWER_BYTES: usize = 16 * 1024;
const MAX_ANSWER_PAGES: usize = 4;
const DAEMON_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy)]
enum Tool {
    Create,
    Send,
    Get,
    Wait,
    List,
    Interrupt,
    Archive,
}

impl Tool {
    const ALL: [Self; 7] = [
        Self::Create,
        Self::Send,
        Self::Get,
        Self::Wait,
        Self::List,
        Self::Interrupt,
        Self::Archive,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Create => "create_agent",
            Self::Send => "send_agent",
            Self::Get => "get_agent",
            Self::Wait => "wait_agent",
            Self::List => "list_agents",
            Self::Interrupt => "interrupt_agent",
            Self::Archive => "archive_agent",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Create => "Start a child agent in the current agent's workspace.",
            Self::Send => "Send a message to a descendant agent.",
            Self::Get => "Read a descendant agent's state and last completed answer.",
            Self::Wait => {
                "Wait for a descendant to become idle, stop, fail, be archived, or need permission."
            }
            Self::List => "List descendant agents.",
            Self::Interrupt => "Interrupt a descendant agent's current turn.",
            Self::Archive => "Archive a descendant agent.",
        }
    }

    fn schema(self) -> Value {
        let string = json!({"type": "string"});
        let (properties, required): (Value, &[&str]) = match self {
            Self::Create => (
                json!({
                    "prompt": string, "title": string,
                    "provider": {"type": "string", "enum": ["claude", "codex"]},
                    "model": string, "mode": string,
                    "mcp": {"type": "boolean", "default": false}
                }),
                &["prompt"],
            ),
            Self::Send => (
                json!({
                    "agent_id": string, "text": string,
                    "behavior": {
                        "type": "string", "enum": ["queue", "steer", "interrupt"], "default": "queue"
                    }
                }),
                &["agent_id", "text"],
            ),
            Self::Wait => (
                json!({
                    "agent_id": string,
                    "timeout_secs": {
                        "type": "integer", "minimum": 0, "default": DEFAULT_WAIT_SECS,
                        "maximum": MAX_WAIT_SECS
                    }
                }),
                &["agent_id"],
            ),
            Self::List => (json!({}), &[]),
            Self::Get | Self::Interrupt | Self::Archive => {
                (json!({"agent_id": string}), &["agent_id"])
            }
        };
        json!({
            "type": "object", "properties": properties, "required": required,
            "additionalProperties": false
        })
    }

    fn validate(self, args: &Value) -> Result<(), ToolError> {
        let args = args.as_object().ok_or(ToolError::InvalidArguments)?;
        let schema = self.schema();
        let required = schema["required"].as_array().into_iter().flatten();
        if required
            .filter_map(Value::as_str)
            .any(|key| !args.contains_key(key))
        {
            return Err(ToolError::InvalidArguments);
        }
        for (key, value) in args {
            let kind = &schema["properties"][key]["type"];
            let valid = match kind.as_str() {
                Some("string") => value.is_string(),
                Some("boolean") => value.is_boolean(),
                Some("integer") => value.is_u64(),
                _ => false,
            };
            if !valid {
                return Err(ToolError::InvalidArguments);
            }
        }
        Ok(())
    }
}

enum ToolError {
    InvalidArguments,
    NotDescendant,
    DaemonPaths,
    DaemonTimeout,
    Daemon(ClientError),
}

impl From<ClientError> for ToolError {
    fn from(error: ClientError) -> Self {
        Self::Daemon(error)
    }
}

impl ToolError {
    fn body(self) -> Value {
        // Daemon error messages and deserialization errors can contain user data.
        let kind = match self {
            Self::InvalidArguments => "invalid_arguments",
            Self::NotDescendant => "not_descendant",
            Self::DaemonPaths => "daemon_paths",
            Self::DaemonTimeout | Self::Daemon(ClientError::HelloTimeout) => "daemon_timeout",
            Self::Daemon(ClientError::Rpc(err)) => {
                return json!({"kind": "daemon_rpc", "code": err.code});
            }
            Self::Daemon(ClientError::VersionMismatch { .. }) => {
                return json!({"kind": "daemon_version_mismatch", "code": VERSION_MISMATCH});
            }
            Self::Daemon(ClientError::Closed) => "daemon_disconnected",
            Self::Daemon(err) if err.is_not_running() => "daemon_unavailable",
            Self::Daemon(ClientError::Io(_)) => "daemon_io",
            Self::Daemon(_) => "daemon_protocol",
        };
        json!({"kind": kind})
    }
}

pub fn list() -> Value {
    let tools = Tool::ALL.map(|tool| {
        json!({
            "name": tool.name(), "description": tool.description(), "inputSchema": tool.schema()
        })
    });
    json!({"tools": tools})
}

pub async fn call(caller: &AgentId, params: &Value) -> Result<Value, rpc::Error> {
    let invalid_params = || rpc::Error::new(INVALID_PARAMS, "Invalid params");
    let name = params["name"].as_str().ok_or_else(invalid_params)?;
    let tool = Tool::ALL
        .into_iter()
        .find(|tool| tool.name() == name)
        .ok_or(rpc::Error::new(INVALID_PARAMS, "Unknown tool"))?;
    let empty = Value::Object(Map::new());
    let args = params.get("arguments").unwrap_or(&empty);
    if !args.is_object() {
        return Err(invalid_params());
    }
    let result = invoke(caller, tool, args).await;
    let is_error = result.is_err();
    let body = result.unwrap_or_else(ToolError::body);
    Ok(json!({"content": [{"type": "text", "text": body.to_string()}], "isError": is_error}))
}

fn wait_duration(seconds: Option<u64>) -> Duration {
    Duration::from_secs(seconds.unwrap_or(DEFAULT_WAIT_SECS).min(MAX_WAIT_SECS))
}

async fn invoke(caller: &AgentId, tool: Tool, args: &Value) -> Result<Value, ToolError> {
    tool.validate(args)?;
    let paths = commands::resolve_paths().map_err(|_| ToolError::DaemonPaths)?;
    if let Tool::Wait = tool {
        let id = agent_id(args)?;
        return wait(
            &paths,
            caller,
            &id,
            wait_duration(args["timeout_secs"].as_u64()),
        )
        .await;
    }
    timeout(DAEMON_TIMEOUT, async {
        let mut client = Client::connect(&paths).await?;
        if let Tool::List = tool {
            return list_descendants(&mut client, caller).await;
        }
        let caller = get(&mut client, caller).await?;
        if let Tool::Create = tool {
            return create(&mut client, caller, args).await;
        }
        target_tool(&mut client, &caller, tool, args).await
    })
    .await
    .map_err(|_| ToolError::DaemonTimeout)?
}

fn agent_id(args: &Value) -> Result<AgentId, ToolError> {
    serde_json::from_value(args["agent_id"].clone()).map_err(|_| ToolError::InvalidArguments)
}

async fn get(client: &mut Client, id: &AgentId) -> Result<Agent, ToolError> {
    let params = AgentIdParams { id: id.clone() };
    Ok(client.call(AGENT_GET, &params).await?)
}

async fn create(client: &mut Client, caller: Agent, args: &Value) -> Result<Value, ToolError> {
    let prompt = args["prompt"].as_str().ok_or(ToolError::InvalidArguments)?;
    if prompt.trim().is_empty() {
        return Err(ToolError::InvalidArguments);
    }
    let provider = args
        .get("provider")
        .map(|value| serde_json::from_value::<ProviderKind>(value.clone()))
        .transpose()
        .map_err(|_| ToolError::InvalidArguments)?
        .unwrap_or(caller.provider);
    let title = args["title"]
        .as_str()
        .map_or_else(|| default_title(prompt), str::to_owned);
    let params = AgentCreateParams {
        workspace_id: caller.workspace_id,
        title,
        provider: Some(provider),
        profile: None,
        model: args["model"].as_str().map(str::to_owned),
        mode: args["mode"].as_str().map(str::to_owned),
        initial_prompt: prompt.to_owned(),
        labels: Default::default(),
        parent_id: Some(caller.id),
        mcp: args["mcp"].as_bool().unwrap_or(false),
    };
    let agent: Agent = client.call(AGENT_CREATE, &params).await?;
    Ok(json!({
        "agent_id": agent.id, "provider": agent.provider, "mode": agent.mode,
        "auto_approve": agent.auto_approve
    }))
}

fn ancestry_steps(caller: &Agent, agent: &Agent) -> u32 {
    // Bound traversal using the recorded relative depth and the largest
    // daemon configuration value, even if a stored record is malformed.
    agent
        .depth
        .min(MAX_AGENT_DEPTH)
        .saturating_sub(caller.depth)
}

async fn is_descendant(
    client: &mut Client,
    caller: &Agent,
    agent: &Agent,
) -> Result<bool, ToolError> {
    if agent.id == caller.id {
        return Ok(false);
    }
    let mut parent = agent.parent_id.clone();
    let steps = ancestry_steps(caller, agent);
    for step in 0..steps {
        let Some(id) = parent else {
            return Ok(false);
        };
        if id == caller.id {
            return Ok(true);
        }
        if step + 1 < steps {
            parent = get(client, &id).await?.parent_id;
        } else {
            break;
        }
    }
    Ok(false)
}

async fn require_descendant(
    client: &mut Client,
    caller: &Agent,
    id: &AgentId,
) -> Result<Agent, ToolError> {
    let agent = get(client, id).await?;
    if !is_descendant(client, caller, &agent).await? {
        return Err(ToolError::NotDescendant);
    }
    Ok(agent)
}

async fn list_descendants(client: &mut Client, caller_id: &AgentId) -> Result<Value, ToolError> {
    let mut listed: Value = client
        .call(
            AGENT_LIST,
            &AgentListParams {
                include_archived: true,
            },
        )
        .await?;
    let agents: Vec<Agent> = serde_json::from_value(
        listed
            .get_mut("agents")
            .map(Value::take)
            .unwrap_or_default(),
    )
    .map_err(ClientError::from)?;
    let by_id: HashMap<_, _> = agents.iter().map(|agent| (&agent.id, agent)).collect();
    let caller = by_id
        .get(caller_id)
        .ok_or_else(|| ClientError::Rpc(RpcError::new(NOT_FOUND, "Caller not found")))?;
    let descendants: Vec<Value> = agents
        .iter()
        .filter(|agent| {
            let mut parent = agent.parent_id.as_ref();
            for _ in 0..ancestry_steps(caller, agent) {
                let Some(id) = parent else { break };
                if id == caller_id {
                    return true;
                }
                parent = by_id.get(id).and_then(|agent| agent.parent_id.as_ref());
            }
            false
        })
        .map(|agent| {
            json!({
                "agent_id": agent.id, "state": agent.state, "title": agent.title,
                "provider": agent.provider
            })
        })
        .collect();
    Ok(json!({"agents": descendants}))
}

async fn target_tool(
    client: &mut Client,
    caller: &Agent,
    tool: Tool,
    args: &Value,
) -> Result<Value, ToolError> {
    let id = agent_id(args)?;
    let agent = require_descendant(client, caller, &id).await?;
    match tool {
        Tool::Get => snapshot(client, &agent).await,
        Tool::Send => {
            let text = args["text"]
                .as_str()
                .ok_or(ToolError::InvalidArguments)?
                .to_owned();
            let behavior = args
                .get("behavior")
                .map(|value| serde_json::from_value::<SendBehavior>(value.clone()))
                .transpose()
                .map_err(|_| ToolError::InvalidArguments)?
                .unwrap_or_default();
            let _: Value = client
                .call(
                    AGENT_SEND,
                    &AgentSendParams {
                        agent_id: id,
                        text,
                        behavior,
                    },
                )
                .await?;
            Ok(json!({}))
        }
        Tool::Interrupt | Tool::Archive => {
            let method = if matches!(tool, Tool::Interrupt) {
                AGENT_INTERRUPT
            } else {
                AGENT_ARCHIVE
            };
            let _: Value = client.call(method, &AgentIdParams { id }).await?;
            Ok(json!({}))
        }
        _ => Err(ToolError::InvalidArguments),
    }
}

fn settled(state: AgentState) -> bool {
    matches!(
        state,
        AgentState::Idle
            | AgentState::Stopped
            | AgentState::Failed
            | AgentState::Archived
            | AgentState::WaitingPermission
    )
}

async fn subscribe(
    paths: &DaemonPaths,
    id: &AgentId,
) -> Result<mpsc::Receiver<Notification>, ToolError> {
    Ok(Client::connect(paths)
        .await?
        .subscribe(SubscribeParams {
            agent_id: Some(id.clone()),
            since_seq: None,
        })
        .await?)
}

async fn wait(
    paths: &DaemonPaths,
    caller: &AgentId,
    id: &AgentId,
    duration: Duration,
) -> Result<Value, ToolError> {
    let setup_deadline = Instant::now() + DAEMON_TIMEOUT;
    let (mut client, mut notifications, mut agent) = timeout_at(setup_deadline, async {
        let mut client = Client::connect(paths).await?;
        let caller = get(&mut client, caller).await?;
        require_descendant(&mut client, &caller, id).await?;
        // Authorization uses only the parent chain. Subscribe before reading
        // the initial state used for waiting so that no transition is missed.
        let notifications = subscribe(paths, id).await?;
        let agent = get(&mut client, id).await?;
        Ok::<_, ToolError>((client, notifications, agent))
    })
    .await
    .map_err(|_| ToolError::DaemonTimeout)??;

    // Initial and final RPCs share one budget outside the state wait.
    let remaining_margin = setup_deadline.saturating_duration_since(Instant::now());
    let completion = timeout(duration, async {
        while !settled(agent.state) {
            let notice = notifications.recv().await.ok_or(ClientError::Closed)?;
            match notice.method.as_str() {
                NOTIFY_LAGGED => {
                    // Lagged ends the daemon's subscription. Reattach before re-reading.
                    notifications = subscribe(paths, id).await?;
                }
                NOTIFY_EVENT if notice.params["kind"]["type"] == "state_changed" => {}
                _ => continue,
            }
            agent = get(&mut client, id).await?;
        }
        Ok::<_, ToolError>(())
    })
    .await;

    timeout(remaining_margin, async {
        match completion {
            Ok(done) => done?,
            Err(_) => {
                // timeout may have cancelled a daemon request. Use a fresh connection
                // before checking the current state rather than consuming its old reply.
                client = Client::connect(paths).await?;
                agent = get(&mut client, id).await?;
            }
        }
        let mut response = snapshot(&mut client, &agent).await?;
        response["timed_out"] = json!(!settled(agent.state));
        Ok(response)
    })
    .await
    .map_err(|_| ToolError::DaemonTimeout)?
}

async fn snapshot(client: &mut Client, agent: &Agent) -> Result<Value, ToolError> {
    let last_answer = read_last_answer(client, &agent.id).await?;
    Ok(json!({
        "state": agent.state, "title": agent.title, "provider": agent.provider,
        "last_answer": last_answer,
        "pending_permission": agent.state == AgentState::WaitingPermission
    }))
}

async fn read_last_answer(client: &mut Client, id: &AgentId) -> Result<String, ToolError> {
    let mut events: Vec<AgentEvent> = Vec::new();
    let mut before = None;
    for _ in 0..MAX_ANSWER_PAGES {
        let query = AgentEventsQuery {
            page: AgentEventsParams {
                agent_id: id.clone(),
                since_seq: before.unwrap_or(0),
                limit: MAX_EVENTS_LIMIT,
                tail: before.is_none().then_some(MAX_EVENTS_LIMIT),
            },
            reverse: before.is_some(),
        };
        let mut result: Value = client.call(AGENT_EVENTS, &query).await?;
        let mut page: Vec<AgentEvent> = serde_json::from_value(
            result
                .get_mut("events")
                .map(Value::take)
                .unwrap_or_default(),
        )
        .map_err(ClientError::from)?;
        let Some(first) = page.first() else { break };
        if let Some(before) = before
            && page.last().and_then(|event| event.seq.checked_add(1)) != Some(before)
        {
            // Rotation can remove events between these two requests. A gap
            // cannot establish that the first visible turn is complete.
            break;
        }
        before = Some(first.seq);
        page.append(&mut events);
        events = page;
        if let Some(answer) = last_complete_answer(&events) {
            return Ok(answer);
        }
        if before == Some(1) {
            break;
        }
    }
    Ok(String::new())
}

fn last_complete_answer(events: &[AgentEvent]) -> Option<String> {
    let mut turn_end = events.len();
    for (index, event) in events.iter().enumerate().rev() {
        if !matches!(event.kind, EventKind::TurnStarted) {
            continue;
        }
        let turn = &events[index + 1..turn_end];
        turn_end = index;
        let Some(end) = turn
            .iter()
            .position(|event| matches!(event.kind, EventKind::TurnCompleted { .. }))
        else {
            continue;
        };
        let completed = &turn[..end];
        if completed.iter().any(|event| {
            matches!(
                event.kind,
                EventKind::Error { .. } | EventKind::RateLimited { .. }
            )
        }) {
            continue;
        }
        let mut answer = String::new();
        let mut finished_block = false;
        for event in completed {
            if let EventKind::AssistantText { text, final_ } = &event.kind {
                if finished_block {
                    answer.push('\n');
                }
                let end = text.floor_char_boundary(MAX_ANSWER_BYTES - answer.len());
                answer.push_str(&text[..end]);
                if end < text.len() || answer.len() == MAX_ANSWER_BYTES {
                    break;
                }
                finished_block = *final_;
            }
        }
        if !answer.is_empty() {
            return Some(answer);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_duration_clamps_above_300_seconds() {
        assert_eq!(wait_duration(Some(301)), Duration::from_secs(300));
        assert_eq!(wait_duration(Some(u64::MAX)), Duration::from_secs(300));
    }
}
